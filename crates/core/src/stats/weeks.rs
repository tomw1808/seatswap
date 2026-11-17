//! The Weeks page (spec §3.2): every weekly window as a grid, the last two
//! weeks as curves, the summary of one calendar week and the detail of one
//! window. Everything here only reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Serialize;

use crate::api::LimitKind;
use crate::burn::Rhythm;
use crate::history::{
    same_reset, AccountRef, By, Event, EventKind, Hosted, Hosts, Range, Sample, WindowRecord,
};
use crate::live_sessions::Values;
use crate::money::{Company, Valuer};
use crate::projects::ProjectId;
use crate::transcripts::Tokens;

use super::{
    account_abbr, account_chosen, account_id, account_position, account_weeks_so_far,
    account_weeks_valued, add_opt_value, add_value, api_shares, blocked_by_week, blocked_spans,
    hours_text, keep_warnings, lost_percent, money_at, money_of, money_text, next_week_start,
    offered_finished_money, offered_money, percent_of_offered, percent_text, previous_week_start,
    range_of, record_chosen, running_elsewhere, switch_counts, week_columns, week_lost_percent,
    week_start, window_midpoint, window_used, working_hours, working_rhythm, AccountWeek, Ctx,
    Query, RangeKey, StatsError, SwitchCounts,
};

/// The Weeks page: the grid of account weeks and the curves of the last 14
/// days.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeksPage {
    /// The calendar weeks of the range, Monday 00:00 local, the same columns
    /// the Overview shows.
    pub week_starts: Vec<DateTime<Utc>>,
    /// One per `week_starts`: whether that column is the calendar week
    /// `Ctx::now` falls in. The page draws its "now" cell there.
    pub running: Vec<bool>,
    pub rows: Vec<WeeksRow>,
    /// The working hours blocked in each column's week; a week without a span
    /// is a measured 0.0.
    pub blocked: Vec<f64>,
    /// One curve per row: the weekly percent of the last 14 days.
    pub recent: Vec<Curve>,
    /// The instant the page was computed; the "now" mark of the recent curves
    /// sits there.
    pub now: DateTime<Utc>,
}

/// One account's row of the grid.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeksRow {
    pub id: String,
    pub plan: Option<String>,
    /// One cell per column; `None` when the account has no finished window of
    /// that week. Every window of the account week is kept in the cell's
    /// `parts`, oldest first; `WeekCell::end` is the last window's end.
    pub cells: Vec<Option<WeekCell>>,
    /// The newest reading of the account's weekly window that is still
    /// running ([`Valuer::running_windows`]), in percent; `None` without one.
    pub now_percent: Option<f64>,
    /// The calendar week that running window counts in: the week of its
    /// midpoint, which may be last week or next week. The page opens that
    /// week's summary from the row's "now" cell. `None` without one.
    pub now_week: Option<DateTime<Utc>>,
}

/// One finished account week as a grid cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekCell {
    pub end: DateTime<Utc>,
    /// The sum of the week's windows' use (an account week may hold several
    /// windows after a reset); it may pass 100.
    pub used: f64,
    /// One part per window of the account week, oldest first: each window's own
    /// use. Their sum is `used`. A window that ended below its start adds 0
    /// (the rule of `stats::window_used`; `account_weeks_valued` names it once
    /// in the page's warnings).
    pub parts: Vec<f64>,
    /// The end of each window of `parts`, in the same order: the page opens a
    /// window's own detail from its part.
    pub window_ends: Vec<DateTime<Utc>>,
    /// The share that expired: every window's `lost` percent, added.
    pub expired: f64,
    /// The money that expired; `None` without a price for the week.
    pub expired_money: Option<f64>,
    /// `used >= 100`.
    pub full: bool,
    pub uncertain: bool,
}

/// One account's weekly percent over time; a reset is a drop to 0.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Curve {
    pub id: String,
    pub points: Vec<(DateTime<Utc>, f64)>,
}

/// One calendar week over all accounts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekSummary {
    pub week: DateTime<Utc>,
    /// `used / offered × 100` of the priced account weeks, the same rule as
    /// Overview's Used share (a reset week offers each of its windows); `None`
    /// when the week has no priced finished window, with `money_missing` saying
    /// why.
    pub used_percent_of_paid: Option<f64>,
    /// Money used of the priced account weeks; `None` like
    /// `used_percent_of_paid`.
    pub used: Option<f64>,
    /// Money expired of the priced account weeks; `None` like
    /// `used_percent_of_paid`.
    pub expired: Option<f64>,
    /// Why `used_percent_of_paid`, `used` and `expired` are `None`. For the
    /// week that holds now without a window, the sentences of
    /// [`running_elsewhere`] say in which week the running windows count.
    pub money_missing: Option<String>,
    /// A window of the week still runs: its use is the use so far, and the
    /// week has no change against the week before yet.
    pub running: bool,
    /// The working hours blocked in the week.
    pub blocked_hours: f64,
    pub switches: SwitchCounts,
    /// The sum of every `by` in `switches`: the "N switches" sub-line.
    pub switches_total: u64,
    /// Each account week's use in percent, per account id, every account week
    /// of the week (priced or not). The page's per-account line uses this.
    pub per_account_percent: Vec<(String, f64)>,
    /// The expired share of the money the week's windows offered: `expired /
    /// offered × 100`, the same denominator as `used_percent_of_paid` and
    /// Overview's Expired share, over the finished windows' offer only (a
    /// running window has expired nothing yet); `None` when the week has no
    /// finished, priced window.
    pub expired_percent_of_paid: Option<f64>,
    /// The projects' share in % of the week's API value, the three largest
    /// then `other`; empty when `values.api` is `None`.
    pub top_projects: Vec<(String, f64)>,
    /// The tokens of the chosen records whose own time lies in the week.
    pub tokens: Tokens,
    /// The two values of the chosen records whose own time lies in the week,
    /// built with [`Values::of`]; a missing half names its reason and the API
    /// half names its currency.
    pub values: Values,
    /// The change of `used` against the week before, in percent; `None` when
    /// the week itself has no priced account week, is `running` (its use is
    /// not complete), or the week before has none.
    pub used_change_percent: Option<f64>,
    /// At most three sentences, in the order the plan gives them.
    pub conclusion: Vec<String>,
}

/// One finished weekly window, with what the page shows about it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDetail {
    pub id: String,
    /// The window's recorded start ([`WindowRecord::weekly_start`]): the reset's
    /// date when a reset began it, otherwise seven days before `end`.
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// The plan of the window's record; `None` when it carried none.
    pub plan: Option<String>,
    /// The money paid for one week at the window's price (monthly price ×
    /// 12 / 52), in the display currency; `None` without a price.
    pub weekly_price: Option<f64>,
    /// The window's final percent.
    pub used: f64,
    /// Money used; `None` without a price for the window.
    pub used_money: Option<f64>,
    /// The share that expired: this window's `lost` percent.
    pub expired: f64,
    /// Money expired; `None` without a price for the window.
    pub expired_money: Option<f64>,
    /// The earliest `Blocked { k: weekly_all }` event of the account inside
    /// the window, any host.
    pub reached_100_at: Option<DateTime<Utc>>,
    /// The working hours blocked from `reached_100_at` to the window's end;
    /// 0.0 when it never reached 100.
    pub blocked_hours_after: f64,
    /// The finished session windows of the account that ended inside the
    /// window with `final > 0`; `None` when it has no session window at all.
    pub session_windows_used: Option<u32>,
    /// How many 5 h session windows fit into the account's typical week: the
    /// working hours of the learned rhythm (the same `working_rhythm` figure the
    /// Forecast and Rhythm pages use) divided by 5 and rounded. The rhythm
    /// covers the four weeks before the window's end, not the Rhythm page's
    /// span. `None` when the account has no session limit or the rhythm knows
    /// no working hour.
    pub possible_session_windows: Option<u32>,
    /// `true` when `session_windows_used` is above
    /// `possible_session_windows`: the page then says the used windows are more
    /// than the usual working hours hold, instead of claiming fewer possible
    /// ones than used.
    pub used_more_than_possible: bool,
    /// One sentence about the window's reset, its start after a reset or an
    /// uncertain last reading; the detail card's Note row.
    pub note: Option<String>,
    pub switches_in_out: u32,
    pub switches_auto: u32,
    /// `end − lastSample` of the window.
    pub last_sample_before_end_secs: i64,
    pub uncertain: bool,
    /// The account's weekly samples inside the window that name its reset.
    pub curve: Vec<(DateTime<Utc>, f64)>,
}

/// The Weeks page of the range the query names.
pub fn weeks<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, q: &Query) -> Result<WeeksPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    weeks_in(ctx, &range, q)
}

/// The Weeks page of `range`; the page's [`weeks`] takes the range from its
/// [`Query`], the tests name one.
pub(crate) fn weeks_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
) -> Result<WeeksPage, StatsError> {
    let valuer = Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
        ctx.provider_subscriptions,
        ctx.warnings,
    )?;
    let finished = account_weeks_valued(ctx, range, q, &valuer)?;
    let spans = blocked_spans(ctx, range, q, Company::Anthropic)?;
    let rhythm = working_rhythm(ctx, range, Company::Anthropic)?;
    let blocked_weeks = blocked_by_week(&spans, &rhythm, &ctx.zone);
    let week_starts = week_columns(range, &ctx.zone);
    // The samples of the last 14 days and the newest one before now: one read
    // serves both the running percents and the curves.
    let samples = keep_warnings(ctx.warnings, || {
        ctx.history.read_claude::<Sample>(&Range::ALL, &Hosts::All)
    })?;
    let recent_from = ctx.now - Duration::days(14);
    // The running windows give each row its "now" cell and the week it opens.
    let running_windows = valuer.running_windows(ctx.now);

    let mut rows = Vec::new();
    let mut recent = Vec::new();
    for account in ctx.accounts {
        if !account_chosen(ctx, q, Some(&AccountRef::of(account))) {
            continue;
        }
        let reference = AccountRef::of(account);
        rows.push(row_for(
            account.id.clone(),
            account.plan.clone(),
            &reference,
            &week_starts,
            &finished,
            &running_windows,
            &ctx.zone,
        )?);
        recent.push(curve(
            account.id.clone(),
            &samples.records,
            &reference,
            recent_from,
            ctx.now,
        ));
    }

    // An account that is no longer stored keeps its row when it has a window
    // in the range: after the stored accounts, with the id `account_id` gives
    // it and the plan of its window record.
    let mut removed: BTreeMap<AccountRef, &AccountWeek> = BTreeMap::new();
    for week in &finished {
        if ctx
            .accounts
            .iter()
            .any(|account| AccountRef::of(account) == week.account)
        {
            continue;
        }
        removed
            .entry(week.account.clone())
            .and_modify(|latest| {
                if week.window.end > latest.window.end {
                    *latest = week;
                }
            })
            .or_insert(week);
    }
    for (reference, latest) in removed {
        let id = account_id(ctx, &reference);
        rows.push(row_for(
            id.clone(),
            latest.window.plan.clone(),
            &reference,
            &week_starts,
            &finished,
            &running_windows,
            &ctx.zone,
        )?);
        recent.push(curve(
            id,
            &samples.records,
            &reference,
            recent_from,
            ctx.now,
        ));
    }

    let blocked = week_starts
        .iter()
        .map(|week| blocked_weeks.get(week).copied().unwrap_or(0.0))
        .collect();
    let this_week = week_start(ctx.now, &ctx.zone);
    let running = week_starts.iter().map(|week| *week == this_week).collect();

    Ok(WeeksPage {
        week_starts,
        running,
        rows,
        blocked,
        recent,
        now: ctx.now,
    })
}

/// One row of the grid: the account's cell of every column, the cell of the
/// account week that `finished` holds for that week, and its running window's
/// newest percent and the week it counts in.
fn row_for<Tz: TimeZone + Copy>(
    id: String,
    plan: Option<String>,
    reference: &AccountRef,
    week_starts: &[DateTime<Utc>],
    finished: &[AccountWeek],
    running_windows: &[WindowRecord],
    zone: &Tz,
) -> Result<WeeksRow, StatsError> {
    let mut cells = Vec::with_capacity(week_starts.len());
    for start in week_starts {
        let week = finished
            .iter()
            .find(|week| &week.account == reference && week.week == *start);
        cells.push(match week {
            Some(week) => Some(week_cell(week)?),
            None => None,
        });
    }
    let running = running_windows
        .iter()
        .find(|window| &window.account == reference);
    Ok(WeeksRow {
        id,
        plan,
        cells,
        now_percent: running.map(|window| window.final_percent),
        now_week: running
            .and_then(window_midpoint)
            .map(|midpoint| week_start(midpoint, zone)),
    })
}

/// One account week as the grid shows it.
fn week_cell(week: &AccountWeek) -> Result<WeekCell, StatsError> {
    let money = money_of(week)?;
    // The windows of the account week, oldest first. A window that ended below
    // its start adds no use (`stats::window_used`, named once in the warnings by
    // `account_weeks_valued`), exactly as `week.used` counted it.
    let parts = week
        .windows
        .iter()
        .map(|window| window_used(window).unwrap_or(0.0))
        .collect();
    let window_ends = week.windows.iter().map(|window| window.end).collect();
    Ok(WeekCell {
        end: week.window.end,
        used: week.used,
        parts,
        window_ends,
        expired: week_lost_percent(week)?,
        expired_money: money.map(|money| money.expired),
        full: week.used >= 100.0,
        uncertain: week.window.uncertain,
    })
}

/// The account's weekly percent from `from` to `now`, a reset as a drop to 0.
/// A sample that names no reset is left out: the drops are the resets.
fn curve(
    id: String,
    samples: &[Hosted<Sample>],
    reference: &AccountRef,
    from: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Curve {
    let mut sampled: Vec<(DateTime<Utc>, f64, DateTime<Utc>)> = samples
        .iter()
        .filter(|hosted| &hosted.record.account == reference)
        .filter(|hosted| hosted.record.t >= from && hosted.record.t <= now)
        .filter_map(|hosted| {
            let limit = hosted.record.limit(&LimitKind::WeeklyAll)?;
            Some((hosted.record.t, limit.p, limit.r?))
        })
        .collect();
    sampled.sort_by_key(|(at, _, _)| *at);

    let mut points = Vec::with_capacity(sampled.len());
    for (index, (at, percent, reset)) in sampled.iter().enumerate() {
        if index > 0 {
            let (earlier_at, _, earlier_reset) = sampled[index - 1];
            if !same_reset(earlier_reset, *reset) {
                // The drop sits at the earlier sample's reset when that lies
                // between the two samples; otherwise it is a vertical drop at
                // the later sample's time.
                let drop_at = if earlier_at < earlier_reset && earlier_reset <= *at {
                    earlier_reset
                } else {
                    *at
                };
                points.push((drop_at, 0.0));
            }
        }
        points.push((*at, *percent));
    }
    Curve { id, points }
}

/// The summary of one calendar week, `[week, week + 7 d)`.
pub fn week_summary<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
    week: DateTime<Utc>,
) -> Result<WeekSummary, StatsError> {
    let range = range_of(q.range, ctx)?;
    let week_range = Range {
        from: week,
        to: next_week_start(week, &ctx.zone),
    };
    let previous = previous_week_start(week, &ctx.zone);
    // The summary needs the week before too, also when the page's range starts
    // later: the change against it is part of the summary.
    let reading = Range {
        from: range.from.min(previous),
        to: range.to.max(week_range.to),
    };
    let valuer = Valuer::build(
        ctx.history,
        &reading,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
        ctx.provider_subscriptions,
        ctx.warnings,
    )?;
    // Every window, the running ones with their use so far: the week that
    // holds a running window shows what it used up to now.
    let weeks = account_weeks_so_far(ctx, &reading, q, &valuer)?;

    let mut used = 0.0;
    let mut offered = 0.0;
    let mut offered_finished = 0.0;
    let mut expired = 0.0;
    let mut used_priced = false;
    let mut missing_reason: Option<String> = None;
    let mut running = false;
    let mut percent_by_account: BTreeMap<AccountRef, f64> = BTreeMap::new();
    for account_week in weeks
        .iter()
        .filter(|account_week| account_week.week == week)
    {
        running |= account_week.running;
        percent_by_account.insert(account_week.account.clone(), account_week.used);
        match money_of(account_week)? {
            Some(money) => {
                used += money.used;
                offered += offered_money(account_week, &money);
                offered_finished += offered_finished_money(account_week, &money);
                expired += money.expired;
                used_priced = true;
            }
            None if missing_reason.is_none() => {
                missing_reason = account_week.price_missing.as_ref().map(ToString::to_string);
            }
            None => {}
        }
    }
    let money_missing = if used_priced {
        None
    } else {
        // A week without any window says why: for the week that holds now, in
        // which week the running windows count.
        Some(missing_reason.unwrap_or_else(|| {
            let elsewhere = running_elsewhere(ctx, &week_range, q, &valuer);
            if elsewhere.is_empty() {
                "no priced weekly window in this week".to_string()
            } else {
                elsewhere.join(" ")
            }
        }))
    };

    let mut before = 0.0;
    let mut before_priced = false;
    for account_week in weeks
        .iter()
        .filter(|account_week| account_week.week == previous)
    {
        if let Some(money) = money_of(account_week)? {
            before += money.used;
            before_priced = true;
        }
    }

    // The hours of the week come from the spans of a range that covers both
    // the week and the page's range, split into weeks by the rhythm of the
    // page's range: a week before the range still shows the Overview's hours.
    let blocked_range = Range {
        from: range.from.min(week_range.from),
        to: range.to.max(week_range.to),
    };
    let spans = blocked_spans(ctx, &blocked_range, q, Company::Anthropic)?;
    let rhythm = working_rhythm(ctx, &range, Company::Anthropic)?;
    let blocked_hours = blocked_by_week(&spans, &rhythm, &ctx.zone)
        .get(&week)
        .copied()
        .unwrap_or(0.0);

    let events = keep_warnings(ctx.warnings, || {
        ctx.history.read_claude::<Event>(&Range::ALL, &Hosts::All)
    })?;
    let switches = switch_counts(&events.records, &week_range, q.host.as_deref());

    let token_read = keep_warnings(ctx.warnings, || {
        ctx.history
            .read::<crate::history::TokenRecord>(&week_range, &Hosts::All)
    })?;
    let mut tokens = Tokens::default();
    let mut values: Option<Values> = None;
    let mut by_project: BTreeMap<String, Values> = BTreeMap::new();
    for hosted in &token_read.records {
        if q.host.as_ref().is_some_and(|host| host != &hosted.host) {
            continue;
        }
        let record = &hosted.record;
        let value = valuer.value(&hosted.host, record);
        if !record_chosen(ctx, q, &value) {
            continue;
        }
        tokens += Tokens::from(record);
        let record_values = Values::of(std::slice::from_ref(&value), ctx.settings, ctx.fx, ctx.now);
        add_opt_value(&mut values, &record_values);
        let name = ctx
            .projects
            .of(&ProjectId::of(&record.project, &hosted.host))
            .name;
        add_value(&mut by_project, name, &record_values);
    }
    let values = values.unwrap_or_else(|| Values::sum(&[]));
    // A share of nothing is no share: a week without API value lists none.
    let top_projects = api_shares(by_project, &values);

    let mut per_account_percent: Vec<(String, f64)> = percent_by_account
        .iter()
        .map(|(reference, percent)| (account_id(ctx, reference), *percent))
        .collect();
    per_account_percent.sort_by(|a, b| {
        account_position(ctx, &a.0)
            .cmp(&account_position(ctx, &b.0))
            .then_with(|| a.0.cmp(&b.0))
    });

    let used_change_percent = if used_priced && !running && before_priced && before != 0.0 {
        Some((used - before) / before * 100.0)
    } else {
        None
    };
    let used_percent_of_paid = percent_of_offered(used, offered);
    // The same denominator as Overview's Expired share: the money the week's
    // finished windows offered, one week's price for each of them.
    let expired_percent_of_paid = percent_of_offered(expired, offered_finished);
    let switches_total = u64::from(switches.total());

    let mut conclusion = Vec::new();
    if blocked_hours <= 0.0 {
        conclusion.push("No hour blocked.".to_string());
    } else {
        conclusion.push(format!("{} blocked.", hours_text(blocked_hours)));
    }
    if let Some((share, account_week)) = most_expired(&weeks, week)? {
        let abbr = account_abbr(ctx, &account_week.account);
        if share < 10.0 {
            conclusion.push(format!(
                "{abbr} came closest to expiring unused: {} left at its reset.",
                percent_text(share)
            ));
        } else if let Some(currency) = ctx.settings.currency.as_deref() {
            if let Some(money) = money_of(account_week)? {
                conclusion.push(format!(
                    "{} of {abbr}'s week expired, {}.",
                    percent_text(share),
                    money_text(money.expired, currency)
                ));
            }
        }
    }
    let (waited, waited_secs) = waited_switches(&events.records, &week_range, q.host.as_deref());
    if waited > 0 {
        let minutes = (waited_secs as f64 / 60.0).round() as i64;
        conclusion.push(format!(
            "Auto-burn held back {waited} planned {}, {minutes} {} in total.",
            if waited == 1 { "switch" } else { "switches" },
            if minutes == 1 { "minute" } else { "minutes" }
        ));
    }

    Ok(WeekSummary {
        week,
        used_percent_of_paid,
        used: used_priced.then_some(used),
        expired: used_priced.then_some(expired),
        money_missing,
        running,
        blocked_hours,
        switches,
        switches_total,
        per_account_percent,
        expired_percent_of_paid,
        top_projects,
        tokens,
        values,
        used_change_percent,
        conclusion,
    })
}

/// The finished account week of `week` with the largest expired share among the
/// weeks whose expired money is known: a week without a price is left out, so
/// an unpriced provider week can never hide a priced week's sentence. A running
/// week has expired nothing yet and is not compared.
fn most_expired(
    weeks: &[AccountWeek],
    week: DateTime<Utc>,
) -> Result<Option<(f64, &AccountWeek)>, StatsError> {
    let mut largest: Option<(f64, &AccountWeek)> = None;
    for account_week in weeks.iter().filter(|account_week| {
        account_week.week == week && !account_week.running && account_week.price.is_some()
    }) {
        let share = week_lost_percent(account_week)?;
        if largest.is_none_or(|(best, _)| share > best) {
            largest = Some((share, account_week));
        }
    }
    Ok(largest)
}

/// The switches whose `waited_secs` is above zero in `range`: how many and
/// their seconds.
fn waited_switches(events: &[Hosted<Event>], range: &Range, host: Option<&str>) -> (u32, u64) {
    let mut waited = 0;
    let mut seconds = 0;
    for hosted in events {
        let at = hosted.record.t;
        if at < range.from || at >= range.to {
            continue;
        }
        if host.is_some_and(|host| host != hosted.host) {
            continue;
        }
        if let EventKind::Switch {
            waited_secs: Some(secs),
            ..
        } = &hosted.record.kind
        {
            if *secs > 0 {
                waited += 1;
                seconds += *secs;
            }
        }
    }
    (waited, seconds)
}

/// The length of a session window in hours, as the detail card's "5 h windows"
/// row names it.
const SESSION_WINDOW_HOURS: f64 = 5.0;

/// How many [`SESSION_WINDOW_HOURS`] windows fit into the typical week: the
/// working hours of `[from, to)` counted with [`working_hours`] over a seven-day
/// span, from the rhythm the [`working_rhythm`] figure learned over the four
/// weeks before the window's end (the same figure the Forecast and Rhythm pages
/// use, not the Rhythm page's span). `None` when the rhythm knows no working
/// hour.
fn possible_5h_windows<Tz: TimeZone + Copy>(
    rhythm: &Rhythm,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    zone: &Tz,
) -> Option<u32> {
    let working = working_hours(from, to, rhythm, zone);
    (working > 0.0).then(|| (working / SESSION_WINDOW_HOURS).round() as u32)
}

/// The detail card's Note: what the window's record says about its reset and
/// its expired share. Never a made-up history: every sentence names a field of
/// `window` (or its `expired` percent). `began_at_reset` is true when a reset
/// record of the same account and limit ends where this window starts;
/// `reset_model` names the model whose limit was given back together with the
/// week, when a scoped reset record says so.
fn window_note(
    window: &WindowRecord,
    expired: f64,
    began_at_reset: bool,
    reset_model: Option<&str>,
) -> Option<String> {
    let mut sentences = Vec::new();
    let late = window.end.signed_duration_since(window.last_sample) > Duration::hours(1);
    // A window after a reset whose first reading is missing has no `from`
    // (write.rs's `Opened::mark`): its use is counted from 0 %, which may be too
    // high. A normal window also has no `from`; a reset that began it (its
    // recorded start differs from end − 7 days, or a reset record ends there)
    // or a last reading too early to be the late-reading case marks the missing
    // first percent.
    let reset_started = began_at_reset
        || window
            .start
            .is_some_and(|start| start != window.end - Duration::days(7));
    let first_unknown = window.uncertain && window.from.is_none() && (reset_started || !late);
    if window.reset {
        let model = reset_model.or(window.m.as_deref());
        if let Some(model) = model {
            sentences.push(format!(
                "{model}'s limit was given back before its reset, so the use before it stays counted."
            ));
        } else {
            sentences.push(
                "The week's limit was given back before its reset, so the use before it stays counted."
                    .to_string(),
            );
        }
    } else if let Some(from) = window.from {
        sentences.push(format!(
            "The week began at {} after a reset.",
            percent_text(from)
        ));
    }
    if first_unknown {
        sentences.push(
            "No reading right after the reset; the use is counted from 0 % and may be too high."
                .to_string(),
        );
    }
    if late {
        sentences.push(format!(
            "The last reading came {} before the reset; the use may be low.",
            hours_text(
                window
                    .end
                    .signed_duration_since(window.last_sample)
                    .num_seconds() as f64
                    / 3600.0,
            )
        ));
    }
    if expired > 0.0 {
        sentences.push(format!("{} expired at its reset.", percent_text(expired)));
    } else {
        sentences.push("Nothing expired at its reset.".to_string());
    }
    Some(sentences.join(" "))
}

/// The detail of the finished weekly window of the stored account that ends at
/// `end`.
pub fn window_detail<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    account_id: &str,
    end: DateTime<Utc>,
) -> Result<WindowDetail, StatsError> {
    // An account the store no longer holds is named by the id `account_id`
    // gives its window's account: the page shows that id's first 8 characters.
    let finished = keep_warnings(ctx.warnings, || {
        ctx.history
            .finished_claude_windows(&Range::ALL, &Hosts::All)
    })?
    .records;
    let reference = match ctx.accounts.iter().find(|account| account.id == account_id) {
        Some(account) => AccountRef::of(account),
        None => finished
            .iter()
            .map(|hosted| &hosted.record.account)
            .find(|reference| super::account_id(ctx, reference) == account_id)
            .cloned()
            .ok_or_else(|| unknown_window(account_id, end, true))?,
    };
    let kept = finished
        .iter()
        .find(|hosted| {
            hosted.record.k == LimitKind::WeeklyAll
                && hosted.record.account == reference
                && same_reset(hosted.record.end, end)
        })
        .ok_or_else(|| unknown_window(account_id, end, false))?;
    let window = kept.record.clone();
    let start = window.weekly_start();

    // The price of the window: the record `account_weeks` keeps, valued over
    // the window itself.
    let reading = Range {
        from: start,
        to: window.end,
    };
    let valuer = Valuer::build(
        ctx.history,
        &reading,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
        ctx.provider_subscriptions,
        ctx.warnings,
    )?;
    let query = Query {
        range: RangeKey::All,
        accounts: vec![account_id.to_string()],
        host: None,
    };
    let weeks = account_weeks_valued(ctx, &reading, &query, &valuer)?;
    let week = weeks
        .iter()
        .find(|week| week.account == reference && same_reset(week.window.end, window.end))
        .ok_or_else(|| unknown_window(account_id, end, false))?;
    // One window's own use: the money below counts that percent, not the sum of
    // the calendar week's windows. A window that ends below its start has no use
    // (`stats::window_used` says `None`): it adds nothing, and the valuer built
    // above names it in the page's warnings (once: `account_weeks_valued` adds
    // the same text only when it is not there yet).
    let used = window_used(&window).unwrap_or(0.0);
    let expired = lost_percent(&window)?;
    let money = week.price.map(|price| money_at(price, used, expired));

    let events = keep_warnings(ctx.warnings, || {
        ctx.history.read_claude::<Event>(&Range::ALL, &Hosts::All)
    })?;
    let reached_100_at = events
        .records
        .iter()
        .filter(|hosted| hosted.record.t >= start && hosted.record.t < window.end)
        .filter(|hosted| {
            let EventKind::Blocked { account, k, .. } = &hosted.record.kind else {
                return false;
            };
            account == &reference && matches!(k, LimitKind::WeeklyAll)
        })
        .map(|hosted| hosted.record.t)
        .min();
    let blocked_hours_after = match reached_100_at {
        Some(reached) => blocked_hours_after(ctx, &reference, reached, &window)?,
        None => 0.0,
    };

    let session_windows: Vec<&WindowRecord> = finished
        .iter()
        .map(|hosted| &hosted.record)
        .filter(|record| record.k == LimitKind::Session && record.account == reference)
        .collect();
    let session_windows_used = (!session_windows.is_empty()).then(|| {
        session_windows
            .iter()
            .filter(|record| {
                record.end >= start && record.end < window.end && record.final_percent > 0.0
            })
            .count() as u32
    });
    // The typical week of the window: the same learned rhythm the Forecast and
    // Rhythm pages take, over the four weeks before the window's reset. The row
    // is a session-limit row, so it is absent without a session window.
    let possible_session_windows = if session_windows_used.is_some() {
        let rhythm = working_rhythm(ctx, &reading, Company::Anthropic)?;
        possible_5h_windows(
            &rhythm,
            window.end - Duration::weeks(1),
            window.end,
            &ctx.zone,
        )
    } else {
        None
    };
    let used_more_than_possible = matches!(
        (session_windows_used, possible_session_windows),
        (Some(used), Some(possible)) if used > possible
    );
    // A reset record of the same limit ending where this window starts says a
    // reset began it; a scoped reset record ending with this weekly reset names
    // the model whose limit was given back together with the week.
    let began_at_reset = window.start.is_some_and(|window_start| {
        finished.iter().any(|hosted| {
            hosted.record.account == reference
                && hosted.record.k == window.k
                && hosted.record.reset
                && same_reset(hosted.record.end, window_start)
        })
    });
    let reset_model = if window.reset {
        finished
            .iter()
            .filter(|hosted| {
                hosted.record.account == reference
                    && hosted.record.k == LimitKind::WeeklyScoped
                    && hosted.record.reset
                    && same_reset(hosted.record.end, window.end)
            })
            .find_map(|hosted| hosted.record.m.clone())
    } else {
        None
    };
    let note = window_note(&window, expired, began_at_reset, reset_model.as_deref());
    let weekly_price = week.price.map(|price| money_at(price, 100.0, 0.0).paid);

    let mut switches_in_out = 0;
    let mut switches_auto = 0;
    for hosted in &events.records {
        let at = hosted.record.t;
        if at < start || at >= window.end {
            continue;
        }
        let EventKind::Switch { from, to, by, .. } = &hosted.record.kind else {
            continue;
        };
        if from.as_ref() != Some(&reference) && to != &reference {
            continue;
        }
        switches_in_out += 1;
        if *by == By::Auto {
            switches_auto += 1;
        }
    }

    let samples = keep_warnings(ctx.warnings, || {
        ctx.history.read_claude::<Sample>(
            &Range {
                from: start,
                to: window.end,
            },
            &Hosts::All,
        )
    })?;
    let curve = samples
        .records
        .iter()
        .filter(|hosted| hosted.record.account == reference)
        .filter_map(|hosted| {
            let limit = hosted.record.limit(&LimitKind::WeeklyAll)?;
            let reset = limit.r?;
            same_reset(reset, window.end).then_some((hosted.record.t, limit.p))
        })
        .collect();

    Ok(WindowDetail {
        id: super::account_id(ctx, &reference),
        start,
        end: window.end,
        plan: window.plan.clone(),
        weekly_price,
        used,
        used_money: money.map(|money| money.used),
        expired,
        expired_money: money.map(|money| money.expired),
        reached_100_at,
        blocked_hours_after,
        session_windows_used,
        possible_session_windows,
        used_more_than_possible,
        note,
        switches_in_out,
        switches_auto,
        last_sample_before_end_secs: (window.end - window.last_sample).num_seconds(),
        uncertain: window.uncertain,
        curve,
    })
}

fn unknown_window(account_id: &str, end: DateTime<Utc>, unknown_account: bool) -> StatsError {
    if unknown_account {
        StatsError::Message(format!(
            "unknown account {account_id}: no finished weekly window ending {end}"
        ))
    } else {
        StatsError::Message(format!(
            "account {account_id} has no finished weekly window ending {end}"
        ))
    }
}

/// The working hours blocked from `from` to the window's end. The hours are
/// counted in the rhythm of the four weeks before [`Ctx::now`]: a single
/// window has no range of its own to take them from.
fn blocked_hours_after<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    reference: &AccountRef,
    from: DateTime<Utc>,
    window: &WindowRecord,
) -> Result<f64, StatsError> {
    let to = window.end.max(ctx.now);
    if to <= from {
        return Ok(0.0);
    }
    let range = Range { from, to };
    let query = Query {
        range: RangeKey::All,
        accounts: vec![account_id(ctx, reference)],
        host: None,
    };
    let spans = blocked_spans(ctx, &range, &query, Company::Anthropic)?;
    let rhythm = working_rhythm(ctx, &range, Company::Anthropic)?;
    Ok(spans
        .iter()
        .map(|span| {
            let start = span.from.max(from);
            let end = span.to.min(window.end);
            if start < end {
                working_hours(start, end, &rhythm, &ctx.zone)
            } else {
                0.0
            }
        })
        .sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    use crate::history::{Event, EventKind, HistoryDir, LimitSample, Sample, Source};
    use crate::stats::tests::{
        all, append_codex_records, codex_ref, provider_subscription, tiny, utc,
    };

    /// The plan's range: 2 Nov .. 16 Nov.
    fn two_weeks() -> Range {
        Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.001
    }

    #[test]
    fn the_grid() {
        let tiny = tiny();
        let page = weeks_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        assert_eq!(
            page.week_starts,
            vec![utc("2026-11-02T00:00:00Z"), utc("2026-11-09T00:00:00Z")]
        );
        assert_eq!(page.rows.len(), 2);

        let lb = &page.rows[0];
        assert_eq!(lb.id, tiny.lb().id);
        assert_eq!(lb.plan.as_deref(), Some("Max"));
        let first = lb.cells[0].as_ref().expect("LB's W1");
        assert_eq!(first.used, 100.0);
        assert_eq!(first.expired, 0.0);
        assert!(first.full);
        let second = lb.cells[1].as_ref().expect("LB's W2");
        assert_eq!(second.used, 80.0);
        assert_eq!(second.expired, 20.0);
        assert!(!second.full);

        let ac = &page.rows[1];
        assert_eq!(ac.id, tiny.ac().id);
        assert_eq!(ac.plan.as_deref(), Some("Team Premium"));
        assert_eq!(ac.cells[0].as_ref().expect("AC's W1").used, 50.0);
        assert_eq!(ac.cells[1].as_ref().expect("AC's W2").used, 90.0);

        assert_eq!(lb.now_percent, Some(30.0));
        assert_eq!(ac.now_percent, Some(40.0));
        // Both running windows have their middle in the week of 16 Nov.
        assert_eq!(lb.now_week, Some(utc("2026-11-16T00:00:00Z")));
        assert_eq!(ac.now_week, Some(utc("2026-11-16T00:00:00Z")));
        assert_eq!(page.blocked, vec![2.0, 0.0]);
    }

    /// The page says which column is the running week, so the browser needs no
    /// clock of its own.
    #[test]
    fn the_running_week_is_marked_by_the_page() {
        let tiny = tiny();
        let ctx = tiny.ctx();
        let range = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: tiny.now,
        };
        let page = weeks_in(&ctx, &range, &all()).unwrap();
        assert_eq!(
            page.week_starts,
            vec![utc("2026-11-09T00:00:00Z"), utc("2026-11-16T00:00:00Z")]
        );
        assert_eq!(page.running, vec![false, true]);
    }

    /// A week before the account existed is empty, not zero: AC is added on
    /// 9 Nov and has no window in the week before.
    #[test]
    fn a_week_before_the_account_existed_is_empty() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        for window in &tiny.windows {
            if window.account == tiny.ac_ref() && window.end == utc("2026-11-09T02:00:00Z") {
                continue;
            }
            history.append(window).expect("a window");
        }
        history
            .append(&Event {
                t: utc("2026-11-09T00:00:00Z"),
                kind: EventKind::AccountAdded {
                    account: tiny.ac_ref(),
                    plan: Some("Team Premium".to_string()),
                },
            })
            .expect("AC added");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let page = weeks_in(&ctx, &two_weeks(), &all()).unwrap();
        let ac = page
            .rows
            .iter()
            .find(|row| row.id == tiny.ac().id)
            .expect("AC's row");
        assert_eq!(ac.cells[0], None);
        assert!(ac.cells[1].is_some());
    }

    #[test]
    fn an_uncertain_window_is_marked() {
        let tiny = tiny();
        let mut second = tiny
            .windows
            .iter()
            .find(|window| {
                window.account == tiny.lb_ref() && window.end == utc("2026-11-16T17:00:00Z")
            })
            .expect("LB's W2")
            .clone();
        second.uncertain = true;
        second.last_sample = second.end - Duration::minutes(1);
        tiny.history.append(&second).expect("the uncertain window");

        let page = weeks_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let cell = page.rows[0].cells[1].as_ref().expect("LB's W2");
        assert!(cell.uncertain);
        assert_eq!(cell.used, 80.0);

        let overview =
            crate::stats::overview::overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let week = overview
            .weeks
            .iter()
            .find(|week| week.week == utc("2026-11-09T00:00:00Z"))
            .expect("W2 in the Overview");
        let lb = week
            .accounts
            .iter()
            .find(|account| account.id == tiny.lb().id)
            .expect("LB's W2 in the Overview");
        assert_eq!(lb.used_percent, 80.0);
        assert!(lb.uncertain);
    }

    #[test]
    fn the_summary_of_w2() {
        let tiny = tiny();
        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-09T00:00:00Z")).unwrap();

        assert_eq!(summary.week, utc("2026-11-09T00:00:00Z"));
        assert!(
            close(summary.used.expect("a priced week"), 62.308),
            "used {:?}",
            summary.used
        );
        assert!(
            close(summary.expired.expect("a priced week"), 11.538),
            "expired {:?}",
            summary.expired
        );
        assert!(
            close(summary.used_percent_of_paid.expect("a priced week"), 84.375),
            "used percent {:?}",
            summary.used_percent_of_paid
        );
        assert_eq!(summary.blocked_hours, 0.0);
        assert_eq!(summary.switches.user, 0);
        assert_eq!(summary.switches.auto, 1);
        assert_eq!(summary.switches.agent, 1);
        assert_eq!(summary.top_projects, vec![("atlas".to_string(), 100.0)]);
        assert_eq!(summary.tokens.out, 4_000_000);
        assert_eq!(summary.values.api, Some(80.0));
        assert_eq!(summary.values.api_currency.as_deref(), Some("EUR"));
        assert!(
            close(summary.used_change_percent.expect("the change"), 8.0),
            "change {:?}",
            summary.used_change_percent
        );

        assert_eq!(summary.per_account_percent.len(), 2);
        assert_eq!(summary.per_account_percent[0].0, tiny.lb().id);
        assert!(
            close(summary.per_account_percent[0].1, 80.0),
            "LB {}",
            summary.per_account_percent[0].1
        );
        assert_eq!(summary.per_account_percent[1].0, tiny.ac().id);
        assert!(
            close(summary.per_account_percent[1].1, 90.0),
            "AC {}",
            summary.per_account_percent[1].1
        );

        assert_eq!(
            summary.conclusion,
            vec![
                "No hour blocked.".to_string(),
                "20 % of LB's week expired, 8.31 EUR.".to_string(),
                "Auto-burn held back 1 planned switch, 2 minutes in total.".to_string(),
            ]
        );
    }

    /// The summary's view has one per-account figure: the percent the page
    /// shows. The money per account (`perAccount`) is not part of the view.
    #[test]
    fn the_summary_shows_no_money_per_account() {
        let tiny = tiny();
        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-09T00:00:00Z")).unwrap();
        let view = serde_json::to_value(&summary).expect("the summary's view");
        let object = view.as_object().expect("the summary is an object");

        assert!(
            !object.contains_key("perAccount"),
            "the view still carries money per account: {view}"
        );
        assert!(
            object.contains_key("perAccountPercent"),
            "the view carries the per-account percent: {view}"
        );
    }

    #[test]
    fn the_detail_of_lb_w1() {
        let tiny = tiny();
        let detail =
            window_detail(&tiny.ctx(), &tiny.lb().id, utc("2026-11-09T17:00:00Z")).unwrap();

        assert_eq!(detail.id, tiny.lb().id);
        assert_eq!(detail.end, utc("2026-11-09T17:00:00Z"));
        assert_eq!(detail.used, 100.0);
        assert_eq!(detail.expired, 0.0);
        assert!(
            close(detail.used_money.expect("a price"), 41.538),
            "used money {:?}",
            detail.used_money
        );
        assert!(
            close(detail.expired_money.expect("a price"), 0.0),
            "expired money {:?}",
            detail.expired_money
        );
        assert_eq!(detail.reached_100_at, Some(utc("2026-11-07T10:00:00Z")));
        assert!(
            close(detail.blocked_hours_after, 2.0),
            "blocked after {}",
            detail.blocked_hours_after
        );
        assert_eq!(detail.session_windows_used, None);
        assert_eq!(detail.switches_in_out, 1);
        assert_eq!(detail.switches_auto, 0);
        assert_eq!(detail.last_sample_before_end_secs, 120);
        assert!(!detail.uncertain);
        assert!(detail.curve.is_empty());
    }

    #[test]
    fn an_unknown_window_is_an_error() {
        let tiny = tiny();
        let error =
            window_detail(&tiny.ctx(), &tiny.lb().id, utc("2026-11-10T17:00:00Z")).unwrap_err();

        assert!(
            error.to_string().contains(&tiny.lb().id),
            "the error names the account: {error}"
        );
        assert!(
            error.to_string().contains("2026-11-10"),
            "the error names the end: {error}"
        );
    }

    #[test]
    fn the_summary_of_an_empty_week() {
        let tiny = tiny();
        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-10-05T00:00:00Z")).unwrap();

        assert_eq!(summary.week, utc("2026-10-05T00:00:00Z"));
        assert_eq!(summary.used, None);
        assert_eq!(summary.expired, None);
        assert_eq!(summary.used_percent_of_paid, None);
        assert_eq!(
            summary.money_missing.as_deref(),
            Some("no priced weekly window in this week")
        );
        assert_eq!(summary.blocked_hours, 0.0);
        assert_eq!(summary.switches, SwitchCounts::default());
        assert!(summary.per_account_percent.is_empty());
        assert!(summary.top_projects.is_empty());
        assert_eq!(summary.tokens, Tokens::default());
        assert_eq!(
            summary.values.api,
            Some(0.0),
            "no record is a known 0, not an unknown value"
        );
        assert_eq!(summary.used_change_percent, None);
        assert_eq!(summary.conclusion, vec!["No hour blocked.".to_string()]);
    }

    /// A curve never goes back in time: a sample on 10 Nov names a reset on
    /// 16 Nov, after the sample on 12 Nov of the new window. The drop belongs
    /// to the later sample's time.
    #[test]
    fn a_curve_never_goes_back_in_time() {
        let tiny = tiny();
        tiny.history
            .append_many(&[
                Sample {
                    t: utc("2026-11-10T09:00:00Z"),
                    account: tiny.lb_ref(),
                    src: Source::Poll,
                    l: vec![LimitSample {
                        k: LimitKind::WeeklyAll,
                        m: None,
                        p: 55.0,
                        r: Some(utc("2026-11-16T09:00:00Z")),
                        b: true,
                    }],
                },
                Sample {
                    t: utc("2026-11-12T09:00:00Z"),
                    account: tiny.lb_ref(),
                    src: Source::Poll,
                    l: vec![LimitSample {
                        k: LimitKind::WeeklyAll,
                        m: None,
                        p: 5.0,
                        r: Some(utc("2026-11-23T09:00:00Z")),
                        b: true,
                    }],
                },
            ])
            .expect("the moved reset");

        let page = weeks_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let curve = page
            .recent
            .iter()
            .find(|curve| curve.id == tiny.lb().id)
            .expect("LB's curve");
        assert!(
            curve.points.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "the points are not in time order: {:#?}",
            curve.points
        );
        assert!(
            curve
                .points
                .iter()
                .any(|(at, percent)| *at == utc("2026-11-12T09:00:00Z") && *percent == 0.0),
            "the drop is not at the later sample's time: {:#?}",
            curve.points
        );
    }

    /// The running week counts its windows' use so far: LB at 30 % of 180 EUR
    /// and AC at 40 % of 140 EUR a month, each offering one week's price. It
    /// has expired nothing yet, so it has no expired share, no "came closest
    /// to expiring" sentence and no change against the week before.
    #[test]
    fn a_running_week_counts_its_use_so_far() {
        let tiny = tiny();
        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-16T00:00:00Z")).unwrap();

        let lb_week = 180.0 * 12.0 / 52.0;
        let ac_week = 140.0 * 12.0 / 52.0;
        let used = 0.30 * lb_week + 0.40 * ac_week;
        assert!(summary.running);
        assert!(close(summary.used.expect("the used money"), used));
        assert_eq!(summary.expired, Some(0.0));
        assert!(close(
            summary.used_percent_of_paid.expect("the used share"),
            used / (lb_week + ac_week) * 100.0
        ));
        assert_eq!(summary.expired_percent_of_paid, None);
        assert_eq!(summary.money_missing, None);
        assert_eq!(
            summary.per_account_percent,
            vec![(tiny.lb().id.clone(), 30.0), (tiny.ac().id.clone(), 40.0)]
        );
        assert_eq!(summary.used_change_percent, None);
        assert!(
            summary
                .conclusion
                .iter()
                .all(|sentence| !sentence.contains("expir")),
            "a running week has expired nothing: {:#?}",
            summary.conclusion
        );
    }

    /// On Monday 23 Nov 01:00 both running windows (resets 23 Nov 02:00 and
    /// 17:00) have their middle in last week: the summary of this week says so
    /// instead of showing an empty week.
    #[test]
    fn this_week_without_a_window_says_where_the_running_ones_count() {
        let tiny = tiny();
        let ctx = Ctx {
            now: utc("2026-11-23T01:00:00Z"),
            ..tiny.ctx()
        };
        let this_week = Query {
            range: RangeKey::ThisWeek,
            accounts: Vec::new(),
            host: None,
        };
        let summary = week_summary(&ctx, &this_week, utc("2026-11-23T00:00:00Z")).unwrap();

        assert!(!summary.running);
        assert_eq!(summary.used, None);
        assert!(summary.per_account_percent.is_empty());
        assert_eq!(
            summary.money_missing.as_deref(),
            Some(
                "The running weekly windows of AC and LB count in last week, \
                 the week that holds their middle."
            )
        );
        // Last week's summary holds them, with their use so far.
        let last = week_summary(&ctx, &this_week, utc("2026-11-16T00:00:00Z")).unwrap();
        assert!(last.running);
        assert_eq!(
            last.per_account_percent,
            vec![(tiny.lb().id.clone(), 30.0), (tiny.ac().id.clone(), 40.0)]
        );
    }

    /// An account no longer in the store keeps its row when it has a window in
    /// the range: after the stored accounts, with `account_id`'s id and its
    /// window record's plan; its cells still show the windows they showed.
    #[test]
    fn an_account_no_longer_stored_keeps_its_row() {
        let tiny = tiny();
        let before = weeks_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let ac_before = before
            .rows
            .iter()
            .find(|row| row.id == tiny.ac().id)
            .expect("AC's row before");
        let shape = |row: &WeeksRow| {
            row.cells
                .iter()
                .map(|cell| {
                    cell.as_ref()
                        .map(|cell| (cell.end, cell.used, cell.expired, cell.full, cell.uncertain))
                })
                .collect::<Vec<_>>()
        };
        let expected = shape(ac_before);
        let ac_acc = tiny.ac().account_uuid.clone();

        let accounts = vec![tiny.accounts[0].clone()];
        let ctx = Ctx {
            accounts: &accounts,
            ..tiny.ctx()
        };
        let page = weeks_in(&ctx, &two_weeks(), &all()).unwrap();

        assert_eq!(page.rows.len(), 2, "LB's row and AC's");
        assert_eq!(page.rows[0].id, tiny.lb().id);
        let ac = &page.rows[1];
        assert_eq!(ac.id, ac_acc, "the id `account_id` gives a removed account");
        assert_eq!(ac.plan.as_deref(), Some("Team Premium"));
        assert_eq!(shape(ac), expected, "AC's cells are as they were");
        assert_eq!(page.recent.len(), page.rows.len(), "curves stay with rows");
    }

    // ---- proof tests of the review ----

    /// The summary of a week shows the hours blocked in that week, also when
    /// the week lies before the query's range: the block of 7 Nov belongs to
    /// the week of 2 Nov, and the Overview shows 2 h for that week.
    #[test]
    fn the_summary_of_a_week_before_the_range_shows_its_blocked_hours() {
        let tiny = tiny();
        let query = Query {
            range: RangeKey::ThisWeek,
            accounts: Vec::new(),
            host: None,
        };

        let summary = week_summary(&tiny.ctx(), &query, utc("2026-11-02T00:00:00Z")).unwrap();

        assert!(
            close(summary.used.expect("a priced week"), 57.692),
            "the money of the week is found outside the range: {:?}",
            summary.used
        );
        assert!(
            close(summary.blocked_hours, 2.0),
            "the week of 2 Nov shows {} blocked; the Overview shows 2h for it",
            summary.blocked_hours
        );
    }

    /// The account filter applies to every number of the summary: the tokens
    /// and the API value of the week belong to the chosen account, as the
    /// Overview's projects of a week do.
    #[test]
    fn the_summary_honours_the_account_filter() {
        let tiny = tiny();
        let query = Query {
            range: RangeKey::All,
            accounts: vec![tiny.lb().id.clone()],
            host: None,
        };

        let summary = week_summary(&tiny.ctx(), &query, utc("2026-11-09T00:00:00Z")).unwrap();

        assert!(
            close(summary.used.expect("a priced week"), 33.231),
            "LB's money of the week of 9 Nov: {:?}",
            summary.used
        );
        assert_eq!(
            summary.tokens.out, 1_000_000,
            "LB's tokens of the week of 9 Nov"
        );
        assert_eq!(
            summary.values.api,
            Some(20.0),
            "LB's API value of the week of 9 Nov"
        );
    }

    /// A switch that waited no time was not held back: the sentence would
    /// claim a hold of zero minutes.
    #[test]
    fn a_switch_that_waited_no_time_is_not_held_back() {
        let tiny = tiny();
        tiny.history
            .append(&Event {
                t: utc("2026-11-06T12:00:00Z"),
                kind: EventKind::Switch {
                    from: None,
                    to: tiny.lb_ref(),
                    by: By::Auto,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: Some(0),
                },
            })
            .expect("a switch that waited no time");

        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-02T00:00:00Z")).unwrap();

        assert!(
            !summary
                .conclusion
                .iter()
                .any(|text| text.contains("held back")),
            "the conclusion claims a hold of no time: {:#?}",
            summary.conclusion
        );
    }

    /// The grid shows the window of every week it shows: a window whose
    /// midpoint lies in the first week of the range is on the page also when
    /// the range's first record is later in that week.
    #[test]
    fn the_grid_shows_the_window_of_the_first_week_of_the_range() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&Event {
                t: utc("2026-11-04T08:00:00Z"),
                kind: EventKind::Live {
                    account: Some(tiny.lb_ref()),
                },
            })
            .expect("the live event");
        history
            .append(&WindowRecord {
                account: tiny.lb_ref(),
                k: LimitKind::WeeklyAll,
                m: None,
                start: Some(utc("2026-10-29T12:00:00Z")),
                end: utc("2026-11-05T12:00:00Z"),
                final_percent: 90.0,
                lost: Some(10.0),
                last_sample: utc("2026-11-05T11:58:00Z"),
                plan: Some("Max".to_string()),
                uncertain: false,
                reset: false,
                from: None,
            })
            .expect("the window");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };
        let query = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: None,
        };

        let range = range_of(RangeKey::All, &ctx).unwrap();
        assert_eq!(
            range.from,
            utc("2026-11-02T00:00:00Z"),
            "the range starts at the Monday of the first record's week"
        );
        let page = weeks_in(&ctx, &range, &query).unwrap();

        assert_eq!(page.week_starts[0], utc("2026-11-02T00:00:00Z"));
        let cell = page.rows[0].cells[0]
            .as_ref()
            .expect("LB's window of the week of 2 Nov");
        assert_eq!(cell.used, 90.0);
        assert_eq!(cell.end, utc("2026-11-05T12:00:00Z"));
    }

    /// Every window of an account and week is counted: the grid shows the
    /// later end and the summed use, and the summary of that week counts both
    /// windows' use.
    #[test]
    fn the_summary_counts_the_window_the_grid_shows() {
        let tiny = tiny();
        let mut second = tiny
            .windows
            .iter()
            .find(|window| {
                window.account == tiny.lb_ref() && window.end == utc("2026-11-09T17:00:00Z")
            })
            .expect("LB's W1")
            .clone();
        // The reset of LB moved earlier: the second window is short, and both
        // windows' midpoints lie in the week of 2 Nov.
        second.start = Some(utc("2026-11-05T17:00:00Z"));
        second.end = utc("2026-11-10T12:00:00Z");
        second.final_percent = 60.0;
        second.lost = Some(40.0);
        second.last_sample = utc("2026-11-10T11:58:00Z");
        tiny.history.append(&second).expect("LB's second window");

        let page = weeks_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let cell = page.rows[0].cells[0].as_ref().expect("LB's W1");
        assert_eq!(cell.end, utc("2026-11-10T12:00:00Z"), "the later end");
        assert_eq!(cell.used, 160.0, "100 % and 60 % of the same week");

        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-02T00:00:00Z")).unwrap();
        assert!(
            close(summary.used.expect("a priced week"), 82.615),
            "the money of the week counts both windows of LB: {:?}",
            summary.used
        );
        assert!(
            close(summary.per_account_percent[0].1, 160.0),
            "LB's use of the week: {}",
            summary.per_account_percent[0].1
        );
    }

    /// A reset inside one calendar week keeps both windows: the cell lists each
    /// window's own use oldest first, and its total is their sum, which may pass
    /// 100 %. The first case is 62 % then a reset, then 30 % from 0 (92 %); the
    /// second is 70 % then a model reset to 45 % and up to 80 % (105 %).
    #[test]
    fn a_week_with_a_reset_sums_its_windows() {
        let tiny = tiny();
        let window = |start: &str,
                      end: &str,
                      from: Option<f64>,
                      final_percent: f64,
                      lost: Option<f64>,
                      reset: bool| {
            WindowRecord {
                account: tiny.lb_ref(),
                k: LimitKind::WeeklyAll,
                m: None,
                start: Some(utc(start)),
                end: utc(end),
                final_percent,
                lost,
                last_sample: utc(end) - Duration::minutes(2),
                plan: Some("Max".to_string()),
                uncertain: false,
                reset,
                from,
            }
        };

        // 62 %, then the limit was given back and the week ran to 30 % from 0.
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append_many(&[
                window(
                    "2026-11-02T00:00:00Z",
                    "2026-11-05T12:00:00Z",
                    None,
                    62.0,
                    None,
                    true,
                ),
                window(
                    "2026-11-05T12:00:00Z",
                    "2026-11-09T12:00:00Z",
                    Some(0.0),
                    30.0,
                    Some(70.0),
                    false,
                ),
            ])
            .expect("the two windows");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };
        let page = weeks_in(&ctx, &two_weeks(), &all()).unwrap();
        let cell = page.rows[0].cells[0].as_ref().expect("LB's week");
        assert_eq!(cell.parts, vec![62.0, 30.0], "62 % then 30 % from 0");
        assert_eq!(cell.used, 92.0, "the week's total");

        // 70 %, then a model reset to 45 % and up to 80 %: 70 + 35 = 105 %.
        let dir = tempdir().expect("a second temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append_many(&[
                // The model window that reset with the week: the record that
                // makes the 45 % start a model reset, not only `from: Some(45.0)`.
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyScoped,
                    m: Some("Fable".to_string()),
                    start: Some(utc("2026-10-30T12:00:00Z")),
                    end: utc("2026-11-06T12:00:00Z"),
                    final_percent: 50.0,
                    lost: None,
                    last_sample: utc("2026-11-06T11:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: true,
                    from: None,
                },
                window(
                    "2026-11-02T00:00:00Z",
                    "2026-11-06T12:00:00Z",
                    None,
                    70.0,
                    None,
                    true,
                ),
                window(
                    "2026-11-06T12:00:00Z",
                    "2026-11-09T12:00:00Z",
                    Some(45.0),
                    80.0,
                    Some(20.0),
                    false,
                ),
            ])
            .expect("the two windows");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };
        let page = weeks_in(&ctx, &two_weeks(), &all()).unwrap();
        let cell = page.rows[0].cells[0].as_ref().expect("LB's week");
        assert_eq!(cell.parts, vec![70.0, 35.0], "70 % then 45 % to 80 %");
        assert_eq!(cell.used, 105.0, "the week's total passes 100 %");
    }

    /// The summary's Expired share divides by the money the week's windows
    /// offered, as Overview does: two ordinary windows with 20 % and 10 % lost
    /// offer two weeks' price, so the week expired 30 / 200 = 15 %, not the last
    /// window's 10 / 100 = 10 %.
    #[test]
    fn the_summary_divides_the_expired_share_by_the_offered_windows() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let window = |end: &str, final_percent: f64, lost: f64| WindowRecord {
            account: tiny.lb_ref(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(utc(end) - Duration::days(7)),
            end: utc(end),
            final_percent,
            lost: Some(lost),
            last_sample: utc(end) - Duration::minutes(2),
            plan: Some("Max".to_string()),
            uncertain: false,
            reset: false,
            from: None,
        };
        history
            .append_many(&[
                window("2026-11-06T12:00:00Z", 80.0, 20.0),
                window("2026-11-09T12:00:00Z", 90.0, 10.0),
            ])
            .expect("the two ordinary windows");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };
        let summary =
            week_summary(&ctx, &all(), utc("2026-11-02T00:00:00Z")).expect("the week's summary");
        assert!(
            close(
                summary.expired_percent_of_paid.expect("a priced week"),
                15.0
            ),
            "expired 30 of the 200 offered: {:?}",
            summary.expired_percent_of_paid
        );
        assert!(
            close(summary.used_percent_of_paid.expect("a priced week"), 85.0),
            "used 170 of the 200 offered: {:?}",
            summary.used_percent_of_paid
        );
        let weekly = tiny.lb().price.expect("LB's price") * 12.0 / 52.0;
        assert!(
            close(summary.expired.expect("a priced week"), 0.30 * weekly),
            "expired money is both windows' lost: {:?}",
            summary.expired
        );
    }

    /// "Of about N possible" counts the typical week's working hours: the
    /// learned [`working_rhythm`] of the four weeks before the window's reset
    /// (the same figure the Forecast and Rhythm pages use), counted with
    /// [`working_hours`] over the seven days before the window's end and divided
    /// by the 5 h session window. This history has 40 working hours in that
    /// rhythm, so 8 session windows fit.
    #[test]
    fn possible_is_counted_from_the_typical_week() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let start = utc("2026-11-09T12:00:00Z");
        let end = utc("2026-11-16T12:00:00Z");
        history
            .append_many(&[
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(start),
                    end,
                    final_percent: 80.0,
                    lost: Some(20.0),
                    last_sample: end - Duration::minutes(2),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: false,
                    from: None,
                },
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::Session,
                    m: None,
                    start: Some(utc("2026-11-09T13:00:00Z")),
                    end: utc("2026-11-09T18:00:00Z"),
                    final_percent: 50.0,
                    lost: Some(50.0),
                    last_sample: utc("2026-11-09T17:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: false,
                    from: None,
                },
            ])
            .expect("the windows");
        for hour in 0..40 {
            history
                .append(&crate::history::TokenRecord {
                    t: utc("2026-11-09T00:00:00Z") + Duration::hours(hour),
                    minutes: 60,
                    session: "session-1".to_string(),
                    cwd: "/home/sam/atlas".to_string(),
                    project: crate::project::ProjectKey::Folder("/home/sam/atlas".to_string()),
                    model: "claude-opus-5-5".to_string(),
                    input: 0,
                    out: 1,
                    cw: 0,
                    cr: 0,
                    answers: 1,
                    agent_answers: 0,
                })
                .expect("a token record");
        }
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let detail = window_detail(&ctx, &tiny.lb().id, end).expect("the detail");
        let rhythm = working_rhythm(
            &ctx,
            &Range {
                from: start,
                to: end,
            },
            Company::Anthropic,
        )
        .expect("the rhythm");
        let working = working_hours(end - Duration::weeks(1), end, &rhythm, &ctx.zone);
        assert!(
            close(working, 40.0),
            "the typical week is working_hours over the seven days before the window's end: {working}"
        );
        assert_eq!(
            detail.possible_session_windows,
            Some(8),
            "40 typical working hours / 5h windows"
        );
    }

    /// Review of T-2: "of about N possible" counts the typical week's working
    /// hours, but the used windows are the session windows that ended in that
    /// week. A week with more session windows than the typical week has
    /// working hours must not print "12 used of about 8 possible in your
    /// working hours": the view says the used count is above the possible one,
    /// and the page then words the line "12 used, more than the about 8 your
    /// usual working hours hold".
    #[test]
    fn proof_the_page_does_not_claim_fewer_possible_windows_than_used() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let start = utc("2026-11-09T12:00:00Z");
        let end = utc("2026-11-16T12:00:00Z");
        let mut windows = vec![WindowRecord {
            account: tiny.lb_ref(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(start),
            end,
            final_percent: 80.0,
            lost: Some(20.0),
            last_sample: end - Duration::minutes(2),
            plan: Some("Max".to_string()),
            uncertain: false,
            reset: false,
            from: None,
        }];
        // Twelve session windows of one percent each, all inside the week.
        for index in 0..12i64 {
            let session_end = start + Duration::hours(1 + 5 * index);
            windows.push(WindowRecord {
                account: tiny.lb_ref(),
                k: LimitKind::Session,
                m: None,
                start: Some(session_end - Duration::hours(5)),
                end: session_end,
                final_percent: 1.0,
                lost: Some(99.0),
                last_sample: session_end - Duration::minutes(2),
                plan: Some("Max".to_string()),
                uncertain: false,
                reset: false,
                from: None,
            });
        }
        history.append_many(&windows).expect("the windows");
        // The typical week: 40 working hours, so eight 5 h windows.
        for hour in 0..40 {
            history
                .append(&crate::history::TokenRecord {
                    t: utc("2026-11-09T00:00:00Z") + Duration::hours(hour),
                    minutes: 60,
                    session: "session-1".to_string(),
                    cwd: "/home/sam/atlas".to_string(),
                    project: crate::project::ProjectKey::Folder("/home/sam/atlas".to_string()),
                    model: "claude-opus-5-5".to_string(),
                    input: 0,
                    out: 1,
                    cw: 0,
                    cr: 0,
                    answers: 1,
                    agent_answers: 0,
                })
                .expect("a token record");
        }
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let detail = window_detail(&ctx, &tiny.lb().id, end).expect("the detail");
        assert_eq!(
            detail.session_windows_used,
            Some(12),
            "twelve session windows ended in the week"
        );
        assert_eq!(
            detail.possible_session_windows,
            Some(8),
            "40 typical working hours / 5h windows"
        );
        assert!(
            detail.used_more_than_possible,
            "the used 5h windows ({:?}) are above the possible ones ({:?}), so the page must not \
             print \"12 used of about 8 possible in your working hours\"",
            detail.session_windows_used, detail.possible_session_windows
        );
    }

    /// Review of T-2: a window whose first percent after a reset is unknown
    /// (the sample after the reset is missing) is marked uncertain, and its
    /// use is counted from 0 % — it may be too high, not low. The Note must
    /// say that and must not say the use "may be low".
    #[test]
    fn proof_the_note_does_not_call_an_over_counted_use_low() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let start = utc("2026-11-09T12:00:00Z");
        let end = utc("2026-11-16T12:00:00Z");
        history
            .append_many(&[WindowRecord {
                account: tiny.lb_ref(),
                k: LimitKind::WeeklyAll,
                m: None,
                start: Some(start),
                end,
                final_percent: 80.0,
                lost: Some(20.0),
                last_sample: end - Duration::minutes(2),
                plan: Some("Max".to_string()),
                // The sample after the reset is missing: the window's first
                // percent is unknown, so its use is counted from 0 %.
                uncertain: true,
                reset: false,
                from: None,
            }])
            .expect("the window");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let detail = window_detail(&ctx, &tiny.lb().id, end).expect("the detail");
        let note = detail.note.clone().unwrap_or_default();
        assert!(
            note.contains(
                "No reading right after the reset; the use is counted from 0 % and may be too high."
            ),
            "the note must say the use is counted from 0 % and may be too high: {note:?}"
        );
        assert!(
            !note.contains("may be low"),
            "the note must not say the use may be low when the window's first percent \
             is unknown and its use is counted from 0 %: {note:?}"
        );
    }

    /// A last reading more than an hour before the reset is the late-reading
    /// kind of uncertain window: the Note names how early the reading came and
    /// says the use may be low.
    #[test]
    fn the_note_of_a_late_last_reading_says_the_use_may_be_low() {
        let tiny = tiny();
        let window = WindowRecord {
            account: tiny.lb_ref(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(utc("2026-11-09T12:00:00Z")),
            end: utc("2026-11-16T12:00:00Z"),
            final_percent: 80.0,
            lost: Some(20.0),
            // Two hours before the end: write.rs's late-reading rule.
            last_sample: utc("2026-11-16T10:00:00Z"),
            plan: Some("Max".to_string()),
            uncertain: true,
            reset: false,
            from: Some(30.0),
        };

        let note = window_note(&window, 20.0, false, None).unwrap_or_default();
        assert!(
            note.contains("The last reading came 2h before the reset; the use may be low."),
            "the note names the early reading and that the use may be low: {note:?}"
        );
        assert!(
            !note.contains("counted from 0 %"),
            "a known first percent is not counted from 0 %: {note:?}"
        );
    }

    /// A window with both kinds of uncertain reading says both: its first
    /// percent after a reset is unknown and its last reading came early.
    #[test]
    fn the_note_of_both_uncertain_kinds_says_both() {
        let tiny = tiny();
        let window = WindowRecord {
            account: tiny.lb_ref(),
            k: LimitKind::WeeklyAll,
            m: None,
            // A reset began it, so `from: None` means the first reading is
            // missing rather than the ordinary 0 %.
            start: Some(utc("2026-11-09T12:00:00Z")),
            end: utc("2026-11-16T12:00:00Z"),
            final_percent: 80.0,
            lost: Some(20.0),
            last_sample: utc("2026-11-16T09:00:00Z"),
            plan: Some("Max".to_string()),
            uncertain: true,
            reset: false,
            from: None,
        };

        let note = window_note(&window, 20.0, true, None).unwrap_or_default();
        assert!(
            note.contains(
                "No reading right after the reset; the use is counted from 0 % and may be too high."
            ),
            "the note says the first percent is missing: {note:?}"
        );
        assert!(
            note.contains("The last reading came 3h before the reset; the use may be low."),
            "the note names the early reading too: {note:?}"
        );
    }

    /// The "given back" sentence names which limit was given back: the week's
    /// own reset when the record is the week, a model's when a scoped reset
    /// record ends with it.
    #[test]
    fn the_note_names_the_limit_that_was_given_back() {
        let tiny = tiny();
        let week_start = utc("2026-11-02T12:00:00Z");
        let end = utc("2026-11-06T12:00:00Z");
        let reset = |model: Option<&str>| WindowRecord {
            account: tiny.lb_ref(),
            k: if model.is_some() {
                LimitKind::WeeklyScoped
            } else {
                LimitKind::WeeklyAll
            },
            m: model.map(str::to_string),
            start: Some(week_start),
            end,
            final_percent: 70.0,
            lost: None,
            last_sample: end - Duration::minutes(2),
            plan: Some("Max".to_string()),
            uncertain: false,
            reset: true,
            from: None,
        };

        let week = window_note(&reset(None), 0.0, false, None).unwrap_or_default();
        assert!(
            week.contains(
                "The week's limit was given back before its reset, so the use before it stays counted."
            ),
            "the week's own reset is named: {week:?}"
        );
        let model = window_note(&reset(Some("Fable")), 0.0, false, None).unwrap_or_default();
        assert!(
            model.contains(
                "Fable's limit was given back before its reset, so the use before it stays counted."
            ),
            "a model's reset names the model: {model:?}"
        );
        let together = window_note(&reset(None), 0.0, false, Some("Fable")).unwrap_or_default();
        assert!(
            together.contains(
                "Fable's limit was given back before its reset, so the use before it stays counted."
            ),
            "a scoped reset record ending with the week names the model: {together:?}"
        );
    }

    /// The detail of a window that a reset began and whose last reading came
    /// early says both uncertain sentences: the first percent is unknown and
    /// the use may be too high, and the last reading may make it low.
    #[test]
    fn the_detail_of_a_reset_started_window_says_both_uncertain_kinds() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let reset_end = utc("2026-11-09T12:00:00Z");
        let end = utc("2026-11-12T12:00:00Z");
        history
            .append_many(&[
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-02T12:00:00Z")),
                    end: reset_end,
                    final_percent: 70.0,
                    lost: None,
                    last_sample: reset_end - Duration::minutes(2),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: true,
                    from: None,
                },
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(reset_end),
                    end,
                    final_percent: 80.0,
                    lost: Some(20.0),
                    last_sample: end - Duration::hours(2),
                    plan: Some("Max".to_string()),
                    uncertain: true,
                    reset: false,
                    from: None,
                },
            ])
            .expect("the two windows");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let detail = window_detail(&ctx, &tiny.lb().id, end).expect("the detail");
        let note = detail.note.clone().unwrap_or_default();
        assert!(
            note.contains(
                "No reading right after the reset; the use is counted from 0 % and may be too high."
            ),
            "the reset began the window and its first percent is unknown: {note:?}"
        );
        assert!(
            note.contains("The last reading came 2h before the reset; the use may be low."),
            "the last reading came early too: {note:?}"
        );
    }

    /// A weekly reset that a scoped reset ended with names the model in the
    /// detail's Note, as the history's scoped record tells them apart.
    #[test]
    fn the_detail_names_the_model_that_reset_with_the_week() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let end = utc("2026-11-06T12:00:00Z");
        history
            .append_many(&[
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-02T00:00:00Z")),
                    end,
                    final_percent: 70.0,
                    lost: None,
                    last_sample: end - Duration::minutes(2),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: true,
                    from: None,
                },
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyScoped,
                    m: Some("Fable".to_string()),
                    start: Some(utc("2026-10-30T00:00:00Z")),
                    end,
                    final_percent: 50.0,
                    lost: None,
                    last_sample: end - Duration::minutes(2),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: true,
                    from: None,
                },
            ])
            .expect("the two reset records");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let detail = window_detail(&ctx, &tiny.lb().id, end).expect("the detail");
        let note = detail.note.clone().unwrap_or_default();
        assert!(
            note.contains(
                "Fable's limit was given back before its reset, so the use before it stays counted."
            ),
            "the scoped reset record names the model: {note:?}"
        );
        assert!(
            !note.contains("The week's limit was given back"),
            "a model reset is not worded as the week's own reset: {note:?}"
        );
    }

    /// The host filter applies to the summary's tokens, API value and top
    /// projects: host vega has neither of atlas's records, so none of atlas's
    /// tokens is in the week.
    #[test]
    fn the_summary_honours_the_host_filter() {
        let tiny = tiny();
        let query = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: Some("vega".to_string()),
        };

        let summary = week_summary(&tiny.ctx(), &query, utc("2026-11-09T00:00:00Z")).unwrap();

        assert_eq!(
            summary.tokens,
            Tokens::default(),
            "atlas's tokens of the week are not vega's"
        );
        assert_eq!(
            summary.values.api,
            Some(0.0),
            "no record is a known 0, not an unknown value"
        );
        assert!(summary.top_projects.is_empty());
        assert!(
            summary.used.is_some(),
            "the week's windows are not a host's"
        );
    }

    /// A window that ends below its start is named in the page's warnings and
    /// adds no use; the week's used percent is the other window's 62 %.
    #[test]
    fn a_window_that_ends_below_its_start_is_named_and_adds_no_use() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let window = |start: &str, end: &str, final_percent: f64, lost, reset, from| WindowRecord {
            account: tiny.lb_ref(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(utc(start)),
            end: utc(end),
            final_percent,
            lost,
            last_sample: utc(end) - Duration::minutes(2),
            plan: Some("Max".to_string()),
            uncertain: false,
            reset,
            from,
        };
        history
            .append_many(&[
                window(
                    "2026-11-02T00:00:00Z",
                    "2026-11-05T12:00:00Z",
                    62.0,
                    None,
                    true,
                    None,
                ),
                window(
                    "2026-11-05T12:00:00Z",
                    "2026-11-09T12:00:00Z",
                    20.0,
                    Some(80.0),
                    false,
                    Some(30.0),
                ),
            ])
            .expect("the two windows");
        let warnings = std::cell::RefCell::new(Vec::new());
        let ctx = Ctx {
            history: &history,
            warnings: &warnings,
            ..tiny.ctx()
        };

        let page = weeks_in(&ctx, &two_weeks(), &all()).unwrap();
        let cell = page
            .rows
            .iter()
            .find(|row| row.id == tiny.lb().id)
            .expect("LB's row")
            .cells[0]
            .as_ref()
            .expect("LB's unfinished week");
        assert_eq!(
            cell.used, 62.0,
            "the window that ends below its start adds nothing"
        );

        let warnings = warnings.borrow();
        assert_eq!(warnings.len(), 1, "one warning: {warnings:?}");
        assert!(
            warnings[0].contains(&tiny.lb_ref().acc)
                && warnings[0].contains("2026-11-09 12:00:00 UTC"),
            "the warning names the account and the window's end: {}",
            warnings[0]
        );
    }

    /// The page names a window that ends below its start; a detail page of
    /// another week must not carry a warning about a window it does not show
    /// (the valuer reads the neighbouring weeks too).
    #[test]
    fn proof_a_window_detail_does_not_warn_about_another_weeks_window() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let window = |start: &str, end: &str, final_percent: f64, lost, from| WindowRecord {
            account: tiny.lb_ref(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(utc(start)),
            end: utc(end),
            final_percent,
            lost,
            last_sample: utc(end) - Duration::minutes(2),
            plan: Some("Max".to_string()),
            uncertain: false,
            reset: false,
            from,
        };
        history
            .append_many(&[
                window(
                    "2026-11-02T12:00:00Z",
                    "2026-11-09T12:00:00Z",
                    20.0,
                    Some(80.0),
                    Some(30.0),
                ),
                window(
                    "2026-11-09T12:00:00Z",
                    "2026-11-16T12:00:00Z",
                    50.0,
                    Some(50.0),
                    None,
                ),
            ])
            .expect("the two windows");
        let warnings = std::cell::RefCell::new(Vec::new());
        let ctx = Ctx {
            history: &history,
            warnings: &warnings,
            ..tiny.ctx()
        };

        window_detail(&ctx, &tiny.lb().id, utc("2026-11-16T12:00:00Z")).unwrap();

        assert!(
            warnings.borrow().is_empty(),
            "the detail of the week ending 16 Nov names the window of 9 Nov: {:?}",
            warnings.borrow()
        );
    }

    /// Review of T-1: the detail of a window that starts after a reset shows
    /// one use: the percent and the money must agree.
    #[test]
    fn proof_a_window_detail_after_a_reset_agrees_with_its_money() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&crate::history::WindowRecord {
                account: tiny.lb_ref(),
                k: crate::api::LimitKind::WeeklyAll,
                m: None,
                start: Some(utc("2026-11-05T12:00:00Z")),
                end: utc("2026-11-09T12:00:00Z"),
                final_percent: 60.0,
                lost: Some(40.0),
                last_sample: utc("2026-11-09T11:58:00Z"),
                plan: Some("Max".to_string()),
                uncertain: false,
                reset: false,
                from: Some(30.0),
            })
            .expect("a window that starts at 30 % after a reset");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };
        let detail = window_detail(&ctx, &tiny.lb().id, utc("2026-11-09T12:00:00Z")).unwrap();
        let paid = 180.0 * 12.0 / 52.0;
        let money = detail.used_money.expect("LB's price is known");
        assert!(
            close(money, detail.used / 100.0 * paid),
            "used {} % but used money {} of a week's {}: the money counts 60 - 30 = 30 %",
            detail.used,
            money,
            paid
        );
    }

    /// A window of an account the store no longer holds answers with the id
    /// `account_id` gives it, instead of an error.
    #[test]
    fn the_detail_of_a_removed_accounts_window_answers() {
        let tiny = tiny();
        tiny.history
            .append(&Event {
                t: utc("2026-11-15T00:00:00Z"),
                kind: EventKind::AccountRemoved {
                    account: tiny.ac_ref(),
                },
            })
            .expect("AC's removal");
        let accounts = vec![tiny.accounts[0].clone()];
        let ctx = Ctx {
            accounts: &accounts,
            ..tiny.ctx()
        };

        let detail =
            window_detail(&ctx, &tiny.ac().account_uuid, utc("2026-11-16T02:00:00Z")).unwrap();

        assert_eq!(detail.id, tiny.ac().account_uuid);
        assert_eq!(detail.used, 90.0);
        assert_eq!(
            detail.used_money, None,
            "a removed account has no stored price to value its window"
        );
    }

    /// Without a dollar rate the API value is in USD, not in the settings'
    /// currency; the summary carries that currency instead of relabelling it.
    #[test]
    fn without_a_rate_the_summary_api_value_is_in_usd() {
        let mut tiny = tiny();
        tiny.fx = crate::money::Fx::default();

        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-09T00:00:00Z")).unwrap();

        assert_eq!(summary.values.api, Some(80.0));
        assert_eq!(summary.values.api_currency.as_deref(), Some("USD"));
        assert_eq!(summary.values.currency.as_deref(), Some("EUR"));
        assert_eq!(summary.top_projects, vec![("atlas".to_string(), 100.0)]);
    }

    #[test]
    fn provider_records_do_not_change_a_window_detail() {
        let without = tiny();
        let with = tiny();
        append_codex_records(&with.history, utc("2026-11-14T09:00:00Z"));
        let end = without
            .windows
            .iter()
            .find(|window| window.account == without.lb_ref())
            .expect("LB's window")
            .end;

        let before = window_detail(&without.ctx(), &without.lb().id, end).expect("the detail");
        let after = window_detail(&with.ctx(), &with.lb().id, end).expect("the detail");
        assert_eq!(after, before, "a Codex record changed a Claude window");

        let before = weeks_in(&without.ctx(), &two_weeks(), &all()).expect("the page");
        let mut after = weeks_in(&with.ctx(), &two_weeks(), &all()).expect("the page");
        // P-2 counts the OpenAI subscription's week as an account week, so the
        // grid gains its row; every Claude figure stays what it was.
        let codex_id = "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13";
        let index = after
            .rows
            .iter()
            .position(|row| row.id == codex_id)
            .expect("the Codex week is a row of the grid");
        let codex_row = after.rows.remove(index);
        after.recent.retain(|curve| curve.id != codex_id);
        assert_eq!(
            after, before,
            "a Codex record changed a Claude figure of the weeks page"
        );
        let cell = codex_row.cells[1]
            .as_ref()
            .expect("the Codex cell of the week it ended in");
        assert_eq!(cell.used, 12.0);
        assert_eq!(cell.expired_money, None, "the subscription has no price");

        let before =
            week_summary(&without.ctx(), &all(), utc("2026-11-09T00:00:00Z")).expect("the summary");
        let mut after =
            week_summary(&with.ctx(), &all(), utc("2026-11-09T00:00:00Z")).expect("the summary");
        // P-2 adds the Codex week to the week's account percents. It has no
        // price, so no money sentence names it; the priced Claude week keeps
        // its sentence instead of being hidden by the unpriced provider week
        // (review C).
        assert_eq!(
            after.per_account_percent.pop(),
            Some((codex_id.to_string(), 12.0)),
            "the Codex week is in the week's account percents"
        );
        assert_eq!(
            after.conclusion,
            vec![
                "No hour blocked.".to_string(),
                "20 % of LB's week expired, 8.31 EUR.".to_string(),
                "Auto-burn held back 1 planned switch, 2 minutes in total.".to_string(),
            ],
            "an unpriced provider week does not hide the priced week's sentence"
        );
        after.per_account_percent = before.per_account_percent.clone();
        after.conclusion = before.conclusion.clone();
        assert_eq!(
            after, before,
            "a Codex record changed a Claude figure of the week summary"
        );
    }

    /// An unpriced provider week never hides a priced week's sentence: the
    /// conclusion is the same with and without the Codex week (review C).
    #[test]
    fn an_unpriced_codex_week_does_not_hide_the_claude_sentence() {
        let without = tiny();
        let with = tiny();
        append_codex_records(&with.history, utc("2026-11-14T09:00:00Z"));

        let before =
            week_summary(&without.ctx(), &all(), utc("2026-11-09T00:00:00Z")).expect("the summary");
        let after =
            week_summary(&with.ctx(), &all(), utc("2026-11-09T00:00:00Z")).expect("the summary");
        assert_eq!(
            after.conclusion, before.conclusion,
            "an unpriced Codex week changed the conclusion"
        );
    }

    /// A priced provider week's sentence names the subscription's abbreviation,
    /// never its id (decision P-2 1, 4, review C).
    #[test]
    fn a_priced_codex_week_is_named_by_its_subscription() {
        let mut tiny = tiny();
        let codex = codex_ref();
        tiny.provider_subscriptions = vec![provider_subscription(&codex, "OA", Some(23.0))];
        append_codex_records(&tiny.history, utc("2026-11-14T09:00:00Z"));

        let summary =
            week_summary(&tiny.ctx(), &all(), utc("2026-11-09T00:00:00Z")).expect("the summary");
        assert!(
            summary
                .conclusion
                .iter()
                .any(|line| line.contains("88 % of OA's week expired")),
            "the priced Codex week is named OA: {:?}",
            summary.conclusion
        );
        assert!(
            summary
                .conclusion
                .iter()
                .all(|line| !line.contains("0f3a6c2e")),
            "no sentence names the Codex account by its id: {:?}",
            summary.conclusion
        );
    }
}
