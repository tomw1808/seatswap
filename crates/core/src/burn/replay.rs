//! The replay (spec §2.4): play the work recorded in a range of the history
//! against the accounts' limits as they stood at the range's start, once for
//! each policy.

use std::collections::BTreeMap;

use chrono::{DateTime, Offset, Utc};

use crate::api::LimitKind;
use crate::history::{
    AccountRef, HistoryDir, HistoryError, Hosted, Hosts, Range, Sample, TokenRecord,
};
use crate::transcripts::hour_start;

use super::plan::{play, AccountState, Inputs, PlanError, Policy, SessionState, Walk, Work};
use super::{mean_over_hours, work_units, Rates, Rhythm};

/// What one policy did with the recorded work of a range.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayResult {
    pub policy: Policy,
    /// The work of the steps that had an account.
    pub worked_wu: f64,
    /// The work of the steps without a usable account.
    pub blocked_wu: f64,
    pub switches: u32,
    /// The week-% unused at every weekly reset the replay passed, summed per
    /// account.
    pub expired: BTreeMap<AccountRef, f64>,
    /// The mean work of the recorded hours that had work: the "typical working
    /// hour" the play's eager rule used. 0.0 when no hour had work.
    pub typical_wu: f64,
    /// The warnings of the sample and token reads, for the caller that shows
    /// them (Task 9's CLI).
    pub warnings: Vec<String>,
}

/// Why a replay cannot be played.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error("account {account}: its first sample at {at} has no {missing}")]
    Unplayable {
        account: String,
        at: DateTime<Utc>,
        missing: &'static str,
    },
}

/// Play the recorded work of `range` (token records, each spread evenly over
/// the four steps of its hour, all hosts together) against the accounts' limits
/// as they stood at `range.from`: each account starts from its first sample in
/// the range (weekly percent and reset, session percent and its reset; a sample
/// without a session limit is an uncapped account). Ids inside the play are
/// `"<acc>/<org>"`, unique per [`AccountRef`]; [`ReplayResult::expired`] names
/// the full [`AccountRef`] again. `rates` is used as given: an account without
/// an entry never fills, as the plan says. `without` leaves accounts out, and
/// the replay starts with no active account, so its first step chooses.
///
/// A sample that cannot be read is the [`ReplayError::History`] error. A sample
/// that cannot be played — no weekly limit, or a weekly limit without a reset
/// time — is [`ReplayError::Unplayable`]; every input is checked with
/// [`Inputs::check`] before the play. The warnings of the reads are kept in
/// [`ReplayResult::warnings`] for the caller that shows them.
pub fn replay(
    history: &HistoryDir,
    range: &Range,
    rates: &BTreeMap<AccountRef, Rates>,
    without: &[AccountRef],
    policy: Policy,
) -> Result<ReplayResult, ReplayError> {
    let sample_read = history.read::<Sample>(range, &Hosts::All)?;
    let token_read = history.read::<TokenRecord>(range, &Hosts::All)?;
    let samples = sample_read.records;
    let tokens = token_read.records;
    let mut warnings = sample_read.warnings;
    warnings.extend(token_read.warnings);

    let mut accounts = Vec::new();
    let mut refs: BTreeMap<String, AccountRef> = BTreeMap::new();
    for (account, sample) in first_samples(&samples, without) {
        let week = sample
            .limit(&LimitKind::WeeklyAll)
            .ok_or_else(|| unplayable(account, sample.t, "weekly limit"))?;
        let week_resets_at = week
            .r
            .ok_or_else(|| unplayable(account, sample.t, "weekly reset"))?;
        let session = sample.limit(&LimitKind::Session).map(|limit| SessionState {
            percent: limit.p,
            resets_at: limit.r,
        });
        let id = id_of(account);
        accounts.push(AccountState {
            id: id.clone(),
            may_use: true,
            login_ok: true,
            week_percent: week.p,
            week_resets_at,
            session,
            model_blocked: false,
            rates: rates.get(account).copied(),
        });
        refs.insert(id, account.clone());
    }

    let hours = recorded_hours(&tokens);
    let walk = Walk::new(Work::Recorded(&hours), range.to);
    let inputs = Inputs {
        now: range.from,
        offset: range.from.offset().fix(),
        accounts,
        active: None,
        // The walk carries the recorded work and its typical hour; a replay
        // never reads the learned rhythm.
        rhythm: Rhythm { wu: [0.0; 168] },
    };
    inputs.check()?;
    let typical_wu = mean_over_hours(hours.values().copied());
    let played = play(&inputs, policy, &walk, typical_wu);
    let expired = played
        .expired
        .into_iter()
        .filter_map(|(id, expired)| refs.get(&id).map(|account| (account.clone(), expired)))
        .collect();
    Ok(ReplayResult {
        policy,
        worked_wu: played.worked_wu,
        blocked_wu: played.plan.blocked_wu,
        switches: played.plan.switches,
        expired,
        typical_wu,
        warnings,
    })
}

/// The id a play uses for `account`: unique per [`AccountRef`], unlike the
/// server's `acc` alone, which two organizations can share.
fn id_of(account: &AccountRef) -> String {
    format!("{}/{}", account.acc, account.org)
}

/// The first sample in the range of every account that is not in `without`.
fn first_samples<'a>(
    samples: &'a [Hosted<Sample>],
    without: &[AccountRef],
) -> Vec<(&'a AccountRef, &'a Sample)> {
    let mut firsts: BTreeMap<&AccountRef, &Sample> = BTreeMap::new();
    for hosted in samples {
        let sample = &hosted.record;
        if without.contains(&sample.account) {
            continue;
        }
        firsts.entry(&sample.account).or_insert(sample);
    }
    firsts.into_iter().collect()
}

/// The work recorded in each UTC hour, all hosts together.
fn recorded_hours(tokens: &[Hosted<TokenRecord>]) -> BTreeMap<DateTime<Utc>, f64> {
    let mut hours: BTreeMap<DateTime<Utc>, f64> = BTreeMap::new();
    for hosted in tokens {
        let record = &hosted.record;
        let units = work_units(record.input, record.out, record.cw, record.cr);
        *hours.entry(hour_start(record.t)).or_insert(0.0) += units;
    }
    hours
}

/// The first sample of `account` cannot be played.
fn unplayable(account: &AccountRef, at: DateTime<Utc>, missing: &'static str) -> ReplayError {
    ReplayError::Unplayable {
        account: id_of(account),
        at,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{Duration, FixedOffset};

    use crate::burn::plan::testkit::{inputs, now, offset, EPS};
    use crate::history::{LimitSample, Source, TokenRecord};
    use crate::project::ProjectKey;

    fn account_ref(id: &str) -> AccountRef {
        AccountRef {
            acc: id.to_string(),
            org: format!("org-{id}"),
        }
    }

    /// One week of token records shaped like Task 5's rhythm: every working hour
    /// is worth the rhythm's 1.0 wu. The range is `now` to `now + 7 d - 1 h`, so
    /// it holds the week's 92 work units before PR's Saturday reset and none of
    /// the next Monday morning.
    fn setup() -> (
        tempfile::TempDir,
        HistoryDir,
        BTreeMap<AccountRef, Rates>,
        Range,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let inputs = inputs();
        let range = Range {
            from: now(),
            to: now() + Duration::days(7) - Duration::hours(1),
        };

        for state in &inputs.accounts {
            let mut limits = vec![LimitSample {
                k: LimitKind::WeeklyAll,
                m: None,
                p: 0.0,
                r: Some(state.week_resets_at),
                b: false,
            }];
            if state.session.is_some() {
                limits.push(LimitSample {
                    k: LimitKind::Session,
                    m: None,
                    p: 0.0,
                    r: None,
                    b: false,
                });
            }
            history
                .append(&Sample {
                    t: range.from,
                    account: account_ref(&state.id),
                    src: Source::Poll,
                    l: limits,
                })
                .expect("write the sample");
        }

        history
            .append_many(&tokens(&inputs.rhythm, &range, offset()))
            .expect("write the token records");

        let rates = inputs
            .accounts
            .iter()
            .map(|state| {
                (
                    account_ref(&state.id),
                    state.rates.expect("the setup's rates"),
                )
            })
            .collect();
        (dir, history, rates, range)
    }

    /// Token records of one hour each, worth that hour's rhythm work.
    fn tokens(rhythm: &Rhythm, range: &Range, offset: FixedOffset) -> Vec<TokenRecord> {
        let mut records = Vec::new();
        let mut at = range.from;
        while at < range.to {
            let work = rhythm.at(at, offset);
            if work > 0.0 {
                records.push(TokenRecord {
                    t: at,
                    session: "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34".to_string(),
                    cwd: "/home/sam/projects/atlas".to_string(),
                    project: ProjectKey::Repo("github.com/sam/atlas".to_string()),
                    model: "claude-fable-5-2".to_string(),
                    input: (work * 1_000_000.0).round() as u64,
                    out: 0,
                    cw: 0,
                    cr: 0,
                    answers: 1,
                    agent_answers: 0,
                });
            }
            at += Duration::hours(1);
        }
        records
    }

    #[test]
    fn replay_lazy_and_eager_differ_as_in_the_model() {
        let (_dir, history, rates, range) = setup();
        let lazy = replay(&history, &range, &rates, &[], Policy::Lazy).expect("replay");
        let eager = replay(&history, &range, &rates, &[], Policy::Eager).expect("replay");

        assert!((eager.blocked_wu - 0.0).abs() < EPS, "{}", eager.blocked_wu);
        assert!(lazy.blocked_wu > 15.0, "{}", lazy.blocked_wu);
        for result in [&lazy, &eager] {
            let worked = result.worked_wu + result.blocked_wu;
            assert!((worked - 92.0).abs() < EPS, "{worked}");
        }
        assert_eq!(lazy.policy, Policy::Lazy);
        assert_eq!(eager.policy, Policy::Eager);
    }

    #[test]
    fn replay_without_an_account_blocks_more() {
        let (_dir, history, rates, range) = setup();
        let with = replay(&history, &range, &rates, &[], Policy::Lazy).expect("replay");
        let without =
            replay(&history, &range, &rates, &[account_ref("AC")], Policy::Lazy).expect("replay");

        assert!(
            without.blocked_wu > with.blocked_wu,
            "{} vs {}",
            without.blocked_wu,
            with.blocked_wu
        );
    }

    // ---- proof tests of the review ----

    /// A history with one account whose first sample cannot be played.
    fn one_account(sample: Sample) -> (tempfile::TempDir, HistoryDir, Range) {
        let dir = tempfile::tempdir().expect("temp dir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let range = Range {
            from: now(),
            to: now() + Duration::hours(1),
        };
        history.append(&sample).expect("write the sample");
        (dir, history, range)
    }

    #[test]
    fn a_sample_the_plan_cannot_play_is_an_error() {
        // A 5 h window at 60 % without a reset time cannot be played: the
        // plan's own check rejects such an input, so the replay must not plan
        // with it (it would take the account for empty).
        let (_dir, history, range) = one_account(Sample {
            t: now(),
            account: account_ref("AC"),
            src: Source::Poll,
            l: vec![
                LimitSample {
                    k: LimitKind::WeeklyAll,
                    m: None,
                    p: 0.0,
                    r: Some(now() + Duration::days(5)),
                    b: false,
                },
                LimitSample {
                    k: LimitKind::Session,
                    m: None,
                    p: 60.0,
                    r: None,
                    b: false,
                },
            ],
        });
        let account = account_ref("AC");
        let rates = BTreeMap::from([(
            account,
            Rates {
                c: 4.0,
                s: Some(100.0),
            },
        )]);

        let err = replay(&history, &range, &rates, &[], Policy::Lazy)
            .expect_err("a window without a reset time cannot be played");
        assert!(err.to_string().contains("AC"), "{err}");
    }

    #[test]
    fn the_error_of_a_sample_that_cannot_be_played_is_not_an_io_error() {
        // The account's first sample has no weekly limit: no file was read or
        // written wrongly, so the error must not be an I/O error about a path
        // that does not exist.
        let (_dir, history, range) = one_account(Sample {
            t: now(),
            account: account_ref("AC"),
            src: Source::Poll,
            l: vec![LimitSample {
                k: LimitKind::Session,
                m: None,
                p: 10.0,
                r: Some(now() + Duration::hours(2)),
                b: false,
            }],
        });

        let err = replay(&history, &range, &BTreeMap::new(), &[], Policy::Lazy)
            .expect_err("a sample without a weekly limit cannot be played");
        assert!(
            !matches!(err, ReplayError::History(HistoryError::Io { .. })),
            "{err}"
        );
    }

    #[test]
    fn two_accounts_that_share_an_acc_get_two_ids() {
        // Two organizations can use the same `acc`; the play's ids must tell
        // them apart, so both accounts keep their own expired percent.
        let dir = tempfile::tempdir().expect("temp dir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let range = Range {
            from: now(),
            to: now() + Duration::days(7),
        };
        let first = AccountRef {
            acc: "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04".to_string(),
            org: "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20".to_string(),
        };
        let second = AccountRef {
            acc: first.acc.clone(),
            org: "c41e6b82-7d3a-4f95-b2c8-5a1d9e3f7b46".to_string(),
        };
        for account in [&first, &second] {
            history
                .append(&Sample {
                    t: range.from,
                    account: account.clone(),
                    src: Source::Poll,
                    l: vec![LimitSample {
                        k: LimitKind::WeeklyAll,
                        m: None,
                        p: 0.0,
                        r: Some(range.from + Duration::days(1)),
                        b: false,
                    }],
                })
                .expect("write the sample");
        }
        let rates = BTreeMap::from([
            (first.clone(), Rates { c: 1.0, s: None }),
            (second.clone(), Rates { c: 1.0, s: None }),
        ]);

        let result = replay(&history, &range, &rates, &[], Policy::Lazy)
            .expect("two accounts with one acc do not collapse");
        assert_eq!(result.expired.len(), 2);
        assert!((result.expired[&first] - 100.0).abs() < EPS);
        assert!((result.expired[&second] - 100.0).abs() < EPS);
    }

    #[test]
    fn the_replay_keeps_the_read_warnings() {
        use std::fs::OpenOptions;
        use std::io::Write;

        let (dir, history, rates, range) = setup();
        let path = dir.path().join("atlas/samples-2026-11.jsonl");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open the samples file");
        file.write_all(b"{\"t\":\"2026-11-14T1")
            .expect("append a torn line");
        drop(file);

        let result = replay(&history, &range, &rates, &[], Policy::Lazy).expect("replay");
        assert_eq!(result.warnings.len(), 1);
        assert!(
            result.warnings[0].contains("samples-2026-11.jsonl"),
            "{:?}",
            result.warnings
        );
    }
}
