//! The reading side of auto-burn (spec §2.3, §2.5, §2.12): one pass over the
//! history and one translation of the tray app's views into the plan's inputs.
//! The CLI and the tray app both call these, so neither builds a `Situation`
//! by hand.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, FixedOffset, Utc};

use crate::api::Limit;
use crate::engine::{AccountView, Blocker};
use crate::history::{
    AccountRef, By, Event, EventKind, HistoryDir, HistoryError, Hosted, Hosts, Range, Sample,
    TokenRecord,
};
use crate::store::{Account, LoginState};

use super::cost::switch_cost;
use super::decide::{decide, regime, Decision, Regime, Situation};
use super::plan::{blocked, follow, AccountState, Inputs, Plan, PlanError, Policy, SessionState};
use super::rates::{rates, rhythm};
use super::{Rates, Rhythm, MANUAL_HOLD_H, RATES_DAYS, RHYTHM_WEEKS};

/// Everything a decision is built from, read once: the rhythm, the accounts'
/// rates, the measured switch cost, the read warnings and the events of the last
/// [`RATES_DAYS`] days.
#[derive(Debug, Clone)]
pub struct Learned {
    pub rhythm: Rhythm,
    pub rates: BTreeMap<AccountRef, Rates>,
    pub cost: Option<super::decide::SwitchCost>,
    pub warnings: Vec<String>,
    /// The events of the last [`RATES_DAYS`] days, every host, sorted by time.
    pub events: Vec<Hosted<Event>>,
    /// This history folder's host. Only its switch events hold auto-burn back.
    pub host: String,
    /// `readSessions` is off: no rates were learned from the sessions
    /// (spec §2.3). The regime says so with [`super::decide::RegimeWhy::SessionsOff`].
    pub sessions_off: bool,
}

/// What [`situation`] found: a checked `Situation`, or the accounts that have a
/// login but no reading yet.
#[allow(clippy::large_enum_variant)]
pub enum Readiness {
    Ready(Situation),
    /// Accounts with a login and no weekly reading. The caller treats this
    /// exactly as [`super::decide::Decision::Stale`]: poll them, do not switch.
    Unread {
        accounts: Vec<String>,
    },
    /// No account at all can go into the inputs (every account is logged out).
    /// The caller decides nothing and shows no view.
    NoAccounts,
}

/// Read the history once: samples and tokens of [`RHYTHM_WEEKS`] weeks, events
/// of [`RATES_DAYS`] days. The rates are computed for `accounts` only; an
/// account with too little work has no entry. With `read_sessions` false no
/// rates are learned: every account plays as one without rates (spec §2.3).
pub fn learn(
    history: &HistoryDir,
    now: DateTime<Utc>,
    offset: FixedOffset,
    accounts: &[Account],
    read_sessions: bool,
) -> Result<Learned, HistoryError> {
    let weeks = Range {
        from: now - Duration::weeks(RHYTHM_WEEKS),
        to: now,
    };
    let days = Range {
        from: now - Duration::days(RATES_DAYS),
        to: now,
    };
    let samples = history.read::<Sample>(&weeks, &Hosts::All)?;
    let tokens = history.read::<TokenRecord>(&weeks, &Hosts::All)?;
    let events = history.read::<Event>(&days, &Hosts::All)?;

    let mut warnings = Vec::new();
    warnings.extend(samples.warnings);
    warnings.extend(tokens.warnings);
    warnings.extend(events.warnings);

    let rhythm = rhythm(&tokens.records, now, offset);
    let mut learned_rates = BTreeMap::new();
    if read_sessions {
        for account in accounts {
            let account_ref = AccountRef::of(account);
            if let Some(account_rates) = rates(
                &account_ref,
                &samples.records,
                &tokens.records,
                &events.records,
                now,
            ) {
                learned_rates.insert(account_ref, account_rates);
            }
        }
    }
    let cost = switch_cost(
        &events.records,
        &samples.records,
        &tokens.records,
        &learned_rates,
        &days,
    );

    Ok(Learned {
        rhythm,
        rates: learned_rates,
        cost,
        warnings,
        events: events.records,
        host: history.host().to_string(),
        sessions_off: !read_sessions,
    })
}

/// Turn the views into [`Inputs`], check them, and derive the two hold times
/// from the learned events (spec §2.5, §2.12).
///
/// A logged-out account is in the inputs when it has a reading, with
/// `login_ok` false: the plan treats it as blocked. An account with a login but
/// no reading makes the whole situation [`Readiness::Unread`]; a logged-out
/// account without a reading is left out. When no account at all can go into
/// the inputs, the situation is [`Readiness::NoAccounts`].
#[allow(clippy::too_many_arguments)]
pub fn situation(
    learned: &Learned,
    views: &[AccountView],
    accounts: &[Account],
    active: Option<&str>,
    now: DateTime<Utc>,
    offset: FixedOffset,
    tied_busy: u32,
) -> Result<Readiness, PlanError> {
    let mut unread = Vec::new();
    let mut states = Vec::new();
    let mut oldest_reading: Option<DateTime<Utc>> = None;

    for account in accounts {
        let view = views.iter().find(|view| view.id == account.id);
        let logged_out = view.is_some_and(|view| view.login_state == LoginState::LoggedOut);
        match view.and_then(reading_of) {
            Some((week, week_resets_at, session, fetched_at)) => {
                let login_ok = view.is_some_and(|view| view.login_state == LoginState::Ok);
                states.push(AccountState {
                    id: account.id.clone(),
                    may_use: account.auto_burn,
                    login_ok,
                    week_percent: week.percent,
                    week_resets_at,
                    session,
                    model_blocked: view.is_some_and(model_blocked),
                    rates: learned.rates.get(&AccountRef::of(account)).copied(),
                });
                if login_ok && (account.auto_burn || active == Some(account.id.as_str())) {
                    oldest_reading = Some(match oldest_reading {
                        Some(oldest) => oldest.min(fetched_at),
                        None => fetched_at,
                    });
                }
            }
            None if !logged_out && (account.auto_burn || active == Some(account.id.as_str())) => {
                unread.push(account.id.clone())
            }
            None => {}
        }
    }

    if !unread.is_empty() {
        return Ok(Readiness::Unread { accounts: unread });
    }
    let Some(oldest_reading) = oldest_reading else {
        // No account with a login has a reading: there is nothing to decide
        // from (every account is logged out, or there are none).
        return Ok(Readiness::NoAccounts);
    };

    let last_switch_at = last_switch_at(learned, now);
    let manual_hold_until = manual_hold_until(learned, accounts, &states, now);

    let situation = Situation {
        inputs: Inputs {
            now,
            offset,
            accounts: states,
            active: active.map(str::to_string),
            rhythm: learned.rhythm.clone(),
        },
        cost: learned.cost,
        last_switch_at,
        manual_hold_until,
        tied_busy,
        oldest_reading,
        sessions_off: learned.sessions_off,
    };
    situation.check()?;
    Ok(Readiness::Ready(situation))
}

/// The account auto-burn would switch to at this reading: the target of a
/// [`Decision::Switch`] or [`Decision::Wait`], otherwise the first account of
/// the lazy plan other than the active one, otherwise `None`. It is the one
/// rule `BurnView` and `status --json`'s `next` use.
pub fn next_account(situation: &Situation, decision: &Decision, plan: &Plan) -> Option<String> {
    match decision {
        Decision::Switch { to, .. } | Decision::Wait { to, .. } => Some(to.clone()),
        Decision::Stay | Decision::Stale | Decision::AllFull { .. } => {
            plan.segments.iter().find_map(|segment| {
                let id = segment.account.as_deref()?;
                if Some(id) == situation.inputs.active.as_deref() {
                    None
                } else {
                    Some(id.to_string())
                }
            })
        }
    }
}

/// The whole shared assembly after [`situation`]: the regime, the decision, the
/// lazy plan and the account the plan names next. The tray app and both CLI
/// commands use it, so they cannot take the steps in a different order.
pub struct Assessment {
    pub situation: Situation,
    pub regime: Regime,
    pub decision: Decision,
    pub plan: Plan,
    pub next: Option<String>,
}

/// Assemble one decision from a checked situation: regime → decide →
/// follow(Lazy) → next. `cached_regime` is a regime an earlier call computed;
/// `None` computes a new one. Keeping that cache is the tray app's own job.
pub fn assess(situation: Situation, cached_regime: Option<&Regime>) -> Assessment {
    let regime = match cached_regime {
        Some(regime) => regime.clone(),
        None => regime(&situation),
    };
    let decision = decide(&situation, &regime);
    let plan = follow(&situation.inputs, Policy::Lazy);
    let next = next_account(&situation, &decision, &plan);
    Assessment {
        situation,
        regime,
        decision,
        plan,
        next,
    }
}

/// The weekly limit and the session state of a view, plus when the reading was
/// fetched. `None` when there is no usage, no weekly limit with a reset, or no
/// fetch time: a value auto-burn cannot plan from.
type Reading<'a> = (
    &'a Limit,
    DateTime<Utc>,
    Option<SessionState>,
    DateTime<Utc>,
);

fn reading_of(view: &AccountView) -> Option<Reading<'_>> {
    let fetched_at = view.fetched_at?;
    let usage = view.usage.as_ref()?;
    let week = usage.weekly_all()?;
    let week_resets_at = week.resets_at?;
    let session = usage.session().map(|session| SessionState {
        percent: session.percent,
        resets_at: session.resets_at,
    });
    Some((week, week_resets_at, session, fetched_at))
}

fn model_blocked(view: &AccountView) -> bool {
    matches!(&view.blocker, Some(Blocker::Model(_)))
}

/// The last `Switch` event of this host, of any `by`.
fn last_switch_at(learned: &Learned, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    learned
        .events
        .iter()
        .filter(|hosted| hosted.host == learned.host && hosted.record.t <= now)
        .filter(|hosted| matches!(hosted.record.kind, EventKind::Switch { .. }))
        .map(|hosted| hosted.record.t)
        .max()
}

/// The latest of a manual switch's [`MANUAL_HOLD_H`] hold and an `AutoBurn`
/// pause, less the manual hold that ended because its account was blocked.
/// Any switch auto-burn did not make starts the hold (`by` other than `Auto`).
/// A hold ends at the first time its account is blocked after the switch — the
/// history's [`EventKind::Blocked`] of that account, or its state now when that
/// is already past — and stays ended. A resume (`paused_until: None`) ends a
/// manual hold that came before it. An `AutoBurnSet` (a settings toggle) is not
/// a pause or a resume and changes neither.
fn manual_hold_until(
    learned: &Learned,
    accounts: &[Account],
    states: &[AccountState],
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let mut manual: Option<(DateTime<Utc>, AccountRef)> = None;
    let mut paused: Option<DateTime<Utc>> = None;
    for hosted in &learned.events {
        if hosted.host != learned.host || hosted.record.t > now {
            continue;
        }
        match &hosted.record.kind {
            EventKind::Switch { to, by, .. } if *by != By::Auto => {
                manual = Some((hosted.record.t, to.clone()));
            }
            EventKind::AutoBurn { paused_until, .. } => {
                paused = *paused_until;
                if paused_until.is_none() {
                    manual = None;
                }
            }
            _ => {}
        }
    }

    let manual_until = manual.and_then(|(at, to)| {
        let until = at + Duration::hours(MANUAL_HOLD_H);
        if until <= now {
            return None;
        }
        let id = accounts
            .iter()
            .find(|account| AccountRef::of(account) == to)
            .map(|account| account.id.as_str());
        let blocked_event = learned
            .events
            .iter()
            .filter(|hosted| hosted.host == learned.host && hosted.record.t > at)
            .filter(|hosted| hosted.record.t <= now)
            .filter_map(|hosted| match &hosted.record.kind {
                EventKind::Blocked { account, .. } if *account == to => Some(hosted.record.t),
                _ => None,
            })
            .min();
        let blocked_now = id
            .and_then(|id| states.iter().find(|state| state.id == id))
            .is_some_and(|state| blocked(state, now))
            .then_some(now);
        let ended = match (blocked_event, blocked_now) {
            (Some(event), Some(state)) => Some(event.min(state)),
            (Some(event), None) => Some(event),
            (None, Some(state)) => Some(state),
            (None, None) => None,
        };
        let until = ended.map_or(until, |ended| until.min(ended));
        (until > now).then_some(until)
    });
    let paused_until = paused.filter(|until| *until > now);
    manual_until.max(paused_until)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::FixedOffset;

    use crate::api::{Limit, LimitKind, Severity, Usage};
    use crate::burn::decide::Reason;
    use crate::burn::plan::{follow, Policy};
    use crate::history::{Recorder, Source, TokenRecord};
    use crate::project::ProjectKey;
    use crate::types::OAuthAccount;

    const ORG: &str = "e92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20";
    const PR_ACC: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const AC_ACC: &str = "8e1d5b7a-6c3f-4a2b-9d8e-1f2a3b4c5d6e";
    const RESET: &str = "2026-11-16T02:00:00Z";

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a test time")
            .with_timezone(&Utc)
    }

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(0).expect("UTC")
    }

    fn account(id: &str, abbr: &str, acc: &str, auto_burn: bool) -> Account {
        Account {
            id: id.to_string(),
            account_uuid: acc.to_string(),
            organization_uuid: ORG.to_string(),
            label: abbr.to_string(),
            abbr: abbr.to_string(),
            color: "#3b82f6".to_string(),
            email: None,
            org_name: None,
            plan: Some("Team".to_string()),
            oauth_account: OAuthAccount(serde_json::Map::new()),
            added_at: utc("2026-11-01T00:00:00Z"),
            last_used_at: None,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            auto_burn,
            price: None,
            price_currency: None,
        }
    }

    fn limit(kind: LimitKind, percent: f64, resets_at: Option<DateTime<Utc>>) -> Limit {
        Limit {
            kind,
            percent,
            severity: Severity::Normal,
            resets_at,
            model: None,
            is_active: false,
        }
    }

    fn usage(week: f64, session: Option<f64>, reset: DateTime<Utc>) -> Usage {
        let mut limits = vec![limit(LimitKind::WeeklyAll, week, Some(reset))];
        if let Some(session) = session {
            limits.push(limit(LimitKind::Session, session, Some(utc(RESET))));
        }
        Usage {
            limits,
            extra_usage_enabled: false,
        }
    }

    fn view(
        id: &str,
        abbr: &str,
        week: f64,
        session: Option<f64>,
        fetched_at: DateTime<Utc>,
    ) -> AccountView {
        AccountView {
            id: id.to_string(),
            label: abbr.to_string(),
            abbr: abbr.to_string(),
            color: "#3b82f6".to_string(),
            email: None,
            plan: None,
            auto_burn: true,
            is_active: false,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            login_days_left: None,
            usage: Some(usage(week, session, utc(RESET))),
            usable: true,
            blocked_until: None,
            blocker: None,
            fetched_at: Some(fetched_at),
            last_error: None,
        }
    }

    fn learned(host: &str, events: Vec<Hosted<Event>>) -> Learned {
        Learned {
            rhythm: Rhythm { wu: [0.0; 168] },
            rates: BTreeMap::new(),
            cost: None,
            warnings: Vec::new(),
            events,
            host: host.to_string(),
            sessions_off: false,
        }
    }

    fn switch_event(host: &str, at: DateTime<Utc>, to: AccountRef, by: By) -> Hosted<Event> {
        Hosted {
            host: host.to_string(),
            record: Event {
                t: at,
                kind: EventKind::Switch {
                    from: None,
                    to,
                    by,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: None,
                },
            },
        }
    }

    fn blocked_event(host: &str, at: DateTime<Utc>, account: AccountRef) -> Hosted<Event> {
        Hosted {
            host: host.to_string(),
            record: Event {
                t: at,
                kind: EventKind::Blocked {
                    account,
                    k: LimitKind::WeeklyAll,
                    m: None,
                },
            },
        }
    }

    fn ready(
        learned: &Learned,
        views: &[AccountView],
        accounts: &[Account],
        active: Option<&str>,
        now: DateTime<Utc>,
    ) -> Situation {
        match situation(learned, views, accounts, active, now, offset(), 0)
            .expect("the views are valid")
        {
            Readiness::Ready(situation) => situation,
            Readiness::Unread { accounts } => panic!("unread accounts: {accounts:?}"),
            Readiness::NoAccounts => panic!("no account can go into the inputs"),
        }
    }

    #[test]
    fn a_manual_hold_ends_when_its_account_is_blocked() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        // AC was switched to by hand an hour ago and is now at 100 % of its week.
        let event = switch_event(
            "atlas",
            now - Duration::hours(1),
            AccountRef::of(&ac),
            By::User,
        );
        let learned = learned("atlas", vec![event]);
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            view("ac", "AC", 100.0, Some(10.0), now - Duration::minutes(1)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(
            situation.manual_hold_until, None,
            "the blocked account ends the hold early"
        );
        assert_eq!(situation.last_switch_at, Some(now - Duration::hours(1)));
    }

    #[test]
    fn a_hold_ended_by_a_full_account_does_not_come_back() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        // AC was switched to by hand two hours ago, was blocked an hour ago,
        // and its window has reset since: the old rule would start the hold
        // again because AC is not blocked at this reading.
        let switch = switch_event(
            "atlas",
            now - Duration::hours(2),
            AccountRef::of(&ac),
            By::User,
        );
        let blocked = blocked_event("atlas", now - Duration::hours(1), AccountRef::of(&ac));
        let learned = learned("atlas", vec![switch, blocked]);
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, Some(10.0), now - Duration::minutes(1)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(
            situation.manual_hold_until, None,
            "the hold ended at the blocked event and stays ended"
        );
    }

    #[test]
    fn an_external_switch_starts_the_hold() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let switch = switch_event(
            "atlas",
            now - Duration::hours(1),
            AccountRef::of(&ac),
            By::External,
        );
        let learned = learned("atlas", vec![switch]);
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, Some(10.0), now - Duration::minutes(1)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(
            situation.manual_hold_until,
            Some(now + Duration::hours(3)),
            "a /login outside seatswap holds the early rule back"
        );
    }

    #[test]
    fn without_reading_sessions_no_rates_are_learned() {
        let now = utc("2026-11-14T18:00:00Z");
        let ac = account("ac", "AC", AC_ACC, true);
        let tmp = tempfile::tempdir().expect("a temp folder");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let mut recorder = Recorder::new(HistoryDir::at(history.root(), history.host()));
        let account_ref = AccountRef::of(&ac);
        recorder
            .event(
                utc("2026-11-14T08:00:00Z"),
                EventKind::Live {
                    account: Some(account_ref.clone()),
                },
            )
            .expect("the live event");
        recorder
            .reading(
                &account_ref,
                None,
                &usage(40.0, None, utc("2026-11-16T02:00:00Z")),
                utc("2026-11-14T09:00:00Z"),
                Source::Poll,
            )
            .expect("the first reading");
        recorder
            .reading(
                &account_ref,
                None,
                &usage(48.0, None, utc("2026-11-16T02:00:00Z")),
                utc("2026-11-14T13:00:00Z"),
                Source::Poll,
            )
            .expect("the second reading");
        recorder
            .tokens(&[TokenRecord {
                t: utc("2026-11-14T10:00:00Z"),
                session: "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d".to_string(),
                cwd: "/home/sam/projects/seatswap".to_string(),
                project: ProjectKey::Folder("seatswap".to_string()),
                model: "claude-fable-5-2".to_string(),
                input: 25_000_000,
                out: 0,
                cw: 0,
                cr: 0,
                answers: 1,
                agent_answers: 0,
            }])
            .expect("the tokens");

        let with_sessions = learn(&history, now, offset(), std::slice::from_ref(&ac), true)
            .expect("the history reads");
        assert!(
            with_sessions.rates.contains_key(&account_ref),
            "the readings are enough rates when sessions are read"
        );
        let without_sessions = learn(&history, now, offset(), std::slice::from_ref(&ac), false)
            .expect("the history reads");
        assert!(
            without_sessions.rates.is_empty(),
            "readSessions off learns no rates"
        );
    }

    #[test]
    fn an_account_without_a_reading_makes_the_situation_stale() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let learned = learned("atlas", Vec::new());
        let mut no_reading = view("ac", "AC", 10.0, None, now - Duration::minutes(1));
        no_reading.usage = None;
        no_reading.fetched_at = None;
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            no_reading,
        ];
        let readiness = situation(&learned, &views, &[pr, ac], Some("pr"), now, offset(), 0)
            .expect("the accounts are valid");
        match readiness {
            Readiness::Unread { accounts } => assert_eq!(accounts, vec!["ac".to_string()]),
            Readiness::Ready(_) => panic!("AC has no reading"),
            Readiness::NoAccounts => panic!("PR still goes into the inputs"),
        }
    }

    #[test]
    fn a_manual_hold_lasts_four_hours() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let switch = switch_event(
            "atlas",
            now - Duration::hours(1),
            AccountRef::of(&ac),
            By::Cli,
        );
        let learned = learned("atlas", vec![switch]);
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, Some(10.0), now - Duration::minutes(1)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(situation.manual_hold_until, Some(now + Duration::hours(3)));
    }

    #[test]
    fn an_unread_account_auto_burn_may_not_use_is_left_out() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, false);
        let learned = learned("atlas", Vec::new());
        let mut no_reading = view("ac", "AC", 10.0, None, now - Duration::minutes(1));
        no_reading.usage = None;
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            no_reading,
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("pr"), now);
        assert_eq!(situation.inputs.accounts.len(), 1);
        assert_eq!(situation.inputs.accounts[0].id, "pr");
    }

    #[test]
    fn the_plan_starts_on_the_active_account() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let learned = learned("atlas", Vec::new());
        let views = vec![view("pr", "PR", 10.0, None, now - Duration::minutes(1))];
        let situation = ready(&learned, &views, &[pr], Some("pr"), now);
        let plan = follow(&situation.inputs, Policy::Lazy);
        assert_eq!(plan.segments[0].account.as_deref(), Some("pr"));
    }

    #[test]
    fn a_logged_out_account_does_not_make_the_situation_stale() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let mut logged_out = view("pr", "PR", 20.0, None, now - Duration::hours(3));
        logged_out.login_state = LoginState::LoggedOut;
        let fresh = view("ac", "AC", 10.0, None, now - Duration::minutes(2));
        let learned = learned("atlas", Vec::new());
        let views = vec![logged_out, fresh];
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(
            situation.oldest_reading,
            now - Duration::minutes(2),
            "the logged-out account's old reading is not counted"
        );
        let pr_state = situation
            .inputs
            .accounts
            .iter()
            .find(|state| state.id == "pr")
            .expect("the logged-out account with a reading is in the inputs");
        assert!(!pr_state.login_ok);
    }

    #[test]
    fn the_oldest_reading_counts_the_usable_and_the_active_accounts() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        // Auto-burn may not use AC and it is not active: its old reading does
        // not make every decision stale.
        let ac = account("ac", "AC", AC_ACC, false);
        let learned = learned("atlas", Vec::new());
        let views = vec![
            view("pr", "PR", 20.0, None, now - Duration::minutes(5)),
            view("ac", "AC", 10.0, None, now - Duration::minutes(30)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("pr"), now);
        assert_eq!(situation.oldest_reading, now - Duration::minutes(5));
    }

    #[test]
    fn no_account_is_no_accounts_not_unread() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let mut no_pr = view("pr", "PR", 10.0, None, now - Duration::minutes(1));
        no_pr.login_state = LoginState::LoggedOut;
        no_pr.usage = None;
        no_pr.fetched_at = None;
        let mut no_ac = view("ac", "AC", 10.0, None, now - Duration::minutes(1));
        no_ac.login_state = LoginState::LoggedOut;
        no_ac.usage = None;
        no_ac.fetched_at = None;
        let learned = learned("atlas", Vec::new());
        let readiness = situation(&learned, &[no_pr, no_ac], &[pr, ac], None, now, offset(), 0)
            .expect("the accounts are valid");
        match readiness {
            Readiness::NoAccounts => {}
            Readiness::Ready(_) => panic!("no account can go into the inputs"),
            Readiness::Unread { accounts } => {
                panic!("logged-out accounts are not unread: {accounts:?}")
            }
        }
    }

    #[test]
    fn resume_ends_a_manual_hold() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let switch = switch_event(
            "atlas",
            now - Duration::hours(1),
            AccountRef::of(&ac),
            By::User,
        );
        let resume = Hosted {
            host: "atlas".to_string(),
            record: Event {
                t: now - Duration::minutes(30),
                kind: EventKind::AutoBurn {
                    on: true,
                    paused_until: None,
                },
            },
        };
        let learned = learned("atlas", vec![switch, resume]);
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, None, now - Duration::minutes(1)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(situation.manual_hold_until, None);
    }

    #[test]
    fn a_toggle_does_not_end_a_manual_hold_or_a_pause() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, None, now - Duration::minutes(1)),
        ];
        let switch = switch_event(
            "atlas",
            now - Duration::hours(1),
            AccountRef::of(&ac),
            By::User,
        );
        let toggle = Hosted {
            host: "atlas".to_string(),
            record: Event {
                t: now - Duration::minutes(30),
                kind: EventKind::AutoBurnSet { on: true },
            },
        };
        let first_learned = learned("atlas", vec![switch, toggle]);
        let situation = ready(
            &first_learned,
            &views,
            &[pr.clone(), ac.clone()],
            Some("ac"),
            now,
        );
        assert_eq!(
            situation.manual_hold_until,
            Some(now + Duration::hours(3)),
            "a settings toggle is not a resume of the manual switch's hold"
        );

        let pause = Hosted {
            host: "atlas".to_string(),
            record: Event {
                t: now - Duration::minutes(45),
                kind: EventKind::AutoBurn {
                    on: true,
                    paused_until: Some(now + Duration::hours(2)),
                },
            },
        };
        let toggle = Hosted {
            host: "atlas".to_string(),
            record: Event {
                t: now - Duration::minutes(30),
                kind: EventKind::AutoBurnSet { on: false },
            },
        };
        let learned = learned("atlas", vec![pause, toggle]);
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(
            situation.manual_hold_until,
            Some(now + Duration::hours(2)),
            "a settings toggle is not a resume of the explicit pause"
        );
    }

    #[test]
    fn next_account_is_the_decisions_target_then_the_plans() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let mut learned = learned("atlas", Vec::new());
        // Every hour has work, so PR at 99.5 % fills in the plan's first step
        // and the lazy plan moves to AC.
        learned.rhythm = Rhythm { wu: [1.0; 168] };
        learned.rates = BTreeMap::from([
            (AccountRef::of(&pr), Rates { c: 2.0, s: None }),
            (
                AccountRef::of(&ac),
                Rates {
                    c: 4.0,
                    s: Some(100.0),
                },
            ),
        ]);
        let views = vec![
            view("pr", "PR", 99.5, None, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, Some(10.0), now - Duration::minutes(1)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("pr"), now);
        let plan = follow(&situation.inputs, Policy::Lazy);
        assert_eq!(plan.segments[1].account.as_deref(), Some("ac"));

        let switch = Decision::Switch {
            to: "lb".to_string(),
            reason: Reason::WindowOpen,
        };
        assert_eq!(
            next_account(&situation, &switch, &plan).as_deref(),
            Some("lb")
        );
        let wait = Decision::Wait {
            to: "lb".to_string(),
            latest: now + Duration::hours(1),
            tied_busy: 1,
            reason: Reason::WindowOpen,
        };
        assert_eq!(
            next_account(&situation, &wait, &plan).as_deref(),
            Some("lb")
        );
        assert_eq!(
            next_account(&situation, &Decision::Stay, &plan).as_deref(),
            Some("ac"),
            "without a target, the first other account of the lazy plan is next"
        );
        assert_eq!(
            next_account(&situation, &Decision::AllFull { next: None }, &plan).as_deref(),
            Some("ac")
        );
    }

    #[test]
    fn paused_until_is_the_situations_hold() {
        let now = utc("2026-11-10T12:00:00Z");
        let pr = account("pr", "PR", PR_ACC, true);
        let ac = account("ac", "AC", AC_ACC, true);
        let switch = switch_event(
            "atlas",
            now - Duration::hours(1),
            AccountRef::of(&ac),
            By::Cli,
        );
        let learned = learned("atlas", vec![switch]);
        let views = vec![
            view("pr", "PR", 10.0, None, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, Some(10.0), now - Duration::minutes(1)),
        ];
        let situation = ready(&learned, &views, &[pr, ac], Some("ac"), now);
        assert_eq!(
            situation.manual_hold_until,
            Some(now + Duration::hours(3)),
            "a CLI switch one hour ago holds for four hours"
        );
    }
}
