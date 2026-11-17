//! The learning side of auto-burn (spec §2.3): the weekly rhythm of work, which
//! account was live when, and the rates an account's limits cost per work unit.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::api::LimitKind;
use crate::history::{
    is_early_reset, same_reset, AccountRef, Event, EventKind, Hosted, Sample, TokenRecord,
};
use crate::money::is_plan_model;
use crate::store::Account;

use crate::stats::{local_hour_of_week, local_hour_start};

use super::{
    week_weight, work_units, Rates, Rhythm, ASSUMED_WEEK_PERCENT_PER_WU, LEARN_MIN_WU, RATES_DAYS,
    RHYTHM_WEEKS,
};

/// Average work units per hour of the week from the Claude token records of
/// all hosts in `[now - 4 weeks, now)`, in the local time of `zone`. A record
/// of another provider (Codex, Ollama: [`crate::money::is_plan_model`] is
/// false) uses no Claude account, so it is no work in the rates' units and is
/// left out. A clock hour of the
/// week is the weighted mean over the four weeks: the two newer weeks weigh 2,
/// the two older 1. A week without a record in that hour counts as 0 for it. A
/// record's hour is spread over the local clock hours it covers, by the minutes
/// each one holds, so an offset that is not a whole hour splits it between two
/// and a daylight-saving change is followed.
pub fn rhythm<Tz: TimeZone + Copy>(
    tokens: &[Hosted<TokenRecord>],
    now: DateTime<Utc>,
    zone: &Tz,
) -> Rhythm {
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
        if !is_plan_model(&record.model) {
            continue;
        }
        let units = work_units(record.input, record.out, record.cw, record.cr);
        spread(&mut weighted, record.t, zone, week_weight(week) * units);
    }
    let mut wu = [0.0f64; super::HOURS];
    for (hour, sum) in wu.iter_mut().zip(weighted) {
        *hour = sum / weight_sum;
    }
    Rhythm { wu }
}

/// Add the work of one record's hour to the clock hours it covers, by the share
/// of the hour each one holds. A whole-hour offset leaves it in one hour. The
/// hour boundaries and the hour-of-week index are the statistics' one rule.
fn spread<Tz: TimeZone + Copy>(
    weighted: &mut [f64; super::HOURS],
    t: DateTime<Utc>,
    zone: &Tz,
    units: f64,
) {
    let end = t + Duration::hours(1);
    let mut cursor = t;
    while cursor < end {
        let to = (local_hour_start(cursor, zone) + Duration::hours(1)).min(end);
        weighted[local_hour_of_week(cursor, zone)] += units * seconds_between(cursor, to) / 3600.0;
        cursor = to;
    }
}

/// Which account was live on `host` at `t`: the last `live` or `switch` event of
/// that host at or before `t`, the one rule of `Timeline::at`. `None` when no
/// such event exists, or when the last one says no account was live.
pub fn account_at(events: &[Hosted<Event>], host: &str, t: DateTime<Utc>) -> Option<AccountRef> {
    Timeline::of(events, host).at(t).cloned()
}

/// One account the rates are learned for: how the history names it and the
/// capacity of its plan ([`Account::capacity`]: 5 for Max, 20 for Ultra;
/// `None` when its tier names no multiplier).
#[derive(Debug, Clone, PartialEq)]
pub struct RateAccount {
    pub account: AccountRef,
    pub capacity: Option<f64>,
}

impl RateAccount {
    pub fn of(account: &Account) -> RateAccount {
        RateAccount {
            account: AccountRef::of(account),
            capacity: account.capacity(),
        }
    }
}

/// Every account's rates (spec §2.3): measured from its samples and the work
/// done while it was live, and, for an account whose plan has a capacity,
/// started from an assumption that the measurement takes over.
///
/// The measurement: over the account's consecutive samples in the last
/// [`RATES_DAYS`] days that lie in one weekly window, are at most 6 h apart and
/// in which the account was live on some host for some time, the sum of the
/// weekly percent gained and the sum of the work units done (a token record
/// counts with the part of its hour inside the pair in which its host had the
/// account live). The same inside one 5 h window gives the session's. A pair
/// in which the account was live on no host is left out: a rise there comes
/// from use seatswap does not see (claude.ai, another machine). A pair
/// whose earlier sample is already at 100 % cannot rise and is left out too.
///
/// The assumption: the weekly percent per work unit of a plan of capacity 1 is
/// [`ASSUMED_WEEK_PERCENT_PER_WU`], weighed as [`LEARN_MIN_WU`] work units,
/// joined with the measurements of every account that has a capacity (gain ×
/// capacity over work). An account of capacity `n` assumes that divided by
/// `n`, again weighed as [`LEARN_MIN_WU`] work units next to its own
/// measurement: `c = (gain + assumed × LEARN_MIN_WU) / (work + LEARN_MIN_WU)`.
/// `s` is assumed the same way from the measured sessions only (there is no
/// built-in session value), and only for an account whose latest sample has a
/// session limit.
///
/// Without a capacity an account has only its measurement: `c` is `None`
/// until its work reaches [`LEARN_MIN_WU`], and `s` likewise. A `c` at or
/// below 0 leaves the account out; an `s` at or below 0 is `None`.
pub fn rates(
    accounts: &[RateAccount],
    samples: &[Hosted<Sample>],
    tokens: &[Hosted<TokenRecord>],
    events: &[Hosted<Event>],
    now: DateTime<Utc>,
) -> BTreeMap<AccountRef, Rates> {
    let timelines = timelines(events);
    let measured: Vec<(&RateAccount, Measured)> = accounts
        .iter()
        .map(|entry| {
            let measured = measure(&entry.account, samples, tokens, &timelines, now);
            (entry, measured)
        })
        .collect();

    // Percent per work unit at capacity 1, over every account with a capacity.
    let pooled = |gain: fn(&Measured) -> (f64, f64), start: Option<f64>| {
        let (mut sum_gain, mut sum_work) = match start {
            Some(assumed) => (assumed * LEARN_MIN_WU, LEARN_MIN_WU),
            None => (0.0, 0.0),
        };
        for (entry, measured) in &measured {
            if let Some(capacity) = entry.capacity {
                let (gained, work) = gain(measured);
                sum_gain += gained * capacity;
                sum_work += work;
            }
        }
        (sum_work >= LEARN_MIN_WU && sum_gain > 0.0).then(|| sum_gain / sum_work)
    };
    let week_per_wu = pooled(
        |m| (m.week_gain, m.week_work),
        Some(ASSUMED_WEEK_PERCENT_PER_WU),
    );
    let session_per_wu = pooled(|m| (m.session_gain, m.session_work), None);

    let mut learned = BTreeMap::new();
    for (entry, measured) in &measured {
        let assumed = |per_wu: Option<f64>| Some(per_wu? / entry.capacity?);
        let Some(c) = blend(measured.week_gain, measured.week_work, assumed(week_per_wu)) else {
            continue;
        };
        let s = if measured.has_session {
            blend(
                measured.session_gain,
                measured.session_work,
                assumed(session_per_wu),
            )
        } else {
            None
        };
        learned.insert(entry.account.clone(), Rates { c, s });
    }
    learned
}

/// A rate from an account's gain and work and an assumed rate weighed as
/// [`LEARN_MIN_WU`] work units; without an assumption, the measurement alone
/// once its work reaches [`LEARN_MIN_WU`]. `None` when there is neither, or
/// the rate is at or below 0.
fn blend(gain: f64, work: f64, assumed: Option<f64>) -> Option<f64> {
    let rate = match assumed {
        Some(assumed) => (gain + assumed * LEARN_MIN_WU) / (work + LEARN_MIN_WU),
        None if work >= LEARN_MIN_WU => gain / work,
        None => return None,
    };
    (rate > 0.0).then_some(rate)
}

/// One account's measurement over its usable sample pairs (see [`rates`]).
struct Measured {
    week_gain: f64,
    week_work: f64,
    session_gain: f64,
    session_work: f64,
    /// The account's latest sample in the range has a session limit.
    has_session: bool,
}

fn measure(
    account: &AccountRef,
    samples: &[Hosted<Sample>],
    tokens: &[Hosted<TokenRecord>],
    timelines: &HashMap<String, Timeline>,
    now: DateTime<Utc>,
) -> Measured {
    let pairs: Vec<Pair> = pairs(account, samples, now)
        .into_iter()
        .filter(|pair| {
            timelines
                .values()
                .any(|timeline| timeline.seconds(account, pair.from, pair.to) > 0.0)
        })
        .collect();
    let week_spans: Vec<(DateTime<Utc>, DateTime<Utc>)> =
        pairs.iter().map(|pair| (pair.from, pair.to)).collect();
    let session_pairs: Vec<&Pair> = pairs.iter().filter(|pair| pair.session.is_some()).collect();
    let session_spans: Vec<(DateTime<Utc>, DateTime<Utc>)> = session_pairs
        .iter()
        .map(|pair| (pair.from, pair.to))
        .collect();
    let from = now - Duration::days(RATES_DAYS);
    let has_session = samples
        .iter()
        .filter(|hosted| &hosted.record.account == account)
        .filter(|hosted| hosted.record.t >= from && hosted.record.t <= now)
        .max_by_key(|hosted| hosted.record.t)
        .is_some_and(|hosted| hosted.record.limit(&LimitKind::Session).is_some());
    Measured {
        week_gain: pairs.iter().map(|pair| pair.week).sum(),
        week_work: work_in(timelines, tokens, account, &week_spans),
        session_gain: session_pairs.iter().filter_map(|pair| pair.session).sum(),
        session_work: work_in(timelines, tokens, account, &session_spans),
        has_session,
    }
}

/// The timeline of every host that has an event. `pub(crate)` so `money` reads
/// a record's account through the same timelines, without a second copy.
pub(crate) fn timelines(events: &[Hosted<Event>]) -> HashMap<String, Timeline> {
    let mut timelines: HashMap<String, Timeline> = HashMap::new();
    for hosted in events {
        if !timelines.contains_key(&hosted.host) {
            timelines.insert(hosted.host.clone(), Timeline::of(events, &hosted.host));
        }
    }
    timelines
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
/// and is left out. A pair whose weekly percent fell by the reset rule
/// ([`is_early_reset`]) is a reset and is left out too, as a changed reset time
/// already is. A pair that goes down by less stays in, so the sum of the
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
            before.record.limit(&LimitKind::WeeklyAll),
            after.record.limit(&LimitKind::WeeklyAll),
        ) else {
            continue;
        };
        let (Some(reset_before), Some(reset_after)) = (week_before.r, week_after.r) else {
            continue;
        };
        if !same_reset(reset_before, reset_after)
            || week_before.p >= 100.0
            || is_early_reset(
                week_before.p,
                week_after.p,
                Some(reset_before),
                after.record.t,
            )
        {
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

/// The session percent a pair gained, when both samples carry a session limit in
/// one 5 h window (the same reset, [`same_reset`]) and the earlier one is below
/// 100 %; otherwise no session pair. A session that fell by the reset rule
/// ([`is_early_reset`]) was reset early and gives no session gain either.
fn session_gain(before: &Sample, after: &Sample) -> Option<f64> {
    let limit_before = before.limit(&LimitKind::Session)?;
    let limit_after = after.limit(&LimitKind::Session)?;
    let (reset_before, reset_after) = (limit_before.r?, limit_after.r?);
    if !same_reset(reset_before, reset_after)
        || limit_before.p >= 100.0
        || is_early_reset(limit_before.p, limit_after.p, Some(reset_before), after.t)
    {
        return None;
    }
    Some(limit_after.p - limit_before.p)
}

/// When an account was live on one host. Every `live` and `switch` event changes
/// it, so the account between two changes is the one of the earlier change.
pub(crate) struct Timeline {
    changes: Vec<(DateTime<Utc>, Option<AccountRef>)>,
}

impl Timeline {
    pub(super) fn of(events: &[Hosted<Event>], host: &str) -> Timeline {
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

    pub(crate) fn at(&self, t: DateTime<Utc>) -> Option<&AccountRef> {
        let after = self.changes.partition_point(|change| change.0 <= t);
        if after == 0 {
            None
        } else {
            self.changes[after - 1].1.as_ref()
        }
    }

    /// The moments in `(from, to)` at which the account in use changes.
    pub(crate) fn changes_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> impl Iterator<Item = DateTime<Utc>> + '_ {
        // The changes are sorted by time: start after the last one at or
        // before `from`, stop at the first one at or after `to`.
        let first = self.changes.partition_point(|change| change.0 <= from);
        self.changes[first..]
            .iter()
            .map(|change| change.0)
            .take_while(move |at| *at < to)
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
/// counting each Claude record (a Codex or Ollama record uses no Claude
/// account and is left out) only while `account` was live on the record's host and
/// spreading a record evenly over its hour: the hour starts at the record's time,
/// as `transcripts::token_records` groups the answers of one hour. `tokens`
/// are sorted by time, as a history read returns them, so only the records
/// whose hour reaches into the spans are looked at.
pub(super) fn work_in(
    timelines: &HashMap<String, Timeline>,
    tokens: &[Hosted<TokenRecord>],
    account: &AccountRef,
    spans: &[(DateTime<Utc>, DateTime<Utc>)],
) -> f64 {
    let (Some(first_span), Some(last_span)) = (spans.first(), spans.last()) else {
        return 0.0;
    };
    debug_assert!(
        tokens.is_sorted_by_key(|hosted| hosted.record.t),
        "work_in needs the token records sorted by time"
    );
    // A record whose hour ends at or before the first span, or that starts at
    // or after the last span's end, has no part inside a span.
    let from =
        tokens.partition_point(|hosted| hosted.record.t + Duration::hours(1) <= first_span.0);
    let to = tokens.partition_point(|hosted| hosted.record.t < last_span.1);
    let mut total = 0.0;
    for hosted in &tokens[from..to.max(from)] {
        let record = &hosted.record;
        // Another provider's work uses no Claude account.
        if !is_plan_model(&record.model) {
            continue;
        }
        let start = record.t;
        let end = start + Duration::hours(1);
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

    use chrono::FixedOffset;

    use crate::history::{By, LimitSample, Source};
    use crate::project::ProjectKey;

    const EPS: f64 = 1e-9;

    const AC_SESSION: &str = "2026-11-14T19:30:00.336716Z";
    const WEEK_RESET: &str = "2026-11-16T16:00:00.336716Z";

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("test date")
    }

    /// One account's rates without a plan capacity: its measurement alone.
    fn rates_of(
        account: &AccountRef,
        samples: &[Hosted<Sample>],
        tokens: &[Hosted<TokenRecord>],
        events: &[Hosted<Event>],
        now: DateTime<Utc>,
    ) -> Option<Rates> {
        let accounts = [RateAccount {
            account: account.clone(),
            capacity: None,
        }];
        rates(&accounts, samples, tokens, events, now).remove(account)
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

    fn pr() -> AccountRef {
        AccountRef {
            acc: "8e2c5a17-4b9d-4f30-a6e1-2d7b9c0f5a84".to_string(),
            org: "f0a3d6c9-2e8b-4c71-9b54-7a1e3d6f8c20".to_string(),
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
        let rhythm = rhythm(&tokens, now, &east_one());

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
        let rhythm = rhythm(&tokens, utc("2026-11-23T12:00:00Z"), &east_one());
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

        let rates = rates_of(
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

    /// The same token line on another provider's model.
    fn token_of(model: &str, host: &str, at: &str, input: u64) -> Hosted<TokenRecord> {
        let mut hosted = token(host, at, input);
        hosted.record.model = model.to_string();
        hosted
    }

    /// Codex and flash work done while AC was live uses no Claude account: it
    /// changes neither AC's rates, nor the switch cost's work, nor the rhythm.
    #[test]
    fn another_providers_work_leaves_the_rates_alone() {
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
        let claude = vec![
            token("atlas", "2026-11-14T10:00:00Z", 10_000_000),
            token("atlas", "2026-11-14T11:00:00Z", 10_000_000),
            token("atlas", "2026-11-14T12:00:00Z", 5_000_000),
        ];
        let mut mixed = claude.clone();
        mixed.push(token_of(
            "gpt-6.1-sol",
            "atlas",
            "2026-11-14T10:00:00Z",
            30_000_000,
        ));
        mixed.push(token_of(
            "deepseek-v4.1-flash:cloud",
            "atlas",
            "2026-11-14T11:30:00Z",
            40_000_000,
        ));
        mixed.sort_by_key(|hosted| hosted.record.t);
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];
        let now = utc("2026-11-14T18:00:00Z");

        let alone = rates_of(&ac(), &samples, &claude, &events, now).expect("25 wu of Claude");
        let with_others = rates_of(&ac(), &samples, &mixed, &events, now).expect("the same 25 wu");
        assert_eq!((with_others.c, with_others.s), (alone.c, alone.s));
        assert!((with_others.c - 0.32).abs() < EPS, "{}", with_others.c);

        let timelines = timelines(&events);
        let spans = [(utc("2026-11-14T09:00:00Z"), utc("2026-11-14T13:00:00Z"))];
        assert!((work_in(&timelines, &mixed, &ac(), &spans) - 25.0).abs() < EPS);
        assert_eq!(
            rhythm(&mixed, now, &east_one()),
            rhythm(&claude, now, &east_one()),
            "the rhythm counts Claude work only"
        );
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

        let rates = rates_of(
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

        assert!(rates_of(
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

        let rates = rates_of(
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
    fn a_reset_that_keeps_its_reset_time_is_left_out_of_the_rates() {
        // The review's case: 40 → 48 → 0 at 09:00, 13:00 and 15:00 with the
        // same reset time. The 13:00 → 15:00 pair is an early reset, not -48
        // points of usage; keeping it would make c (8 - 48) / 50 = -0.8 and
        // `rates` None. The rising pair's 25 wu gives c = 8 / 25 = 0.32.
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
                "2026-11-14T15:00:00Z",
                ac(),
                vec![weekly(0.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T10:00:00Z", 25_000_000),
            token("atlas", "2026-11-14T14:00:00Z", 25_000_000),
        ];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];

        let rates = rates_of(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        )
        .expect("the rising pair carries 25 wu");
        assert!((rates.c - 0.32).abs() < EPS, "c = {}", rates.c);
    }

    #[test]
    fn a_model_reset_that_keeps_its_reset_time_is_left_out() {
        // A model reset takes the week from 70 % to 45 % with the reset time
        // unchanged. The pair is a reset: with it c would be (30 - 25) / 50 =
        // 0.1; without it the rising pair's 30 points over 25 wu give 1.2.
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
                vec![weekly(70.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T15:00:00Z",
                ac(),
                vec![weekly(45.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T10:00:00Z", 25_000_000),
            token("atlas", "2026-11-14T14:00:00Z", 25_000_000),
        ];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];

        let rates = rates_of(
            &ac(),
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        )
        .expect("the rising pair carries 25 wu");
        assert!((rates.c - 1.2).abs() < EPS, "c = {}", rates.c);
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

        let rates = rates_of(
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
            rates_of(&ac(), &samples, &tokens, &splitting, now).expect("23 1/3 wu is enough work");
        assert!((split.c - 0.36).abs() < EPS, "{}", split.c);

        // AC live the whole hour: the same lines are worth 30.0 wu, and the rate
        // would be lower.
        let whole_hour = vec![live("atlas", "2026-11-14T08:00:00Z", Some(ac()))];
        let unsplit =
            rates_of(&ac(), &samples, &tokens, &whole_hour, now).expect("30 wu is enough work");
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

        let rates = rates_of(
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
        let rhythm = rhythm(&tokens, utc("2026-11-23T12:00:00Z"), &half_hour());

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
        let rhythm = rhythm(&tokens, utc("2026-11-23T08:00:00Z"), &east_one());

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
        assert!(rates_of(&ac(), &full, &tokens, &events, utc("2026-11-14T18:00:00Z")).is_none());

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
        let rates = rates_of(
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

    /// A record at 09:00 in +02:00 stays in hour 9 of the week even when the
    /// four weeks start in +01:00: the rhythm uses the offset of each instant.
    #[test]
    fn a_summer_record_is_in_its_own_rhythm_hour() {
        let tokens = vec![token("atlas", "2026-03-30T07:00:00Z", 2_000_000)];
        let rhythm = rhythm(
            &tokens,
            utc("2026-03-30T12:00:00Z"),
            &crate::stats::tests::SummerZone,
        );

        assert!(
            (rhythm.wu[9] - 2.0 / 3.0).abs() < EPS,
            "hour 9: {}",
            rhythm.wu[9]
        );
        assert!((rhythm.wu[8] - 0.0).abs() < EPS, "hour 8: {}", rhythm.wu[8]);
    }

    fn with_capacity(account: AccountRef, capacity: Option<f64>) -> RateAccount {
        RateAccount { account, capacity }
    }

    #[test]
    fn a_plan_without_any_measurement_starts_from_the_assumption() {
        // No sample at all: a 5x plan assumes the built-in rate over 5, a 20x
        // plan over 20, and a plan without a capacity has no rates.
        let accounts = [with_capacity(ac(), Some(5.0)), with_capacity(lb(), None)];
        let learned = rates(&accounts, &[], &[], &[], utc("2026-11-14T18:00:00Z"));
        let ac_rates = learned.get(&ac()).expect("AC has a capacity");
        assert!(
            (ac_rates.c - ASSUMED_WEEK_PERCENT_PER_WU / 5.0).abs() < EPS,
            "{}",
            ac_rates.c
        );
        assert!(ac_rates.s.is_none(), "no session was ever measured");
        assert!(!learned.contains_key(&lb()), "no capacity, no measurement");
    }

    #[test]
    fn a_measured_plan_moves_the_assumption_of_the_others() {
        // LB (capacity 20) gained 10 % of its week over 200 wu while live.
        // At capacity 1 that is (10 × 20 + 1.235 × 20) / (200 + 20) % per wu;
        // AC (capacity 5, never measured) assumes a fifth of it. LB itself joins
        // its own 200 wu with its share of the assumption, weighed as 20 wu.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                lb(),
                vec![weekly(40.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                lb(),
                vec![weekly(50.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![
            token("atlas", "2026-11-14T09:00:00Z", 100_000_000),
            token("atlas", "2026-11-14T10:00:00Z", 100_000_000),
        ];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(lb()))];
        let accounts = [
            with_capacity(lb(), Some(20.0)),
            with_capacity(ac(), Some(5.0)),
        ];
        let learned = rates(
            &accounts,
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        );

        let per_wu =
            (10.0 * 20.0 + ASSUMED_WEEK_PERCENT_PER_WU * LEARN_MIN_WU) / (200.0 + LEARN_MIN_WU);
        let ac_c = learned.get(&ac()).expect("AC").c;
        assert!((ac_c - per_wu / 5.0).abs() < EPS, "{ac_c}");
        let lb_c = learned.get(&lb()).expect("LB").c;
        let lb_expected = (10.0 + per_wu / 20.0 * LEARN_MIN_WU) / (200.0 + LEARN_MIN_WU);
        assert!((lb_c - lb_expected).abs() < EPS, "{lb_c}");
    }

    #[test]
    fn a_rise_while_the_account_is_live_nowhere_is_left_out() {
        // AC rose 30 % while LB was live on the only host: the rise came from
        // use seatswap does not see. AC keeps its assumption unchanged; without
        // a capacity it has no rates at all.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                ac(),
                vec![weekly(10.0, WEEK_RESET)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(40.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![token("atlas", "2026-11-14T09:00:00Z", 100_000_000)];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(lb()))];
        let now = utc("2026-11-14T18:00:00Z");

        let learned = rates(
            &[with_capacity(ac(), Some(5.0))],
            &samples,
            &tokens,
            &events,
            now,
        );
        let c = learned.get(&ac()).expect("AC has a capacity").c;
        assert!((c - ASSUMED_WEEK_PERCENT_PER_WU / 5.0).abs() < EPS, "{c}");
        assert!(rates_of(&ac(), &samples, &tokens, &events, now).is_none());
    }

    #[test]
    fn a_session_rate_is_assumed_from_another_measured_session() {
        // LB (capacity 5) filled 50 % of a 5 h window with 25 wu: 2 % per wu,
        // 10 % at capacity 1. AC (capacity 20, capped, no work) assumes 0.5 %
        // per wu; PR (capacity 20, no session limit) gets no s.
        let samples = vec![
            sample(
                "atlas",
                "2026-11-14T09:00:00Z",
                lb(),
                vec![weekly(40.0, WEEK_RESET), session(0.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                lb(),
                vec![weekly(48.0, WEEK_RESET), session(50.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                ac(),
                vec![weekly(1.0, WEEK_RESET), session(0.0, AC_SESSION)],
            ),
            sample(
                "atlas",
                "2026-11-14T13:00:00Z",
                pr(),
                vec![weekly(1.0, WEEK_RESET)],
            ),
        ];
        let tokens = vec![token("atlas", "2026-11-14T10:00:00Z", 25_000_000)];
        let events = vec![live("atlas", "2026-11-14T08:00:00Z", Some(lb()))];
        let accounts = [
            with_capacity(lb(), Some(5.0)),
            with_capacity(ac(), Some(20.0)),
            with_capacity(pr(), Some(20.0)),
        ];
        let learned = rates(
            &accounts,
            &samples,
            &tokens,
            &events,
            utc("2026-11-14T18:00:00Z"),
        );
        let s = learned.get(&ac()).expect("AC").s.expect("AC is capped");
        assert!((s - 50.0 * 5.0 / 25.0 / 20.0).abs() < EPS, "{s}");
        assert!(learned.get(&pr()).expect("PR").s.is_none());
    }
}
