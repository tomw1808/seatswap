//! The Weeks page (spec §3.2): every weekly window as a grid, the last two
//! weeks as curves, the summary of one calendar week and the detail of one
//! window. Everything here only reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::api::LimitKind;
use crate::history::{
    same_reset, AccountRef, By, Event, EventKind, Hosted, Hosts, Range, Sample, WindowRecord,
};
use crate::money::Valuer;
use crate::projects::ProjectId;
use crate::transcripts::Tokens;

use super::{
    account_abbr, account_chosen, account_id, account_position, account_weeks_valued,
    blocked_by_week, blocked_spans, hours_text, lost_percent, money_of, money_text, percent_text,
    range_of, switch_counts, top_three, week_columns, working_hours, working_rhythm, AccountWeek,
    Ctx, Query, RangeKey, StatsError, SwitchCounts,
};

/// The Weeks page: the grid of account weeks and the curves of the last 14
/// days.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeksPage {
    /// The calendar weeks of the range, Monday 00:00 local, the same columns
    /// the Overview shows.
    pub week_starts: Vec<DateTime<Utc>>,
    pub rows: Vec<WeeksRow>,
    /// The working hours blocked in each column's week; a week without a span
    /// is a measured 0.0.
    pub blocked: Vec<f64>,
    /// One curve per row: the weekly percent of the last 14 days.
    pub recent: Vec<Curve>,
}

/// One account's row of the grid.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeksRow {
    pub id: String,
    pub plan: Option<String>,
    /// One cell per column; `None` when the account has no finished window of
    /// that week. Two windows of one account in one week (its reset moved)
    /// give the cell of the later end.
    pub cells: Vec<Option<WeekCell>>,
    /// The percent of the account's newest sample whose weekly window is still
    /// running; `None` without one.
    pub now_percent: Option<f64>,
}

/// One finished account week as a grid cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekCell {
    pub end: DateTime<Utc>,
    /// The window's final percent.
    pub used: f64,
    /// The share that expired: the window's `lost` percent.
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
    /// `used / paid × 100` of the priced account weeks; 0.0 when the week has
    /// no priced account week.
    pub used_percent_of_paid: f64,
    /// Money used of the priced account weeks.
    pub used: f64,
    /// Money expired of the priced account weeks.
    pub expired: f64,
    /// The working hours blocked in the week.
    pub blocked_hours: f64,
    pub switches: SwitchCounts,
    /// The money used per account id, priced account weeks only.
    pub per_account: Vec<(String, f64)>,
    /// The projects' share in % of the week's API value, the three largest
    /// then `other`; empty when `api_value` is `None`.
    pub top_projects: Vec<(String, f64)>,
    /// The tokens of the chosen records whose own time lies in the week.
    pub tokens: Tokens,
    /// The chosen records' API value; `None` when a record of the week has
    /// none, or the week has no chosen record.
    pub api_value: Option<f64>,
    /// The change of `used` against the week before, in percent; `None` when
    /// the week itself has no priced account week (a running week has no
    /// finished window) or the week before has none.
    pub used_change_percent: Option<f64>,
    /// At most three sentences, in the order the plan gives them.
    pub conclusion: Vec<String>,
}

/// One finished weekly window, with what the page shows about it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDetail {
    pub id: String,
    pub end: DateTime<Utc>,
    /// The window's final percent.
    pub used: f64,
    /// Money used; `None` without a price for the window.
    pub used_money: Option<f64>,
    /// The share that expired: the window's `lost` percent.
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
    pub switches_in_out: u32,
    pub switches_auto: u32,
    /// `end − lastSample` of the window.
    pub last_sample_before_end_secs: i64,
    pub uncertain: bool,
    /// The account's weekly samples inside the window that name its reset.
    pub curve: Vec<(DateTime<Utc>, f64)>,
}

/// The Weeks page of the range the query names.
pub fn weeks(ctx: &Ctx, q: &Query) -> Result<WeeksPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    weeks_in(ctx, &range, q)
}

/// The Weeks page of `range`; the page's [`weeks`] takes the range from its
/// [`Query`], the tests name one.
pub(crate) fn weeks_in(ctx: &Ctx, range: &Range, q: &Query) -> Result<WeeksPage, StatsError> {
    let valuer = Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
    )?;
    let finished = account_weeks_valued(ctx, range, q, &valuer)?;
    let spans = blocked_spans(ctx, range, q)?;
    let rhythm = working_rhythm(ctx, range)?;
    let blocked_weeks = blocked_by_week(&spans, &rhythm, ctx.offset);
    let week_starts = week_columns(range, ctx.offset);
    // The samples of the last 14 days and the newest one before now: one read
    // serves both the running percents and the curves.
    let samples = ctx.history.read::<Sample>(&Range::ALL, &Hosts::All)?;
    let recent_from = ctx.now - Duration::days(14);

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
            &samples.records,
            ctx.now,
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
            &samples.records,
            ctx.now,
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

    Ok(WeeksPage {
        week_starts,
        rows,
        blocked,
        recent,
    })
}

/// One row of the grid: the account's cell of every column, the cell of the
/// account week that `finished` holds for that week, and its running percent.
fn row_for(
    id: String,
    plan: Option<String>,
    reference: &AccountRef,
    week_starts: &[DateTime<Utc>],
    finished: &[AccountWeek],
    samples: &[Hosted<Sample>],
    now: DateTime<Utc>,
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
    Ok(WeeksRow {
        id,
        plan,
        cells,
        now_percent: running_percent(samples, reference, now),
    })
}

/// One account week as the grid shows it.
fn week_cell(week: &AccountWeek) -> Result<WeekCell, StatsError> {
    let money = money_of(week)?;
    Ok(WeekCell {
        end: week.window.end,
        used: week.window.final_percent,
        expired: lost_percent(&week.window)?,
        expired_money: money.map(|money| money.expired),
        full: week.window.final_percent >= 100.0,
        uncertain: week.window.uncertain,
    })
}

/// The percent of the account's newest sample at or before `now` whose
/// `weekly_all` limit resets after `now`.
fn running_percent(
    samples: &[Hosted<Sample>],
    reference: &AccountRef,
    now: DateTime<Utc>,
) -> Option<f64> {
    samples
        .iter()
        .filter(|hosted| &hosted.record.account == reference && hosted.record.t <= now)
        .filter_map(|hosted| {
            let limit = hosted.record.limit(&LimitKind::WeeklyAll)?;
            let reset = limit.r?;
            (reset > now).then_some((hosted.record.t, limit.p))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, percent)| percent)
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
pub fn week_summary(ctx: &Ctx, q: &Query, week: DateTime<Utc>) -> Result<WeekSummary, StatsError> {
    let range = range_of(q.range, ctx)?;
    let week_range = Range {
        from: week,
        to: week + Duration::days(7),
    };
    let previous = week - Duration::days(7);
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
    )?;
    let weeks = account_weeks_valued(ctx, &reading, q, &valuer)?;

    let mut used = 0.0;
    let mut paid = 0.0;
    let mut expired = 0.0;
    let mut used_priced = false;
    let mut used_by_account: BTreeMap<AccountRef, f64> = BTreeMap::new();
    for account_week in weeks
        .iter()
        .filter(|account_week| account_week.week == week)
    {
        if let Some(money) = money_of(account_week)? {
            used += money.used;
            paid += money.paid;
            expired += money.expired;
            used_priced = true;
            *used_by_account
                .entry(account_week.account.clone())
                .or_insert(0.0) += money.used;
        }
    }

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
    let spans = blocked_spans(ctx, &blocked_range, q)?;
    let rhythm = working_rhythm(ctx, &range)?;
    let blocked_hours = blocked_by_week(&spans, &rhythm, ctx.offset)
        .get(&week)
        .copied()
        .unwrap_or(0.0);

    let events = ctx.history.read::<Event>(&Range::ALL, &Hosts::All)?;
    let switches = switch_counts(&events.records, &week_range, q.host.as_deref());

    let token_read = ctx
        .history
        .read::<crate::history::TokenRecord>(&week_range, &Hosts::All)?;
    let mut tokens = Tokens::default();
    let mut api_total = 0.0;
    let mut records_seen = false;
    let mut api_missing = false;
    let mut by_project: BTreeMap<String, f64> = BTreeMap::new();
    for hosted in &token_read.records {
        let record = &hosted.record;
        let value = valuer.value(&hosted.host, record);
        if !account_chosen(ctx, q, value.account.as_ref()) {
            continue;
        }
        records_seen = true;
        tokens += Tokens::from(record);
        match value.api {
            Some(api) => {
                api_total += api;
                let name = ctx
                    .projects
                    .of(&ProjectId::of(&record.project, &hosted.host))
                    .name;
                *by_project.entry(name).or_insert(0.0) += api;
            }
            None => api_missing = true,
        }
    }
    let api_value = (records_seen && !api_missing).then_some(api_total);
    // A share of nothing is no share: a week without API value lists none.
    let top_projects = match api_value {
        Some(total) if total > 0.0 => top_three(
            by_project
                .into_iter()
                .map(|(name, value)| (name, value / total * 100.0))
                .collect(),
        ),
        _ => Vec::new(),
    };

    let mut per_account: Vec<(String, f64)> = used_by_account
        .iter()
        .map(|(reference, money)| (account_id(ctx, reference), *money))
        .collect();
    per_account.sort_by(|a, b| {
        account_position(ctx, &a.0)
            .cmp(&account_position(ctx, &b.0))
            .then_with(|| a.0.cmp(&b.0))
    });

    let used_change_percent = if used_priced && before_priced && before != 0.0 {
        Some((used - before) / before * 100.0)
    } else {
        None
    };
    let used_percent_of_paid = if paid > 0.0 { used / paid * 100.0 } else { 0.0 };

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
        used,
        expired,
        blocked_hours,
        switches,
        per_account,
        top_projects,
        tokens,
        api_value,
        used_change_percent,
        conclusion,
    })
}

/// The account week of `week` with the largest expired share.
fn most_expired(
    weeks: &[AccountWeek],
    week: DateTime<Utc>,
) -> Result<Option<(f64, &AccountWeek)>, StatsError> {
    let mut largest: Option<(f64, &AccountWeek)> = None;
    for account_week in weeks
        .iter()
        .filter(|account_week| account_week.week == week)
    {
        let share = lost_percent(&account_week.window)?;
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

/// The detail of the finished weekly window of the stored account that ends at
/// `end`.
pub fn window_detail(
    ctx: &Ctx,
    account_id: &str,
    end: DateTime<Utc>,
) -> Result<WindowDetail, StatsError> {
    let account = ctx
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| unknown_window(account_id, end, true))?;
    let reference = AccountRef::of(account);
    let finished = ctx.history.finished_windows(&Range::ALL, &Hosts::All)?;
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
    let money = money_of(week)?;

    let events = ctx.history.read::<Event>(&Range::ALL, &Hosts::All)?;
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

    let samples = ctx.history.read::<Sample>(
        &Range {
            from: start,
            to: window.end,
        },
        &Hosts::All,
    )?;
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
        id: account_id.to_string(),
        end: window.end,
        used: window.final_percent,
        used_money: money.map(|money| money.used),
        expired: lost_percent(&window)?,
        expired_money: money.map(|money| money.expired),
        reached_100_at,
        blocked_hours_after,
        session_windows_used,
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
fn blocked_hours_after(
    ctx: &Ctx,
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
    let spans = blocked_spans(ctx, &range, &query)?;
    let rhythm = working_rhythm(ctx, &range)?;
    Ok(spans
        .iter()
        .map(|span| {
            let start = span.from.max(from);
            let end = span.to.min(window.end);
            if start < end {
                working_hours(start, end, &rhythm, ctx.offset)
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
    use crate::stats::tests::{all, tiny, utc};

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
        assert_eq!(ac.plan.as_deref(), Some("Team 5x"));
        assert_eq!(ac.cells[0].as_ref().expect("AC's W1").used, 50.0);
        assert_eq!(ac.cells[1].as_ref().expect("AC's W2").used, 90.0);

        assert_eq!(lb.now_percent, Some(30.0));
        assert_eq!(ac.now_percent, Some(40.0));
        assert_eq!(page.blocked, vec![2.0, 0.0]);
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
                    plan: Some("Team 5x".to_string()),
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
        assert!(close(summary.used, 62.308), "used {}", summary.used);
        assert!(
            close(summary.expired, 11.538),
            "expired {}",
            summary.expired
        );
        assert!(
            close(summary.used_percent_of_paid, 84.375),
            "used percent {}",
            summary.used_percent_of_paid
        );
        assert_eq!(summary.blocked_hours, 0.0);
        assert_eq!(summary.switches.user, 0);
        assert_eq!(summary.switches.auto, 1);
        assert_eq!(summary.switches.agent, 1);
        assert_eq!(summary.top_projects, vec![("atlas".to_string(), 100.0)]);
        assert_eq!(summary.tokens.out, 4_000_000);
        assert_eq!(summary.api_value, Some(80.0));
        assert!(
            close(summary.used_change_percent.expect("the change"), 8.0),
            "change {:?}",
            summary.used_change_percent
        );

        assert_eq!(summary.per_account.len(), 2);
        assert_eq!(summary.per_account[0].0, tiny.lb().id);
        assert!(
            close(summary.per_account[0].1, 33.231),
            "LB {}",
            summary.per_account[0].1
        );
        assert_eq!(summary.per_account[1].0, tiny.ac().id);
        assert!(
            close(summary.per_account[1].1, 29.077),
            "AC {}",
            summary.per_account[1].1
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
        assert_eq!(summary.used, 0.0);
        assert_eq!(summary.expired, 0.0);
        assert_eq!(summary.used_percent_of_paid, 0.0);
        assert_eq!(summary.blocked_hours, 0.0);
        assert_eq!(summary.switches, SwitchCounts::default());
        assert!(summary.per_account.is_empty());
        assert!(summary.top_projects.is_empty());
        assert_eq!(summary.tokens, Tokens::default());
        assert_eq!(summary.api_value, None);
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

    /// A running week has no finished window, so it has no priced account week
    /// and no change against the week before.
    #[test]
    fn a_running_week_has_no_change() {
        let tiny = tiny();
        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-16T00:00:00Z")).unwrap();

        assert_eq!(summary.used, 0.0);
        assert_eq!(summary.used_change_percent, None);
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
        assert_eq!(ac.plan.as_deref(), Some("Team 5x"));
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
            close(summary.used, 57.692),
            "the money of the week is found outside the range: {}",
            summary.used
        );
        assert!(
            close(summary.blocked_hours, 2.0),
            "the week of 2 Nov shows {} h blocked; the Overview shows 2 h for it",
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
            close(summary.used, 33.231),
            "LB's money of the week of 9 Nov: {}",
            summary.used
        );
        assert_eq!(
            summary.tokens.out, 1_000_000,
            "LB's tokens of the week of 9 Nov"
        );
        assert_eq!(
            summary.api_value,
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

    /// One window per account and week: the grid shows the later end, and the
    /// summary of that week counts that window only.
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
        // The reset of LB moved five days earlier: the second window is short
        // and its midpoint lies in the week of 2 Nov as well.
        second.start = Some(utc("2026-11-09T17:00:00Z"));
        second.end = utc("2026-11-10T12:00:00Z");
        second.final_percent = 60.0;
        second.lost = Some(40.0);
        second.last_sample = utc("2026-11-10T11:58:00Z");
        tiny.history.append(&second).expect("LB's second window");

        let page = weeks_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let cell = page.rows[0].cells[0].as_ref().expect("LB's W1");
        assert_eq!(cell.end, utc("2026-11-10T12:00:00Z"), "the later end");
        assert_eq!(cell.used, 60.0);

        let summary = week_summary(&tiny.ctx(), &all(), utc("2026-11-02T00:00:00Z")).unwrap();
        assert!(
            close(summary.used, 41.077),
            "the money of the week counts one window of LB: {}",
            summary.used
        );
        assert!(
            close(summary.per_account[0].1, 24.923),
            "LB's money of the week: {}",
            summary.per_account[0].1
        );
    }
}
