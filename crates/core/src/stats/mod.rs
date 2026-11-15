//! The statistics pages (spec §3.1–§3.6, §3.9): what the history, the prices
//! and the projects mean for a chosen range. Everything here only reads.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Duration, FixedOffset, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::api::LimitKind;
use crate::burn::{rates, Rhythm, RHYTHM_WEEKS};
use crate::history::{
    AccountRef, By, Event, EventKind, HistoryDir, HistoryError, Hosted, Hosts, Range, Record,
    Sample, SessionRecord, TokenRecord, UpRecord, WindowRecord,
};
use crate::money::{percent_value, Fx, Missing, MoneyError, Prices, Valuer};
use crate::projects::{Projects, ProjectsLogRecord};
use crate::store::{Account, Settings};

pub mod overview;

#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    /// A value the statistics cannot compute; the text says what is missing.
    #[error("{0}")]
    Message(String),
}

/// The range switch of the statistics area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeKey {
    ThisWeek,
    LastWeek,
    Days30,
    Days90,
    Months12,
    All,
}

/// What the statistics pages ask for: a range, account ids and one host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub range: RangeKey,
    /// Account ids; empty = all.
    pub accounts: Vec<String>,
    /// One host, or every host when `None`.
    pub host: Option<String>,
}

/// Everything a page reads: the history, the store's accounts and settings, the
/// prices, the projects and the local time the pages are shown in.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub history: &'a HistoryDir,
    pub accounts: &'a [Account],
    pub settings: &'a Settings,
    pub prices: &'a Prices,
    pub fx: &'a Fx,
    pub projects: &'a Projects,
    pub now: DateTime<Utc>,
    pub offset: FixedOffset,
}

/// ThisWeek: Monday 00:00 local of this week .. now. LastWeek: the week before. Days30 / Days90 / Months12: from the
/// Monday 00:00 of the week that holds now - 30 d / 90 d / 365 d .. now. All: from the first record .. now, or from
/// this week's Monday 00:00 when the history holds no record.
pub fn range_of(key: RangeKey, ctx: &Ctx) -> Result<Range, StatsError> {
    let this_week = week_start(ctx.now, ctx.offset);
    let (from, to) = match key {
        RangeKey::ThisWeek => (this_week, ctx.now),
        RangeKey::LastWeek => (this_week - Duration::days(7), this_week),
        RangeKey::Days30 => (
            week_start(ctx.now - Duration::days(30), ctx.offset),
            ctx.now,
        ),
        RangeKey::Days90 => (
            week_start(ctx.now - Duration::days(90), ctx.offset),
            ctx.now,
        ),
        RangeKey::Months12 => (
            week_start(ctx.now - Duration::days(365), ctx.offset),
            ctx.now,
        ),
        RangeKey::All => (first_record(ctx)?.unwrap_or(this_week), ctx.now),
    };
    Range::between(Some(from), Some(to)).map_err(StatsError::from)
}

/// Monday 00:00 local time of the week that holds `t`, as an instant.
pub fn week_start(t: DateTime<Utc>, offset: FixedOffset) -> DateTime<Utc> {
    let local = t.with_timezone(&offset);
    let days = i64::from(local.weekday().num_days_from_monday());
    let into_day = Duration::hours(i64::from(local.hour()))
        + Duration::minutes(i64::from(local.minute()))
        + Duration::seconds(i64::from(local.second()))
        + Duration::nanoseconds(i64::from(local.nanosecond()));
    t - Duration::days(days) - into_day
}

/// One finished weekly window, put into the calendar week its midpoint falls in.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountWeek {
    pub week: DateTime<Utc>,
    pub account: AccountRef,
    pub window: WindowRecord,
    /// The account's monthly price at the window's midpoint, in the display
    /// currency, from the [`EventKind::PriceChanged`] events of the history;
    /// `None` when `price_missing` says why there is none.
    pub price: Option<f64>,
    /// Why `price` is `None`; `None` when it has a value.
    pub price_missing: Option<Missing>,
}

/// Every finished weekly_all window whose midpoint lies in the range, of the chosen accounts, any host; if several
/// hosts recorded the same window (same account and end), the one with the latest lastSample.
pub fn account_weeks(ctx: &Ctx, range: &Range, q: &Query) -> Result<Vec<AccountWeek>, StatsError> {
    let valuer = Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
    )?;
    account_weeks_valued(ctx, range, q, &valuer)
}

/// [`account_weeks`] with a [`Valuer`] the caller already built for the same
/// range, so a page that values token records too builds only one.
pub(crate) fn account_weeks_valued(
    ctx: &Ctx,
    range: &Range,
    q: &Query,
    valuer: &Valuer,
) -> Result<Vec<AccountWeek>, StatsError> {
    let mut weeks = Vec::new();
    for hosted in ctx.history.finished_windows(&Range::ALL, &Hosts::All)? {
        let window = hosted.record;
        if window.k != LimitKind::WeeklyAll {
            continue;
        }
        let Some(midpoint) = window.end.checked_sub_signed(Duration::hours(84)) else {
            continue;
        };
        if midpoint < range.from || midpoint >= range.to {
            continue;
        }
        let stored = stored_account(ctx, &window.account);
        if !q.accounts.is_empty() {
            let Some(account) = stored else {
                continue;
            };
            if !q.accounts.iter().any(|id| id == &account.id) {
                continue;
            }
        }
        let (price, price_missing) = match valuer.price_at(&window.account, midpoint) {
            Ok(price) => (Some(price), None),
            Err(missing) => (None, Some(missing)),
        };
        weeks.push(AccountWeek {
            week: week_start(midpoint, ctx.offset),
            account: window.account.clone(),
            price,
            price_missing,
            window,
        });
    }
    Ok(weeks)
}

/// The switches of the range by who made them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchCounts {
    pub user: u32,
    pub cli: u32,
    pub auto: u32,
    pub agent: u32,
    pub external: u32,
}

/// The `Switch` events of `range`, only of `host` when it is set, by their
/// `by`.
pub(crate) fn switch_counts(
    events: &[Hosted<Event>],
    range: &Range,
    host: Option<&str>,
) -> SwitchCounts {
    let mut counts = SwitchCounts::default();
    for hosted in events {
        let at = hosted.record.t;
        if at < range.from || at >= range.to {
            continue;
        }
        if host.is_some_and(|host| host != hosted.host) {
            continue;
        }
        let EventKind::Switch { by, .. } = &hosted.record.kind else {
            continue;
        };
        match by {
            By::User => counts.user += 1,
            By::Cli => counts.cli += 1,
            By::Auto => counts.auto += 1,
            By::Agent => counts.agent += 1,
            By::External => counts.external += 1,
        }
    }
    counts
}

/// The calendar weeks of `range`: Monday 00:00 local of the week that holds
/// `range.from`, then every seven days while the start is before `range.to`.
pub(crate) fn week_columns(range: &Range, offset: FixedOffset) -> Vec<DateTime<Utc>> {
    let mut columns = Vec::new();
    let mut week = week_start(range.from, offset);
    while week < range.to {
        columns.push(week);
        week += Duration::weeks(1);
    }
    columns
}

/// The blocked hours of `spans` per calendar week: a span is split at every
/// week start it crosses and each part counts the working hours it has.
pub(crate) fn blocked_by_week(
    spans: &[BlockedSpan],
    rhythm: &Rhythm,
    offset: FixedOffset,
) -> BTreeMap<DateTime<Utc>, f64> {
    let mut by_week = BTreeMap::new();
    for span in spans {
        let mut cursor = span.from;
        while cursor < span.to {
            let week = week_start(cursor, offset);
            let end = (week + Duration::weeks(1)).min(span.to);
            *by_week.entry(week).or_insert(0.0) += working_hours(cursor, end, rhythm, offset);
            cursor = end;
        }
    }
    by_week
}

/// The money of one account week: a full window is `paid`, the share that
/// expired is `expired` and the rest is `used`. `None` without a price; a
/// weekly window without `lost` is an error (the history always writes it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WindowMoney {
    pub(crate) used: f64,
    pub(crate) paid: f64,
    pub(crate) expired: f64,
}

/// The money of a weekly window, from its `lost` percent and its price:
/// `expired = lost × pv`, `paid = 100 × pv`, `used = paid − expired`.
pub(crate) fn money_of(week: &AccountWeek) -> Result<Option<WindowMoney>, StatsError> {
    let lost = lost_percent(&week.window)?;
    let Some(price) = week.price else {
        return Ok(None);
    };
    let pv = percent_value(price);
    let paid = 100.0 * pv;
    let expired = lost * pv;
    Ok(Some(WindowMoney {
        used: paid - expired,
        paid,
        expired,
    }))
}

/// The `lost` percent of a weekly window; a window without it is an error,
/// because seatswap always writes it.
pub(crate) fn lost_percent(window: &WindowRecord) -> Result<f64, StatsError> {
    window.lost.ok_or_else(|| {
        StatsError::Message(format!(
            "the weekly window of {} ending {} has no lost percent; seatswap always writes it",
            window.account.acc, window.end
        ))
    })
}

/// The account week of `account` whose window holds `t`.
pub(crate) fn week_of_record<'a>(
    weeks: &'a [AccountWeek],
    account: &AccountRef,
    t: DateTime<Utc>,
) -> Option<&'a AccountWeek> {
    weeks.iter().find(|week| {
        &week.account == account && week.window.weekly_start() <= t && t < week.window.end
    })
}

/// The id a page writes for `reference`: the stored account's id, or the
/// reference's account id when it is no longer stored.
pub(crate) fn account_id(ctx: &Ctx, reference: &AccountRef) -> String {
    stored_account(ctx, reference)
        .map(|account| account.id.clone())
        .unwrap_or_else(|| reference.acc.clone())
}

/// Money as the page writes it (`crates/app/ui/common.js`'s `money`): two
/// decimals below 10 (`8.31 EUR`), else whole units with thousands commas
/// (`19 EUR`, `1,204 EUR`).
pub(crate) fn money_text(amount: f64, currency: &str) -> String {
    let value = (amount * 100.0).round() / 100.0;
    if value < 10.0 {
        format!("{value:.2} {currency}")
    } else {
        format!("{} {currency}", thousands(value.round() as i64))
    }
}

/// A whole number with a comma every three digits: `1204` → `1,204`.
fn thousands(whole: i64) -> String {
    let digits = whole.unsigned_abs().to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    if whole < 0 {
        format!("-{text}")
    } else {
        text
    }
}

/// Hours with at most one decimal and no `.0`: `2 h`, `0.5 h`.
pub(crate) fn hours_text(hours: f64) -> String {
    if (hours * 10.0).round() % 10.0 == 0.0 {
        format!("{} h", hours.round() as i64)
    } else {
        format!("{hours:.1} h")
    }
}

/// A percent, whole: `27 %`.
pub(crate) fn percent_text(percent: f64) -> String {
    format!("{} %", percent.round() as i64)
}

/// A date in local time: `12 Nov`.
pub(crate) fn date_text(t: DateTime<Utc>, offset: FixedOffset) -> String {
    let local = t.with_timezone(&offset);
    format!("{} {}", local.day(), local.format("%b"))
}

/// A stretch in which the account in use on a host was blocked, cut at every
/// change of the account in use, at the range's ends and at its `Unblocked`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedSpan {
    pub host: String,
    pub account: AccountRef,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// The working hours of the span (the rhythm's hours count).
    pub hours: f64,
    /// The part of `hours` in which another auto-burn account was usable and
    /// had a finished weekly window running that ended with something expired.
    pub avoidable_hours: f64,
}

/// `None` when the range holds a reading; else what to show instead of a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Empty {
    pub recording_since: Option<DateTime<Utc>>,
}

/// The blocked time of the chosen hosts in the range. Only the time the blocked
/// account was the one in use counts, and only in the working hours of the
/// rhythm of the four weeks before the range's end. A host's spans come only from
/// the blocks that host recorded and the account that host had in use, so two
/// hosts blocked at the same time each have their span.
pub fn blocked_spans(ctx: &Ctx, range: &Range, q: &Query) -> Result<Vec<BlockedSpan>, StatsError> {
    let events = ctx.history.read::<Event>(&Range::ALL, &Hosts::All)?;
    let intervals = block_intervals(&events.records, range.to);
    let mut weekly = Vec::new();
    for hosted in ctx.history.finished_windows(&Range::ALL, &Hosts::All)? {
        if hosted.record.k == LimitKind::WeeklyAll {
            weekly.push(hosted);
        }
    }
    let rhythm = working_rhythm(ctx, range)?;
    let timelines = rates::timelines(&events.records);

    let mut spans = Vec::new();
    for host in hosts_of(q, &events.records) {
        let Some(timeline) = timelines.get(&host) else {
            continue;
        };
        for (interval_host, account, blocked_from, blocked_to) in &intervals {
            if interval_host != &host {
                continue;
            }
            if !q.accounts.is_empty() {
                let Some(stored) = stored_account(ctx, account) else {
                    continue;
                };
                if !q.accounts.iter().any(|id| id == &stored.id) {
                    continue;
                }
            }
            let start = (*blocked_from).max(range.from);
            let end = (*blocked_to).min(range.to);
            if start >= end {
                continue;
            }
            let mut points = vec![start];
            points.extend(timeline.changes_between(start, end));
            points.push(end);
            points.sort_unstable();
            points.dedup();
            for pair in points.windows(2) {
                let (from, to) = (pair[0], pair[1]);
                if from >= to || timeline.at(from) != Some(account) {
                    continue;
                }
                spans.push(BlockedSpan {
                    host: host.clone(),
                    account: account.clone(),
                    from,
                    to,
                    hours: working_hours(from, to, &rhythm, ctx.offset),
                    avoidable_hours: avoidable_hours(
                        ctx,
                        &host,
                        account,
                        Range { from, to },
                        &intervals,
                        &weekly,
                        &rhythm,
                    ),
                });
            }
        }
    }
    spans.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| a.account.cmp(&b.account))
    });
    Ok(spans)
}

/// Whether the history holds no record in the range; then the page says since
/// when seatswap has been recording.
pub fn empty(ctx: &Ctx, range: &Range) -> Result<Option<Empty>, StatsError> {
    if has_record(ctx, range)? {
        return Ok(None);
    }
    Ok(Some(Empty {
        recording_since: first_record(ctx)?,
    }))
}

fn earliest<R: Record>(
    history: &HistoryDir,
    range: &Range,
) -> Result<Option<DateTime<Utc>>, HistoryError> {
    let read = history.read::<R>(range, &Hosts::All)?;
    Ok(read.records.first().map(|hosted| hosted.record.at()))
}

fn first_record(ctx: &Ctx) -> Result<Option<DateTime<Utc>>, StatsError> {
    let range = Range::ALL;
    let found = [
        earliest::<Sample>(ctx.history, &range)?,
        earliest::<WindowRecord>(ctx.history, &range)?,
        earliest::<Event>(ctx.history, &range)?,
        earliest::<TokenRecord>(ctx.history, &range)?,
        earliest::<SessionRecord>(ctx.history, &range)?,
        earliest::<UpRecord>(ctx.history, &range)?,
        earliest::<ProjectsLogRecord>(ctx.history, &range)?,
    ];
    Ok(found.into_iter().flatten().min())
}

fn any_record<R: Record>(history: &HistoryDir, range: &Range) -> Result<bool, HistoryError> {
    Ok(!history.read::<R>(range, &Hosts::All)?.records.is_empty())
}

fn has_record(ctx: &Ctx, range: &Range) -> Result<bool, StatsError> {
    Ok(any_record::<Sample>(ctx.history, range)?
        || any_record::<WindowRecord>(ctx.history, range)?
        || any_record::<Event>(ctx.history, range)?
        || any_record::<TokenRecord>(ctx.history, range)?
        || any_record::<SessionRecord>(ctx.history, range)?
        || any_record::<UpRecord>(ctx.history, range)?
        || any_record::<ProjectsLogRecord>(ctx.history, range)?)
}

fn stored_account<'a>(ctx: &Ctx<'a>, reference: &AccountRef) -> Option<&'a Account> {
    ctx.accounts.iter().find(|account| {
        account.account_uuid == reference.acc && account.organization_uuid == reference.org
    })
}

fn hosts_of(q: &Query, events: &[Hosted<Event>]) -> Vec<String> {
    match &q.host {
        Some(host) => vec![host.clone()],
        None => {
            let mut hosts: Vec<String> = events.iter().map(|hosted| hosted.host.clone()).collect();
            hosts.sort();
            hosts.dedup();
            hosts
        }
    }
}

/// The working hours of the range: the hours of the week whose value in the
/// rhythm of the four weeks before `range.to` is above 0.
fn working_rhythm(ctx: &Ctx, range: &Range) -> Result<Rhythm, StatsError> {
    let from = range.to - Duration::weeks(RHYTHM_WEEKS);
    let read = ctx
        .history
        .read::<TokenRecord>(&Range { from, to: range.to }, &Hosts::All)?;
    Ok(rates::rhythm(&read.records, range.to, ctx.offset))
}

/// The hours of `[from, to)` that fall in a working hour.
fn working_hours(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    rhythm: &Rhythm,
    offset: FixedOffset,
) -> f64 {
    let mut hours = 0.0;
    let mut cursor = from;
    while cursor < to {
        let local = cursor.with_timezone(&offset);
        let into_hour = Duration::minutes(i64::from(local.minute()))
            + Duration::seconds(i64::from(local.second()))
            + Duration::nanoseconds(i64::from(local.nanosecond()));
        let next = (cursor - into_hour + Duration::hours(1)).min(to);
        if rhythm.at(cursor, offset) > 0.0 {
            hours += seconds_between(cursor, next) / 3600.0;
        }
        cursor = next;
    }
    hours
}

fn seconds_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    let span = to.signed_duration_since(from);
    span.num_seconds() as f64 + span.subsec_nanos() as f64 / 1e9
}

type BlockedInterval = (String, AccountRef, DateTime<Utc>, DateTime<Utc>);
/// A block belongs to one host and one account.
type BlockKey = (String, AccountRef);
/// The limits a block holds open, by API name and model.
type OpenLimits = BTreeSet<(String, Option<String>)>;

/// The stretches in which an account was blocked on one host, any limit kind. A
/// block that is still open at the last event counts up to `open_to` (the
/// range's end).
fn block_intervals(events: &[Hosted<Event>], open_to: DateTime<Utc>) -> Vec<BlockedInterval> {
    let mut active: BTreeMap<BlockKey, OpenLimits> = BTreeMap::new();
    let mut opened: BTreeMap<BlockKey, DateTime<Utc>> = BTreeMap::new();
    let mut intervals = Vec::new();
    for hosted in events {
        match &hosted.record.kind {
            EventKind::Blocked { account, k, m } => {
                let key = (hosted.host.clone(), account.clone());
                let blocked = active.entry(key.clone()).or_default();
                let was_empty = blocked.is_empty();
                blocked.insert((k.as_api().to_string(), m.clone()));
                if was_empty {
                    opened.insert(key, hosted.record.t);
                }
            }
            EventKind::Unblocked { account, k, m } => {
                let key = (hosted.host.clone(), account.clone());
                let Some(blocked) = active.get_mut(&key) else {
                    continue;
                };
                blocked.remove(&(k.as_api().to_string(), m.clone()));
                if blocked.is_empty() {
                    active.remove(&key);
                    if let Some(start) = opened.remove(&key) {
                        if start < hosted.record.t {
                            intervals.push((key.0, key.1, start, hosted.record.t));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (key, blocked) in active {
        if blocked.is_empty() {
            continue;
        }
        if let Some(start) = opened.remove(&key) {
            if start < open_to {
                intervals.push((key.0, key.1, start, open_to));
            }
        }
    }
    intervals.sort_by(|a, b| {
        a.2.cmp(&b.2)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    intervals
}

fn blocked_at(
    intervals: &[BlockedInterval],
    host: &str,
    account: &AccountRef,
    t: DateTime<Utc>,
) -> bool {
    intervals.iter().any(|(blocked_host, blocked, from, to)| {
        blocked_host == host && blocked == account && *from <= t && t < *to
    })
}

/// The working hours of `[from, to)` in which another account auto-burn may use
/// was not blocked on `host` and had a finished weekly window running that ended
/// with something expired.
fn avoidable_hours(
    ctx: &Ctx,
    host: &str,
    blocked: &AccountRef,
    span: Range,
    intervals: &[BlockedInterval],
    weekly: &[Hosted<WindowRecord>],
    rhythm: &Rhythm,
) -> f64 {
    let (from, to) = (span.from, span.to);
    let has_other = ctx
        .accounts
        .iter()
        .any(|account| account.auto_burn && AccountRef::of(account) != *blocked);
    if !has_other {
        return 0.0;
    }

    let mut points = vec![from, to];
    for (blocked_host, account, start, end) in intervals {
        if blocked_host != host || account == blocked {
            continue;
        }
        for at in [*start, *end] {
            if at > from && at < to {
                points.push(at);
            }
        }
    }
    for hosted in weekly {
        if hosted.record.account == *blocked {
            continue;
        }
        if let Some(start) = hosted.record.start {
            for at in [start, hosted.record.end] {
                if at > from && at < to {
                    points.push(at);
                }
            }
        }
    }
    points.sort_unstable();
    points.dedup();

    let mut hours = 0.0;
    for pair in points.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        if avoidable_at(ctx, host, blocked, start, intervals, weekly) {
            hours += working_hours(start, end, rhythm, ctx.offset);
        }
    }
    hours
}

fn avoidable_at(
    ctx: &Ctx,
    host: &str,
    blocked: &AccountRef,
    t: DateTime<Utc>,
    intervals: &[BlockedInterval],
    weekly: &[Hosted<WindowRecord>],
) -> bool {
    ctx.accounts
        .iter()
        .filter(|account| account.auto_burn)
        .any(|account| {
            let other = AccountRef::of(account);
            other != *blocked
                && !blocked_at(intervals, host, &other, t)
                && weekly.iter().any(|hosted| {
                    hosted.record.account == other
                        && hosted
                            .record
                            .start
                            .is_some_and(|start| start <= t && t < hosted.record.end)
                        && 100.0 - hosted.record.final_percent > 0.0
                })
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::fs;

    use tempfile::{tempdir, TempDir};

    use crate::history::{By, LimitSample, Source};
    use crate::money::{price_key, ModelPrice, PricePoint};
    use crate::project::ProjectKey;
    use crate::store::{LoginState, OwnRate};
    use crate::types::OAuthAccount;

    const LB_ID: &str = "7c1e4b2a9d3f4e6ab8c52f0a1d7e9b34";
    const LB_ACC: &str = "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04";
    const LB_ORG: &str = "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20";
    const AC_ID: &str = "2b7d9c1e5a4f4c3b9e8d7f6a5b4c3d2e";
    const AC_ACC: &str = "3b8d4c1f-7a2e-4d69-8f51-0c9a6e3b2d17";
    const AC_ORG: &str = "d4f6a8c2-0e71-4b39-a5c8-7e2d1f6b3a90";

    /// The fixture every test of this plan uses: host `atlas`, local time UTC,
    /// "now" = Thu 19 Nov 2026 10:00 (step-c3.md, "The fixture every test of
    /// this plan uses").
    pub(crate) struct Tiny {
        pub(crate) dir: TempDir,
        pub(crate) history: HistoryDir,
        pub(crate) accounts: Vec<Account>,
        pub(crate) settings: Settings,
        pub(crate) prices: Prices,
        pub(crate) fx: Fx,
        pub(crate) projects: Projects,
        pub(crate) now: DateTime<Utc>,
        pub(crate) offset: FixedOffset,
        pub(crate) windows: Vec<WindowRecord>,
    }

    impl Tiny {
        pub(crate) fn ctx(&self) -> Ctx<'_> {
            Ctx {
                history: &self.history,
                accounts: &self.accounts,
                settings: &self.settings,
                prices: &self.prices,
                fx: &self.fx,
                projects: &self.projects,
                now: self.now,
                offset: self.offset,
            }
        }

        pub(crate) fn lb(&self) -> &Account {
            &self.accounts[0]
        }

        pub(crate) fn ac(&self) -> &Account {
            &self.accounts[1]
        }

        pub(crate) fn lb_ref(&self) -> AccountRef {
            AccountRef::of(self.lb())
        }

        pub(crate) fn ac_ref(&self) -> AccountRef {
            AccountRef::of(self.ac())
        }
    }

    pub(crate) fn utc(text: &str) -> DateTime<Utc> {
        text.parse::<DateTime<Utc>>().expect("test date")
    }

    pub(crate) fn all() -> Query {
        Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: None,
        }
    }

    fn two_weeks() -> Range {
        Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.001
    }

    fn account(
        id: &str,
        acc: &str,
        org: &str,
        abbr: &str,
        plan: &str,
        price: f64,
        color: &str,
    ) -> Account {
        Account {
            id: id.to_string(),
            account_uuid: acc.to_string(),
            organization_uuid: org.to_string(),
            label: abbr.to_string(),
            abbr: abbr.to_string(),
            color: color.to_string(),
            email: Some(format!("{}@mail.example", abbr.to_ascii_lowercase())),
            org_name: None,
            plan: Some(plan.to_string()),
            price: Some(price),
            price_currency: Some("EUR".to_string()),
            oauth_account: OAuthAccount(serde_json::Map::new()),
            added_at: utc("2026-11-01T00:00:00Z"),
            last_used_at: None,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            auto_burn: true,
        }
    }

    fn weekly_window(
        account: &AccountRef,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        final_percent: f64,
        plan: &str,
    ) -> WindowRecord {
        WindowRecord {
            account: account.clone(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(start),
            end,
            final_percent,
            lost: Some(100.0 - final_percent),
            last_sample: end - Duration::minutes(2),
            plan: Some(plan.to_string()),
            uncertain: false,
        }
    }

    fn token(t: &str, session: &str, cwd: &str, project: &ProjectKey, out: u64) -> TokenRecord {
        TokenRecord {
            t: utc(t),
            session: session.to_string(),
            cwd: cwd.to_string(),
            project: project.clone(),
            model: "claude-opus-5-5".to_string(),
            input: 0,
            out,
            cw: 0,
            cr: 0,
            answers: 1,
            agent_answers: 0,
        }
    }

    fn session(
        session: &str,
        cwd: &str,
        project: &ProjectKey,
        first: &str,
        last: &str,
        closed_at: &str,
    ) -> SessionRecord {
        SessionRecord {
            t: utc(closed_at),
            session: session.to_string(),
            cwd: cwd.to_string(),
            project: project.clone(),
            entrypoint: Some("cli".to_string()),
            first: utc(first),
            last: utc(last),
            agents_started: 0,
            agents_finished: 0,
            closed: true,
        }
    }

    pub(crate) fn tiny() -> Tiny {
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let now = utc("2026-11-19T10:00:00Z");
        let offset = FixedOffset::east_opt(0).expect("UTC offset");

        let accounts = vec![
            account(LB_ID, LB_ACC, LB_ORG, "LB", "Max", 180.0, "#3b82f6"),
            account(AC_ID, AC_ACC, AC_ORG, "AC", "Team 5x", 140.0, "#f59e0b"),
        ];
        let lb = AccountRef::of(&accounts[0]);
        let ac = AccountRef::of(&accounts[1]);

        let atlas = ProjectKey::Repo("github.com/sam/atlas".to_string());
        let acme_kb = ProjectKey::Repo("github.com/acme/acme-kb".to_string());
        let engine = "/home/sam/projects/atlas/engine";
        let frontend = "/home/sam/projects/atlas/frontend";
        let acme_folder = "/home/sam/projects/acme-kb";

        let windows = vec![
            weekly_window(
                &ac,
                utc("2026-11-02T02:00:00Z"),
                utc("2026-11-09T02:00:00Z"),
                50.0,
                "Team 5x",
            ),
            weekly_window(
                &lb,
                utc("2026-11-02T17:00:00Z"),
                utc("2026-11-09T17:00:00Z"),
                100.0,
                "Max",
            ),
            weekly_window(
                &ac,
                utc("2026-11-09T02:00:00Z"),
                utc("2026-11-16T02:00:00Z"),
                90.0,
                "Team 5x",
            ),
            weekly_window(
                &lb,
                utc("2026-11-09T17:00:00Z"),
                utc("2026-11-16T17:00:00Z"),
                80.0,
                "Max",
            ),
        ];

        let events = vec![
            Event {
                t: utc("2026-11-02T08:00:00Z"),
                kind: EventKind::Live {
                    account: Some(lb.clone()),
                },
            },
            Event {
                t: utc("2026-11-07T10:00:00Z"),
                kind: EventKind::Blocked {
                    account: lb.clone(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                },
            },
            Event {
                t: utc("2026-11-07T12:00:00Z"),
                kind: EventKind::Switch {
                    from: Some(lb.clone()),
                    to: ac.clone(),
                    by: By::User,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: None,
                },
            },
            Event {
                t: utc("2026-11-09T17:00:00Z"),
                kind: EventKind::Unblocked {
                    account: lb.clone(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                },
            },
            Event {
                t: utc("2026-11-10T09:00:00Z"),
                kind: EventKind::Switch {
                    from: Some(ac.clone()),
                    to: lb.clone(),
                    by: By::Auto,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: Some(120),
                },
            },
            Event {
                t: utc("2026-11-12T15:00:00Z"),
                kind: EventKind::Switch {
                    from: Some(lb.clone()),
                    to: ac.clone(),
                    by: By::Agent,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: None,
                },
            },
            Event {
                t: utc("2026-11-13T02:00:00Z"),
                kind: EventKind::Blocked {
                    account: ac.clone(),
                    k: LimitKind::Session,
                    m: None,
                },
            },
            Event {
                t: utc("2026-11-13T03:00:00Z"),
                kind: EventKind::Unblocked {
                    account: ac.clone(),
                    k: LimitKind::Session,
                    m: None,
                },
            },
            Event {
                t: utc("2026-11-14T10:30:00Z"),
                kind: EventKind::Waited {
                    session: "s3".to_string(),
                    project: atlas.clone(),
                    secs: 600,
                },
            },
        ];

        let tokens = vec![
            token("2026-11-03T09:00:00Z", "s1", engine, &atlas, 1_000_000),
            token("2026-11-03T10:00:00Z", "s1", engine, &atlas, 1_000_000),
            token(
                "2026-11-08T09:00:00Z",
                "s2",
                acme_folder,
                &acme_kb,
                1_000_000,
            ),
            token("2026-11-11T09:00:00Z", "s4", engine, &atlas, 1_000_000),
            token("2026-11-14T10:00:00Z", "s3", frontend, &atlas, 2_000_000),
            token("2026-11-14T11:00:00Z", "s3", frontend, &atlas, 1_000_000),
        ];

        let sessions = vec![
            session(
                "s1",
                engine,
                &atlas,
                "2026-11-03T09:00:00Z",
                "2026-11-03T11:00:00Z",
                "2026-11-03T11:05:00Z",
            ),
            session(
                "s2",
                acme_folder,
                &acme_kb,
                "2026-11-08T09:00:00Z",
                "2026-11-08T10:00:00Z",
                "2026-11-08T10:05:00Z",
            ),
            session(
                "s4",
                engine,
                &atlas,
                "2026-11-11T09:00:00Z",
                "2026-11-11T10:00:00Z",
                "2026-11-11T10:05:00Z",
            ),
            session(
                "s3",
                frontend,
                &atlas,
                "2026-11-14T10:00:00Z",
                "2026-11-14T12:00:00Z",
                "2026-11-14T12:05:00Z",
            ),
        ];

        let samples = vec![
            Sample {
                t: now - Duration::minutes(1),
                account: lb.clone(),
                src: Source::Poll,
                l: vec![LimitSample {
                    k: LimitKind::WeeklyAll,
                    m: None,
                    p: 30.0,
                    r: Some(utc("2026-11-23T17:00:00Z")),
                    b: true,
                }],
            },
            Sample {
                t: now - Duration::minutes(1),
                account: ac.clone(),
                src: Source::Poll,
                l: vec![LimitSample {
                    k: LimitKind::WeeklyAll,
                    m: None,
                    p: 40.0,
                    r: Some(utc("2026-11-23T02:00:00Z")),
                    b: true,
                }],
            },
        ];

        let mut ups = Vec::new();
        let mut at = utc("2026-11-03T08:00:00Z");
        while at <= utc("2026-11-03T12:00:00Z") {
            ups.push(UpRecord { t: at });
            at += Duration::minutes(5);
        }

        history.append_many(&windows).expect("windows");
        history.append_many(&events).expect("events");
        history.append_many(&tokens).expect("tokens");
        history.append_many(&sessions).expect("sessions");
        history.append_many(&samples).expect("samples");
        history.append_many(&ups).expect("up lines");

        let mut prices = Prices::default();
        prices.models.insert(
            price_key("anthropic", "claude-opus-5-5"),
            vec![PricePoint {
                from: utc("2026-01-01T00:00:00Z"),
                price: ModelPrice {
                    input: 4.0,
                    output: 20.0,
                    cache_write: 5.0,
                    cache_read: 0.2,
                    tiered: false,
                },
            }],
        );
        let fx = Fx {
            currency: "EUR".to_string(),
            ..Fx::default()
        };
        let settings = Settings {
            currency: Some("EUR".to_string()),
            usd_rate: Some(OwnRate {
                currency: "EUR".to_string(),
                value: 1.0,
            }),
            ..Settings::default()
        };

        let log: Vec<Hosted<ProjectsLogRecord>> = Vec::new();
        let hosted_sessions: Vec<Hosted<SessionRecord>> = sessions
            .iter()
            .cloned()
            .map(|record| Hosted {
                host: "atlas".to_string(),
                record,
            })
            .collect();
        let projects = Projects::resolve(&log, &hosted_sessions, &accounts);

        Tiny {
            dir,
            history,
            accounts,
            settings,
            prices,
            fx,
            projects,
            now,
            offset,
            windows,
        }
    }

    #[test]
    fn range_keys() {
        let tiny = tiny();
        let ctx = tiny.ctx();

        let this_week = range_of(RangeKey::ThisWeek, &ctx).unwrap();
        assert_eq!(this_week.from, utc("2026-11-16T00:00:00Z"));
        assert_eq!(this_week.to, tiny.now);

        let last_week = range_of(RangeKey::LastWeek, &ctx).unwrap();
        assert_eq!(last_week.from, utc("2026-11-09T00:00:00Z"));
        assert_eq!(last_week.to, utc("2026-11-16T00:00:00Z"));

        let days30 = range_of(RangeKey::Days30, &ctx).unwrap();
        assert_eq!(days30.from, utc("2026-10-19T00:00:00Z"));
        assert_eq!(days30.to, tiny.now);
    }

    #[test]
    fn a_windows_price_is_the_price_at_its_midpoint() {
        let tiny = tiny();
        tiny.history
            .append_many(&[
                Event {
                    t: utc("2026-11-06T00:00:00Z"),
                    kind: EventKind::PriceChanged {
                        account: tiny.lb_ref(),
                        amount: Some(180.0),
                        currency: Some("EUR".to_string()),
                    },
                },
                Event {
                    t: utc("2026-11-06T10:00:00Z"),
                    kind: EventKind::PriceChanged {
                        account: tiny.lb_ref(),
                        amount: Some(200.0),
                        currency: Some("EUR".to_string()),
                    },
                },
            ])
            .expect("LB's price changes");

        let weeks = account_weeks(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let price_of = |end: &str| {
            weeks
                .iter()
                .find(|week| week.account == tiny.lb_ref() && week.window.end == utc(end))
                .expect("LB's window")
                .price
        };
        assert_eq!(price_of("2026-11-09T17:00:00Z"), Some(180.0));
        assert_eq!(price_of("2026-11-16T17:00:00Z"), Some(200.0));
    }

    #[test]
    fn a_price_in_another_currency_is_missing() {
        let mut tiny = tiny();
        tiny.accounts[0].price_currency = Some("USD".to_string());

        let weeks = account_weeks(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let lb = weeks
            .iter()
            .find(|week| {
                week.account == tiny.lb_ref() && week.window.end == utc("2026-11-09T17:00:00Z")
            })
            .expect("LB's window ending 9 Nov");
        assert_eq!(lb.price, None);
        assert_eq!(
            lb.price_missing,
            Some(Missing::PriceInOtherCurrency {
                currency: "USD".to_string()
            })
        );
    }

    #[test]
    fn windows_go_to_the_week_of_their_midpoint() {
        let tiny = tiny();
        let ctx = tiny.ctx();

        let weeks = account_weeks(&ctx, &two_weeks(), &all()).unwrap();
        assert_eq!(weeks.len(), 4);
        let first = weeks
            .iter()
            .find(|week| {
                week.account == tiny.lb_ref() && week.window.end == utc("2026-11-09T17:00:00Z")
            })
            .expect("LB's window ending 9 Nov");
        assert_eq!(first.week, utc("2026-11-02T00:00:00Z"));
    }

    #[test]
    fn a_window_recorded_by_two_hosts_counts_once() {
        let tiny = tiny();
        let mut older = tiny
            .windows
            .iter()
            .find(|window| {
                window.account == tiny.lb_ref() && window.end == utc("2026-11-09T17:00:00Z")
            })
            .expect("LB's window ending 9 Nov")
            .clone();
        older.last_sample = utc("2026-11-09T16:00:00Z");
        older.final_percent = 77.0;
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&older).expect("the vega copy");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let weeks = account_weeks(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(weeks.len(), 4);
        let first = weeks
            .iter()
            .find(|week| {
                week.account == tiny.lb_ref() && week.window.end == utc("2026-11-09T17:00:00Z")
            })
            .expect("LB's window ending 9 Nov");
        assert_eq!(first.window.final_percent, 100.0);
        assert_eq!(first.window.last_sample, utc("2026-11-09T16:58:00Z"));
    }

    #[test]
    fn blocked_time_counts_only_working_hours_of_the_active_account() {
        let tiny = tiny();
        let spans = blocked_spans(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(spans.len(), 2);

        let lb = spans
            .iter()
            .find(|span| span.account == tiny.lb_ref())
            .expect("LB's blocked span");
        assert_eq!(lb.from, utc("2026-11-07T10:00:00Z"));
        assert_eq!(lb.to, utc("2026-11-07T12:00:00Z"));
        assert!(close(lb.hours, 2.0), "hours {}", lb.hours);
        assert!(
            close(lb.avoidable_hours, 2.0),
            "avoidable {}",
            lb.avoidable_hours
        );

        let ac = spans
            .iter()
            .find(|span| span.account == tiny.ac_ref())
            .expect("AC's blocked span");
        assert_eq!(ac.from, utc("2026-11-13T02:00:00Z"));
        assert_eq!(ac.to, utc("2026-11-13T03:00:00Z"));
        assert!(close(ac.hours, 0.0), "hours {}", ac.hours);
    }

    #[test]
    fn blocked_while_not_the_active_account_does_not_count() {
        let tiny = tiny();
        let spans = blocked_spans(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let lb: Vec<&BlockedSpan> = spans
            .iter()
            .filter(|span| span.account == tiny.lb_ref())
            .collect();
        assert_eq!(lb.len(), 1);
        assert_eq!(lb[0].to, utc("2026-11-07T12:00:00Z"));
        assert!(close(lb[0].hours, 2.0), "hours {}", lb[0].hours);
    }

    #[test]
    fn an_empty_range_says_since_when() {
        let tiny = tiny();
        let ctx = tiny.ctx();
        let october = Range {
            from: utc("2026-10-01T00:00:00Z"),
            to: utc("2026-11-01T00:00:00Z"),
        };
        assert_eq!(
            empty(&ctx, &october).unwrap(),
            Some(Empty {
                recording_since: Some(utc("2026-11-02T08:00:00Z")),
            })
        );

        let dir = tempdir().expect("an empty history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let empty_ctx = Ctx {
            history: &history,
            ..ctx
        };
        assert_eq!(
            empty(&empty_ctx, &october).unwrap(),
            Some(Empty {
                recording_since: None,
            })
        );
    }

    #[test]
    fn all_on_a_new_history_is_this_week() {
        let tiny = tiny();
        let dir = tempdir().expect("an empty history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let range = range_of(RangeKey::All, &ctx).unwrap();
        assert_eq!(range.from, utc("2026-11-16T00:00:00Z"));
        assert_eq!(range.to, tiny.now);
        assert_eq!(
            empty(&ctx, &range).unwrap(),
            Some(Empty {
                recording_since: None,
            })
        );
    }

    #[test]
    fn the_account_filter_applies() {
        let tiny = tiny();
        let query = Query {
            range: RangeKey::All,
            accounts: vec![tiny.lb().id.clone()],
            host: None,
        };
        let weeks = account_weeks(&tiny.ctx(), &two_weeks(), &query).unwrap();
        assert_eq!(weeks.len(), 2);
        assert!(weeks.iter().all(|week| week.account == tiny.lb_ref()));
    }

    /// A block is a record of one host: atlas blocked LB at 10:00 and
    /// unblocked it on 9 Nov, vega blocked it at 11:00 and unblocked it at
    /// 13:00. Every span must lie inside the block of its own host.
    #[test]
    fn a_block_belongs_to_the_host_that_recorded_it() {
        let tiny = tiny();
        let lb = tiny.lb_ref();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append_many(&[
            Event {
                t: utc("2026-11-02T08:00:00Z"),
                kind: EventKind::Live {
                    account: Some(lb.clone()),
                },
            },
            Event {
                t: utc("2026-11-07T11:00:00Z"),
                kind: EventKind::Blocked {
                    account: lb.clone(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                },
            },
            Event {
                t: utc("2026-11-07T13:00:00Z"),
                kind: EventKind::Unblocked {
                    account: lb.clone(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                },
            },
        ])
        .expect("the vega records");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let spans = blocked_spans(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let vega_spans: Vec<&BlockedSpan> =
            spans.iter().filter(|span| span.host == "vega").collect();
        assert_eq!(vega_spans.len(), 1, "vega's spans: {vega_spans:#?}");
        assert_eq!(vega_spans[0].from, utc("2026-11-07T11:00:00Z"));
        assert_eq!(vega_spans[0].to, utc("2026-11-07T13:00:00Z"));
        assert!(
            close(vega_spans[0].hours, 1.0),
            "hours {}",
            vega_spans[0].hours
        );
    }

    /// The week starts at Monday 00:00 in the local time of the host, also
    /// when that is not midnight in UTC.
    #[test]
    fn the_range_starts_at_the_local_monday() {
        let tiny = tiny();
        let ctx = Ctx {
            offset: FixedOffset::east_opt(3600).expect("+01:00"),
            ..tiny.ctx()
        };
        let this_week = range_of(RangeKey::ThisWeek, &ctx).unwrap();
        assert_eq!(this_week.from, utc("2026-11-15T23:00:00Z"));
        assert_eq!(this_week.to, tiny.now);

        let last_week = range_of(RangeKey::LastWeek, &ctx).unwrap();
        assert_eq!(last_week.from, utc("2026-11-08T23:00:00Z"));
        assert_eq!(last_week.to, utc("2026-11-15T23:00:00Z"));

        let days30 = range_of(RangeKey::Days30, &ctx).unwrap();
        assert_eq!(days30.from, utc("2026-10-18T23:00:00Z"));
    }

    /// The hours are counted by the minute: 30 working minutes are half an
    /// hour.
    #[test]
    fn blocked_hours_are_counted_by_the_minute() {
        let tiny = tiny();
        // A range that starts inside LB's block, half an hour before the
        // switch that ended it.
        let half_hour = Range {
            from: utc("2026-11-07T11:30:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };
        let spans = blocked_spans(&tiny.ctx(), &half_hour, &all()).unwrap();
        let lb: Vec<&BlockedSpan> = spans
            .iter()
            .filter(|span| span.account == tiny.lb_ref())
            .collect();
        assert_eq!(lb.len(), 1, "LB's spans: {lb:#?}");
        assert_eq!(lb[0].from, utc("2026-11-07T11:30:00Z"));
        assert_eq!(lb[0].to, utc("2026-11-07T12:00:00Z"));
        assert!(close(lb[0].hours, 0.5), "hours {}", lb[0].hours);
    }
}
