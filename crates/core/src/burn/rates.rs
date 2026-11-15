//! The learning side of auto-burn (spec §2.3): the weekly rhythm of work, which
//! account was live when, and the rates an account's limits cost per work unit.

use std::collections::HashMap;

use chrono::{DateTime, Duration, FixedOffset, Timelike, Utc};

use crate::api::LimitKind;
use crate::history::{
    same_reset, AccountRef, Event, EventKind, Hosted, LimitSample, Sample, TokenRecord,
};

use super::{
    hour_of_week, week_weight, work_units, Rates, Rhythm, LEARN_MIN_WU, RATES_DAYS, RHYTHM_WEEKS,
};

/// Average work units per hour of the week from the token records of all hosts
/// in `[now - 4 weeks, now)`, in the local time of `offset`. A clock hour of the
/// week is the weighted mean over the four weeks: the two newer weeks weigh 2,
/// the two older 1. A week without a record in that hour counts as 0 for it. A
/// record's hour is spread over the local clock hours it covers, by the minutes
/// each one holds, so an offset that is not a whole hour splits it between two.
pub fn rhythm(tokens: &[Hosted<TokenRecord>], now: DateTime<Utc>, offset: FixedOffset) -> Rhythm {
    let mut weighted = [0.0f64; super::HOURS];
    let weight_sum: f64 = (0..RHYTHM_WEEKS).map(week_weight).sum();
    for hosted in tokens {
        if hosted.record.t >= now {
            continue;
        }
        let age = now.signed_duration_since(hosted.record.t);
        if age >= Duration::weeks(RHYTHM_WEEKS) {
            continue;
        }
        let week = age.num_days() / 7;
        let record = &hosted.record;
        let units = work_units(record.input, record.out, record.cw, record.cr);
        spread(&mut weighted, record.t, offset, week_weight(week) * units);
    }
    let mut wu = [0.0f64; super::HOURS];
    for (hour, sum) in wu.iter_mut().zip(weighted) {
        *hour = sum / weight_sum;
    }
    Rhythm { wu }
}

/// Add the work of one record's hour to the clock hours it covers, by the share
/// of the hour each one holds. A whole-hour offset leaves it in one hour.
fn spread(weighted: &mut [f64; super::HOURS], t: DateTime<Utc>, offset: FixedOffset, units: f64) {
    let end = t + Duration::hours(1);
    let mut cursor = t;
    while cursor < end {
        let local = cursor.with_timezone(&offset);
        let into_hour = Duration::minutes(local.minute() as i64)
            + Duration::seconds(local.second() as i64)
            + Duration::nanoseconds(local.nanosecond() as i64);
        let to = (cursor - into_hour + Duration::hours(1)).min(end);
        weighted[hour_of_week(cursor, offset)] += units * seconds_between(cursor, to) / 3600.0;
        cursor = to;
    }
}

/// Which account was live on `host` at `t`: the last `live` or `switch` event of
/// that host at or before `t`, the one rule of `Timeline::at`. `None` when no
/// such event exists, or when the last one says no account was live.
pub fn account_at(events: &[Hosted<Event>], host: &str, t: DateTime<Utc>) -> Option<AccountRef> {
    Timeline::of(events, host).at(t).cloned()
}

/// An account's rates, learned from its samples and the work done while it was
/// live (spec §2.3). `c` is the sum of its weekly percent gain over the sum of
/// work units, over its consecutive samples in the last [`RATES_DAYS`] days that
/// lie in the same weekly window and are at most 6 h apart; `s` is the same with
/// the session percent inside one 5 h window. A token record counts with the part
/// of its hour that lies inside a sample pair's interval and in which the record's
/// host had this account live. `None` until the week's work reaches
/// [`LEARN_MIN_WU`]; `s` is `None` without a session limit or without that much
/// session work.
///
/// A pair whose earlier sample is already at 100 % of the week (or of the window,
/// for `s`) cannot rise: that pair and its work are left out. When `c` still comes
/// out at or below 0, the rates are unknown and this is `None`; when `s` does,
/// `s` is `None`.
pub fn rates(
    account: &AccountRef,
    samples: &[Hosted<Sample>],
    tokens: &[Hosted<TokenRecord>],
    events: &[Hosted<Event>],
    now: DateTime<Utc>,
) -> Option<Rates> {
    let pairs = pairs(account, samples, now);

    let mut timelines: HashMap<String, Timeline> = HashMap::new();
    for hosted in events {
        if !timelines.contains_key(&hosted.host) {
            timelines.insert(hosted.host.clone(), Timeline::of(events, &hosted.host));
        }
    }

    let week_spans: Vec<(DateTime<Utc>, DateTime<Utc>)> =
        pairs.iter().map(|pair| (pair.from, pair.to)).collect();
    let week_work = work_in(&timelines, tokens, account, &week_spans);
    if week_work < LEARN_MIN_WU {
        return None;
    }
    let c = pairs.iter().map(|pair| pair.week).sum::<f64>() / week_work;
    if c <= 0.0 {
        return None;
    }

    let session_pairs: Vec<&Pair> = pairs.iter().filter(|pair| pair.session.is_some()).collect();
    let session_spans: Vec<(DateTime<Utc>, DateTime<Utc>)> = session_pairs
        .iter()
        .map(|pair| (pair.from, pair.to))
        .collect();
    let session_work = work_in(&timelines, tokens, account, &session_spans);
    let s = if session_work < LEARN_MIN_WU {
        None
    } else {
        let gain: f64 = session_pairs.iter().filter_map(|pair| pair.session).sum();
        let s = gain / session_work;
        if s > 0.0 {
            Some(s)
        } else {
            None
        }
    };

    Some(Rates { c, s })
}

/// Two consecutive samples of one account that lie in one weekly window.
struct Pair {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    /// The weekly percent gained between the two samples.
    week: f64,
    /// The session percent gained, when both samples lie in one 5 h window.
    session: Option<f64>,
}

/// The pairs of the account's samples of every host in the last [`RATES_DAYS`]
/// days, sorted by time: consecutive, at most 6 h apart, the same weekly reset
/// ([`same_reset`]). A pair whose earlier sample is already at 100 % cannot rise
/// and is left out. A pair whose percent goes down stays in, so the sum of the
/// differences is the last percent minus the first.
fn pairs(account: &AccountRef, samples: &[Hosted<Sample>], now: DateTime<Utc>) -> Vec<Pair> {
    let from = now - Duration::days(RATES_DAYS);
    let mut lines: Vec<&Hosted<Sample>> = samples
        .iter()
        .filter(|hosted| &hosted.record.account == account)
        .filter(|hosted| hosted.record.t >= from && hosted.record.t <= now)
        .collect();
    lines.sort_by(|a, b| {
        a.record
            .t
            .cmp(&b.record.t)
            .then_with(|| a.host.cmp(&b.host))
    });

    let mut pairs = Vec::new();
    for pair in lines.windows(2) {
        let (before, after) = (pair[0], pair[1]);
        let span = after.record.t.signed_duration_since(before.record.t);
        if span <= Duration::zero() || span > Duration::hours(6) {
            continue;
        }
        let (Some(week_before), Some(week_after)) = (
            limit(&before.record, &LimitKind::WeeklyAll),
            limit(&after.record, &LimitKind::WeeklyAll),
        ) else {
            continue;
        };
        let (Some(reset_before), Some(reset_after)) = (week_before.r, week_after.r) else {
            continue;
        };
        if !same_reset(reset_before, reset_after) || week_before.p >= 100.0 {
            continue;
        }
        pairs.push(Pair {
            from: before.record.t,
            to: after.record.t,
            week: week_after.p - week_before.p,
            session: session_gain(&before.record, &after.record),
        });
    }
    pairs
}

fn limit<'a>(sample: &'a Sample, kind: &LimitKind) -> Option<&'a LimitSample> {
    sample.l.iter().find(|limit| &limit.k == kind)
}

/// The session percent a pair gained, when both samples carry a session limit in
/// one 5 h window (the same reset, [`same_reset`]) and the earlier one is below
/// 100 %; otherwise no session pair.
fn session_gain(before: &Sample, after: &Sample) -> Option<f64> {
    let limit_before = limit(before, &LimitKind::Session)?;
    let limit_after = limit(after, &LimitKind::Session)?;
    let (reset_before, reset_after) = (limit_before.r?, limit_after.r?);
    if !same_reset(reset_before, reset_after) || limit_before.p >= 100.0 {
        return None;
    }
    Some(limit_after.p - limit_before.p)
}

/// When an account was live on one host. Every `live` and `switch` event changes
/// it, so the account between two changes is the one of the earlier change.
struct Timeline {
    changes: Vec<(DateTime<Utc>, Option<AccountRef>)>,
}

impl Timeline {
    fn of(events: &[Hosted<Event>], host: &str) -> Timeline {
        let mut changes: Vec<(DateTime<Utc>, Option<AccountRef>)> = events
            .iter()
            .filter(|hosted| hosted.host == host)
            .filter_map(|hosted| match &hosted.record.kind {
                EventKind::Live { account } => Some((hosted.record.t, account.clone())),
                EventKind::Switch { to, .. } => Some((hosted.record.t, Some(to.clone()))),
                _ => None,
            })
            .collect();
        changes.sort_by_key(|change| change.0);
        Timeline { changes }
    }

    fn at(&self, t: DateTime<Utc>) -> Option<&AccountRef> {
        let after = self.changes.partition_point(|change| change.0 <= t);
        if after == 0 {
            None
        } else {
            self.changes[after - 1].1.as_ref()
        }
    }

    /// The seconds of `[from, to)` in which this timeline had `account` live.
    fn seconds(&self, account: &AccountRef, from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
        let mut total = 0.0;
        let mut cursor = from;
        let mut live = self.at(from) == Some(account);
        let first = self.changes.partition_point(|change| change.0 <= from);
        for (at, target) in &self.changes[first..] {
            if *at >= to {
                break;
            }
            if live {
                total += seconds_between(cursor, *at);
            }
            cursor = *at;
            live = target.as_ref() == Some(account);
        }
        if live {
            total += seconds_between(cursor, to);
        }
        total
    }
}

fn seconds_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    let span = to.signed_duration_since(from);
    span.num_seconds() as f64 + span.subsec_nanos() as f64 / 1e9
}

/// The work units of `tokens` inside the given disjoint, time-sorted spans,
/// counting each record only while `account` was live on the record's host and
/// spreading a record evenly over its hour: the hour starts at the record's time,
/// as `transcripts::token_records` groups the answers of one hour.
fn work_in(
    timelines: &HashMap<String, Timeline>,
    tokens: &[Hosted<TokenRecord>],
    account: &AccountRef,
    spans: &[(DateTime<Utc>, DateTime<Utc>)],
) -> f64 {
    if spans.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    for hosted in tokens {
        let start = hosted.record.t;
        let end = start + Duration::hours(1);
        let record = &hosted.record;
        let units = work_units(record.input, record.out, record.cw, record.cr);
        let Some(timeline) = timelines.get(&hosted.host) else {
            continue;
        };
        let first = spans.partition_point(|span| span.1 <= start);
        for span in &spans[first..] {
            if span.0 >= end {
                break;
            }
            let from = span.0.max(start);
            let to = span.1.min(end);
            if from >= to {
                continue;
            }
            total += units * timeline.seconds(account, from, to) / 3600.0;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::history::{By, Source};
    use crate::project::ProjectKey;

    const EPS: f64 = 1e-9;

    const AC_SESSION: &str = "2026-11-14T19:30:00.336716Z";
    const WEEK_RESET: &str = "2026-11-16T16:00:00.336716Z";

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("test date")
    }

    fn east_one() -> FixedOffset {
        FixedOffset::east_opt(3600).expect("+01:00")
    }

    /// A zone that is not a whole hour off UTC.
    fn half_hour() -> FixedOffset {
        FixedOffset::east_opt(5 * 3600 + 1800).expect("+05:30")
    }

    fn ac() -> AccountRef {
        AccountRef {
            acc: "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04".to_string(),
            org: "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20".to_string(),
        }
    }

    fn lb() -> AccountRef {
        AccountRef {
            acc: "3b7d1f04-9c2e-4a61-8d5b-0e7f2a4c9b13".to_string(),
            org: "c41e6b82-7d3a-4f95-b2c8-5a1d9e3f7b46".to_string(),
        }
    }

    fn weekly(percent: f64, reset: &str) -> LimitSample {
        LimitSample {
            k: LimitKind::WeeklyAll,
            m: None,
            p: percent,
            r: Some(utc(reset)),
            b: false,
        }
    }

    fn session(percent: f64, reset: &str) -> LimitSample {
        LimitSample {
            k: LimitKind::Session,
            m: None,
            p: percent,
            r: Some(utc(reset)),
            b: false,
        }
    }

    fn sample(host: &str, at: &str, account: AccountRef, l: Vec<LimitSample>) -> Hosted<Sample> {
        Hosted {
            host: host.to_string(),
            record: Sample {
                t: utc(at),
                account,
                src: Source::Poll,
                l,
            },
        }
    }

    /// A token line of `input` input tokens (one work unit per million).
    fn token(host: &str, at: &str, input: u64) -> Hosted<TokenRecord> {
        Hosted {
            host: host.to_string(),
            record: TokenRecord {
                t: utc(at),
                session: "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34".to_string(),
                cwd: "/home/sam/projects/atlas".to_string(),
                project: ProjectKey::Repo("github.com/sam/atlas".to_string()),
                model: "claude-fable-5-2".to_string(),
                input,
                out: 0,
                cw: 0,
                cr: 0,
                answers: 1,
                agent_answers: 0,
            },
        }
    }

    fn live(host: &str, at: &str, account: Option<AccountRef>) -> Hosted<Event> {
        Hosted {
            host: host.to_string(),
            record: Event {
                t: utc(at),
                kind: EventKind::Live { account },
            },
        }
    }

    fn switched(host: &str, at: &str, to: AccountRef) -> Hosted<Event> {
        Hosted {
            host: host.to_string(),
            record: Event {
                t: utc(at),
                kind: EventKind::Switch {
                    from: None,
                    to,
                    by: By::User,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: None,
                },
            },
        }
    }

    #[test]
    fn work_units_weigh_the_four_kinds() {
        let wu = work_units(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((wu - 7.35).abs() < EPS, "{wu}");
    }

    #[test]
    fn rhythm_averages_per_hour_of_the_week() {
        // The last four Mondays, 09:00 local (+01:00), 2.0 wu each; two lines
        // from another host count the same as this host's.
        let tokens = vec![
            token("atlas", "2026-11-23T08:00:00Z", 2_000_000),
            token("vega", "2026-11-16T08:00:00Z", 2_000_000),
            token("atlas", "2026-11-09T08:00:00Z", 2_000_000),
            token("vega", "2026-11-02T08:00:00Z", 2_000_000),
        ];
        let now = utc("2026-11-23T12:00:00Z");
        let rhythm = rhythm(&tokens, now, east_one());

        assert!((rhythm.wu[9] - 2.0).abs() < EPS, "{:?}", rhythm.wu[9]);
        for (hour, wu) in rhythm.wu.iter().enumerate() {
            if hour != 9 {
                assert!((*wu - 0.0).abs() < EPS, "hour {hour}: {wu}");
            }
        }
        assert!((rhythm.typical_hour() - 2.0).abs() < EPS);
        // Monday 09:30 local is that hour; Tuesday 09:00 local is another one.
        assert!((rhythm.at(utc("2026-11-23T08:30:00Z"), east_one()) - 2.0).abs() < EPS);
        assert!((rhythm.at(utc("2026-11-24T08:00:00Z"), east_one()) - 0.0).abs() < EPS);
    }

    #[test]
    fn newer_weeks_count_double() {
        let tokens = vec![
            token("atlas", "2026-11-23T08:00:00Z", 4_000_000),
            token("atlas", "2026-11-16T08:00:00Z", 4_000_000),
            token("atlas", "2026-11-09T08:00:00Z", 1_000_000),
            token("atlas", "2026-11-02T08:00:00Z", 1_000_000),
        ];
        let rhythm = rhythm(&tokens, utc("2026-11-23T12:00:00Z"), east_one());
        assert!((rhythm.wu[9] - 3.0).abs() < EPS, "{:?}", rhythm.wu[9]);
    }

    #[test]
    fn account_at_follows_live_and_switch_events() {
        let events = vec![
            live("atlas", "2026-11-14T08:00:00Z", Some(lb())),
            switched("atlas", "2026-11-14T10:00:00Z", ac()),
        ];
        assert_eq!(
            account_at(&events, "atlas", utc("2026-11-14T09:00:00Z")),
            Some(lb())
        );
        assert_eq!(
            account_at(&events, "atlas", utc("2026-11-14T10:00:00Z")),
            Some(ac())
        );
        assert_eq!(
            account_at(&events, "atlas", utc("2026-11-14T07:00:00Z")),
            None
        );
        assert_eq!(
            account_at(&events, "vega", utc("2026-11-14T09:00:00Z")),
            None
        );
    }

    #[test]
    fn rates_divide_percent_by_work() {
        // AC live all day: weekly 40 → 48 and session 0 → 100 over 25 wu.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET), session(0.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.0, WEEK_RESET), session(100.0, AC_SESSION)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T10:00:00Z", 10_000_000),
            token("atlas", "2026-11-14T11:00:00Z", 10_000_000),
            token("atlas", "2026-11-14T12:00:00Z", 5_000_000),
        ];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];

        let rates = rates(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        )
        .expect("25 wu is enough work");
        assert!((rates.c - 0.32).abs() < EPS, "{}", rates.c);
        let s = rates.s.expect("a session limit");
        assert!((s - 4.0).abs() < EPS, "{s}");
    }

    #[test]
    fn work_on_another_account_does_not_count() {
        // The same samples and the same 25 wu for AC; 25 wu more fall into the
        // hour in which LB was live on that host. With LB's work counted, c would
        // be 8 / 50 = 0.16.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET), session(0.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.0, WEEK_RESET), session(100.0, AC_SESSION)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T09:00:00Z", 25_000_000),
            token("atlas", "2026-11-14T10:00:00Z", 25_000_000),
        ];
        let events = vec![
            live("atlas", "2026-11-14T08:00:00Z", Some(ac())),
            switched("atlas", "2026-11-14T10:00:00Z", lb()),
        ];

        let rates = rates(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        )
        .expect("AC's 25 wu is enough work");
        assert!((rates.c - 0.32).abs() < EPS, "{}", rates.c);
        let s = rates.s.expect("a session limit");
        assert!((s - 4.0).abs() < EPS, "{s}");
    }

    #[test]
    fn less_than_twenty_work_units_is_unknown() {
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![token("atlas", "2026-11-14T10:00:00Z", 19_900_000)];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];

        assert!(rates(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z")
        )
        .is_none());
    }

    #[test]
    fn a_reset_between_two_samples_is_not_a_drop() {
        // A valid pair gains 8 % of the week over 25 wu: c = 8 / 25 = 0.32.
        // The 96 % → 3 % pair crosses the Monday reset; keeping it would add
        // -93 % over 25 wu more, so c would be (8 - 93) / 50 = -1.7.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-16T15:00:00Z",
                ac(),
                vec![weekly(96.0, "2026-11-16T16:00:00.336716Z")],
            ),
            sample(
                "atlas",
                "2026-11-16T17:00:00Z",
                ac(),
                vec![weekly(3.0, "2026-11-23T16:00:00.336716Z")],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T10:00:00Z", 25_000_000),
            token("atlas", "2026-11-16T16:00:00Z", 25_000_000),
        ];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];

        let rates = rates(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-16T18:00:00Z"),
        )
        .expect("the valid pair carries 25 wu");
        assert!((rates.c - 0.32).abs() < EPS, "c = {}", rates.c);
    }

    #[test]
    fn an_account_without_a_session_limit_has_no_s() {
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![token("atlas", "2026-11-14T10:00:00Z", 25_000_000)];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];

        let rates = rates(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        )
        .expect("25 wu is enough work");
        assert!((rates.c - 0.32).abs() < EPS, "{}", rates.c);
        assert!(rates.s.is_none());
    }

    #[test]
    fn a_switch_inside_an_hour_splits_its_work() {
        // A switch at 11:20 splits the 11:00 token line's hour 20/40: AC gets one
        // third of its 10.0 wu, so 23 1/3 wu carry 8.4 % of the week.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.4, WEEK_RESET)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T09:00:00Z", 10_000_000),
            token("atlas", "2026-11-14T10:00:00Z", 10_000_000),
            token("atlas", "2026-11-14T11:00:00Z", 10_000_000),
        ];
        let now = utc("2026-11-14T18:00:00Z");

        let splitting = vec![
            live("atlas", "2026-11-14T08:00:00Z", Some(ac())),
            switched("atlas", "2026-11-14T11:20:00Z", lb()),
        ];
        let split =
            rates(&ac(), &samples, &tokens, &splitting, now).expect("23 1/3 wu is enough work");
        assert!((split.c - 0.36).abs() < EPS, "{}", split.c);

        // AC live the whole hour: the same lines are worth 30.0 wu, and the rate
        // would be lower.
        let whole_hour = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];
        let unsplit =
            rates(&ac(), &samples, &tokens, &whole_hour, now).expect("30 wu is enough work");
        assert!((unsplit.c - 8.4 / 30.0).abs() < EPS, "{}", unsplit.c);
    }

    #[test]
    fn two_token_lines_of_one_hour_add_up() {
        // Three lines of one session, hour and model (one per scan), and one line
        // from another host: all four are work, 20.0 wu over the pair.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T10:00:00Z", 5_000_000),
            token("atlas", "2026-11-14T10:00:00Z", 5_000_000),
            token("atlas", "2026-11-14T10:00:00Z", 5_000_000),
            token("vega", "2026-11-14T10:00:00Z", 5_000_000),
        ];
        let events = vec![
            live("atlas", "2026-11-14T08:00:00Z", Some(ac())),
            live("vega", "2026-11-14T08:00:00Z", Some(ac())),
        ];

        let rates = rates(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        )
        .expect("20 wu is enough work");
        assert!((rates.c - 0.4).abs() < EPS, "{}", rates.c);
    }

    // ---- proof tests of the review ----

    #[test]
    fn a_half_hour_offset_splits_a_record_over_two_local_hours() {
        // 10:00Z is 15:30 in +05:30, so the hour of the record (10:00Z to
        // 11:00Z) is 15:30 to 16:30 local: half of its work is in local hour
        // 15 and half in local hour 16.
        let tokens = vec![
            token("atlas", "2026-11-23T10:00:00Z", 2_000_000),
            token("atlas", "2026-11-16T10:00:00Z", 2_000_000),
            token("atlas", "2026-11-09T10:00:00Z", 2_000_000),
            token("atlas", "2026-11-02T10:00:00Z", 2_000_000),
        ];
        let rhythm = rhythm(&tokens, utc("2026-11-23T12:00:00Z"), half_hour());

        assert!(
            (rhythm.wu[15] - 1.0).abs() < EPS,
            "hour 15: {}",
            rhythm.wu[15]
        );
        assert!(
            (rhythm.wu[16] - 1.0).abs() < EPS,
            "hour 16: {}",
            rhythm.wu[16]
        );
        let total: f64 = rhythm.wu.iter().sum();
        assert!((total - 2.0).abs() < EPS, "total: {total}");
    }

    #[test]
    fn a_record_at_now_is_outside_the_four_weeks() {
        // The window is [now - 4 weeks, now): the hour of a record that starts
        // at now lies in the future and is not work of the past four weeks.
        let tokens = vec![token("atlas", "2026-11-23T08:00:00Z", 2_000_000)];
        let rhythm = rhythm(&tokens, utc("2026-11-23T08:00:00Z"), east_one());

        assert!((rhythm.wu[9] - 0.0).abs() < EPS, "hour 9: {}", rhythm.wu[9]);
    }

    #[test]
    fn a_percent_that_cannot_rise_gives_no_rate() {
        // The account is at 100 % of its week and of its window: neither
        // percent can rise, so the pair and its 25 wu are left out. With no
        // pair left there is not enough work and no rate at all; a rate of 0 %
        // per work unit is not a rate: task 5 divides by it (rem / c,
        // (100 - session%) / s and 100 * c / s).
        let full = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(100.0, WEEK_RESET), session(100.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(100.0, WEEK_RESET), session(100.0, AC_SESSION)],
            ),
        ];
        let tokens = vec![token("atlas", "2026-11-14T16:00:00Z", 25_000_000)];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];
        assert!(rates(&ac(), &full, &tokens, &events, utc("2026-11-14T18:00:00Z")).is_none());

        // A pair that can rise beside a 100 % one keeps its positive rate; the
        // 100 % pair's 25 wu stay out (with them c would be 8 / 50 = 0.16 and
        // s 50 / 50 = 1.0).
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET), session(0.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(48.0, WEEK_RESET), session(50.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-16T09:00:00Z",
                ac(),
                vec![weekly(100.0, WEEK_RESET), session(100.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-16T13:00:00Z",
                ac(),
                vec![weekly(100.0, WEEK_RESET), session(100.0, AC_SESSION)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T10:00:00Z", 25_000_000),
            token("atlas", "2026-11-16T10:00:00Z", 25_000_000),
        ];
        let rates = rates(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-16T18:00:00Z"),
        )
        .expect("the 25 wu of the pair that can rise is enough work");
        assert!((rates.c - 0.32).abs() < EPS, "c = {}", rates.c);
        let s = rates.s.expect("a session limit");
        assert!((s - 2.0).abs() < EPS, "s = {s}");
    }
}
