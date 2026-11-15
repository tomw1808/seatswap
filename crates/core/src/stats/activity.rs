//! The Rhythm and Activity pages (spec §3.5, §3.9): when the hours of the week
//! are worked, and when each host worked, idled or was off. Everything here
//! only reads.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, Timelike, Utc};
use serde::Serialize;

use crate::history::{
    Event, EventKind, Hosted, Hosts, Range, SessionRecord, TokenRecord, UpRecord,
};
use crate::live_sessions::{most_output, session_label, Values};
use crate::money::{Valued, Valuer};
use crate::projects::ProjectId;
use crate::transcripts::Tokens;

use super::projects::folder_name;
use super::{
    account_chosen, blocked_by_weekday, blocked_spans, local_day_start, local_hour_start, range_of,
    week_columns, week_start, working_rhythm, Ctx, Query, StatsError,
};

/// The Rhythm page: the heat map of the week and the averages per weekday.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RhythmPage {
    /// 168 values: the output tokens of each hour of the week, Monday 00:00
    /// local first, averaged over the range's calendar weeks.
    pub heat: Vec<f64>,
    /// Seven rows, Monday first: the working and blocked hours per week.
    pub per_day: Vec<DayRow>,
}

/// One weekday of the Rhythm page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayRow {
    /// 0 = Monday.
    pub weekday: u8,
    /// The distinct hours with tokens on that weekday, all chosen hosts
    /// together, divided by the range's calendar weeks.
    pub working_hours: f64,
    /// The hours blocked on that weekday, by the range's working rhythm,
    /// divided by the range's calendar weeks.
    pub blocked_hours: f64,
}

/// What a host was doing in one hour (spec §3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HourState {
    Off,
    Idle,
    Work,
    Unknown,
}

/// One hour of one host's lane, `[t, t + 1 h)`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HourCell {
    pub t: DateTime<Utc>,
    pub state: HourState,
    /// The output tokens per model of the hour, largest first, ties by name.
    pub by_model: Vec<(String, u64)>,
    /// The distinct sessions of the hour's records.
    pub sessions: u32,
    /// The project with the most output tokens in the hour, ties by name;
    /// `None` when the hour has no output tokens.
    pub top_project: Option<String>,
    /// The two values of the hour's records.
    pub values: Values,
}

/// One host's last 168 hours.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLane {
    pub host: String,
    /// The 168 hours that end with the hour holding the range's end, oldest
    /// first.
    pub hours: Vec<HourCell>,
    pub working: u32,
    /// The lane's output tokens over its working hours; `None` without a
    /// working hour in the lane.
    pub per_working_hour: Option<f64>,
    pub idle: u32,
    pub off: u32,
    pub unknown: u32,
    /// The host's tokens of the whole range, not only of the lane.
    pub tokens: Tokens,
    /// The two values of the host's records of the whole range, not only of
    /// the lane.
    pub values: Values,
}

/// The output tokens of one local date, per host.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayTokens {
    pub date: NaiveDate,
    /// The output tokens per host; every host of the range with 0 when it has
    /// none that day.
    pub out: Vec<(String, u64)>,
}

/// One calendar week of the Activity page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityWeek {
    /// The week's Monday 00:00 local time, as an instant.
    pub week: DateTime<Utc>,
    /// The hours with tokens per host; every host of the range with 0 when it
    /// has none that week.
    pub working_hours: Vec<(String, u32)>,
    pub tokens: Tokens,
    pub values: Values,
    /// The distinct sessions of the week's records.
    pub sessions: u32,
    /// The (project, folder) with the most output tokens; `None` without
    /// tokens.
    pub top: Option<(String, String)>,
}

/// One session of the range, largest by output tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BigSession {
    /// `<project>-<first two characters of the session id>`.
    pub label: String,
    pub project: String,
    pub host: String,
    /// The model with the most output tokens; `None` without tokens.
    pub model: Option<String>,
    /// The session's distinct hours with tokens.
    pub worked_hours: u32,
    pub tokens: Tokens,
    pub values: Values,
    /// `agents_started` of the session's newest [`SessionRecord`], any time;
    /// `None` without one (a running session has no record yet).
    pub agents: Option<u32>,
}

/// The Activity page: the lanes, the days, the weeks, the largest sessions,
/// the waited minutes and the peak hour.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPage {
    pub lanes: Vec<HostLane>,
    pub days: Vec<DayTokens>,
    pub weeks: Vec<ActivityWeek>,
    /// At most 20 sessions, largest by output tokens.
    pub largest: Vec<BigSession>,
    /// The minutes sessions waited for the user, per local date, oldest first;
    /// days without a `Waited` event are left out.
    pub waited: Vec<(NaiveDate, f64)>,
    /// The tokens of the hour with the most output tokens, all chosen hosts
    /// together; `Tokens::default()` when the range has no record.
    pub peak_hour: Tokens,
}

/// The Rhythm page of the range the query names.
pub fn rhythm(ctx: &Ctx, q: &Query) -> Result<RhythmPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    rhythm_in(ctx, &range, q)
}

/// The Rhythm page of `range`; the page's [`rhythm`] takes the range from its
/// [`Query`], the tests name one.
pub(crate) fn rhythm_in(ctx: &Ctx, range: &Range, q: &Query) -> Result<RhythmPage, StatsError> {
    let valuer = valuer(ctx, range)?;
    let tokens = ctx.history.read::<TokenRecord>(range, &Hosts::All)?;
    let entries = chosen_entries(ctx, q, range, &valuer, &tokens.records);

    let columns = week_columns(range, ctx.offset);
    let weeks = columns.len();
    if weeks == 0 {
        return Err(StatsError::Message(format!(
            "the range {} .. {} holds no calendar week",
            range.from, range.to
        )));
    }
    let mut heat = vec![0.0; 168];
    let mut hours_by_weekday: Vec<BTreeSet<DateTime<Utc>>> =
        (0..7).map(|_| BTreeSet::new()).collect();
    for entry in &entries {
        let hour = local_hour_start(entry.record.t, ctx.offset);
        let local = hour.with_timezone(&ctx.offset);
        let index = local.weekday().num_days_from_monday() as usize * 24 + local.hour() as usize;
        heat[index] += entry.record.out as f64;
        hours_by_weekday[local.weekday().num_days_from_monday() as usize].insert(hour);
    }
    for value in &mut heat {
        *value /= weeks as f64;
    }

    let spans = blocked_spans(ctx, range, q)?;
    let rhythm = working_rhythm(ctx, range)?;
    let blocked = blocked_by_weekday(&spans, &rhythm, ctx.offset);

    let mut per_day = Vec::with_capacity(7);
    for (weekday, hours) in hours_by_weekday.iter().enumerate() {
        per_day.push(DayRow {
            weekday: weekday as u8,
            working_hours: hours.len() as f64 / weeks as f64,
            blocked_hours: blocked[weekday] / weeks as f64,
        });
    }

    Ok(RhythmPage { heat, per_day })
}

/// The Activity page of the range the query names.
pub fn activity(ctx: &Ctx, q: &Query) -> Result<ActivityPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    activity_in(ctx, &range, q)
}

/// The Activity page of `range`; the page's [`activity`] takes the range from
/// its [`Query`], the tests name one.
pub(crate) fn activity_in(ctx: &Ctx, range: &Range, q: &Query) -> Result<ActivityPage, StatsError> {
    let valuer = valuer(ctx, range)?;
    // The lanes and the days end with the hour and the date that hold the
    // range's last instant (`range.to` itself is outside the range).
    let last = range.to - Duration::nanoseconds(1);
    let lane_end = local_hour_start(last, ctx.offset) + Duration::hours(1);
    let lane = Range {
        from: lane_end - Duration::hours(168),
        to: lane_end,
    };
    let day_end = local_day_start(last, ctx.offset) + Duration::days(1);
    let day_window = Range {
        from: day_end - Duration::days(14),
        to: day_end,
    };
    let read = Range {
        from: range.from.min(lane.from).min(day_window.from),
        to: range.to.max(lane.to).max(day_window.to),
    };
    let tokens = ctx.history.read::<TokenRecord>(&read, &Hosts::All)?;
    let ups = ctx.history.read::<UpRecord>(&Range::ALL, &Hosts::All)?;
    let entries = chosen_entries(ctx, q, range, &valuer, &tokens.records);
    let lane_entries = chosen_entries(ctx, q, &lane, &valuer, &tokens.records);
    let day_entries = chosen_entries(ctx, q, &day_window, &valuer, &tokens.records);
    let hosts = hosts(q, range, &tokens.records, &ups.records);

    let lanes = host_lanes(ctx, &lane, &entries, &lane_entries, &hosts, &ups.records);
    let days = days(ctx, &day_window, &day_entries, &hosts)?;
    let weeks = activity_weeks(ctx, range, &entries, &hosts);
    let largest = largest_sessions(ctx, &entries)?;
    let waited = waited_minutes(ctx, range, q)?;
    let peak_hour = peak_hour(ctx, &entries);

    Ok(ActivityPage {
        lanes,
        days,
        weeks,
        largest,
        waited,
        peak_hour,
    })
}

fn valuer(ctx: &Ctx, range: &Range) -> Result<Valuer, StatsError> {
    Ok(Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
    )?)
}

/// One chosen token record with its value.
struct Entry {
    host: String,
    record: TokenRecord,
    valued: Valued,
}

/// The token records of `range` on the chosen hosts and accounts, each valued
/// once.
fn chosen_entries(
    ctx: &Ctx,
    q: &Query,
    range: &Range,
    valuer: &Valuer,
    records: &[Hosted<TokenRecord>],
) -> Vec<Entry> {
    let mut entries = Vec::new();
    for hosted in records {
        if hosted.record.t < range.from || hosted.record.t >= range.to {
            continue;
        }
        if q.host.as_ref().is_some_and(|host| host != &hosted.host) {
            continue;
        }
        let valued = valuer.value(&hosted.host, &hosted.record);
        if !account_chosen(ctx, q, valued.account.as_ref()) {
            continue;
        }
        entries.push(Entry {
            host: hosted.host.clone(),
            record: hosted.record.clone(),
            valued,
        });
    }
    entries
}

/// The chosen hosts: `q.host`, else every host with a token or `up` record in
/// the range, sorted.
fn hosts(
    q: &Query,
    range: &Range,
    tokens: &[Hosted<TokenRecord>],
    ups: &[Hosted<UpRecord>],
) -> Vec<String> {
    if let Some(host) = &q.host {
        return vec![host.clone()];
    }
    let mut hosts: BTreeSet<String> = tokens
        .iter()
        .filter(|hosted| hosted.record.t >= range.from && hosted.record.t < range.to)
        .map(|hosted| hosted.host.clone())
        .collect();
    hosts.extend(
        ups.iter()
            .filter(|hosted| hosted.record.t >= range.from && hosted.record.t < range.to)
            .map(|hosted| hosted.host.clone()),
    );
    hosts.into_iter().collect()
}

/// The host lanes: the 168 local hours of `lane`, the window that ends with
/// the hour holding the range's last instant. `entries` are the range's chosen
/// records, for the lane's `tokens` and `values`; `lane_entries` are the
/// chosen records of the lane's own window.
fn host_lanes(
    ctx: &Ctx,
    lane: &Range,
    entries: &[Entry],
    lane_entries: &[Entry],
    hosts: &[String],
    ups: &[Hosted<UpRecord>],
) -> Vec<HostLane> {
    let first = lane.from;
    let end = lane.to;

    let mut first_up: BTreeMap<&str, DateTime<Utc>> = BTreeMap::new();
    let mut up_hours: BTreeSet<(String, DateTime<Utc>)> = BTreeSet::new();
    for hosted in ups {
        first_up
            .entry(hosted.host.as_str())
            .and_modify(|first| {
                if hosted.record.t < *first {
                    *first = hosted.record.t;
                }
            })
            .or_insert(hosted.record.t);
        if hosted.record.t >= first && hosted.record.t < end {
            up_hours.insert((
                hosted.host.clone(),
                local_hour_start(hosted.record.t, ctx.offset),
            ));
        }
    }

    let mut by_hour: BTreeMap<(String, DateTime<Utc>), Vec<&Entry>> = BTreeMap::new();
    for entry in lane_entries {
        by_hour
            .entry((
                entry.host.clone(),
                local_hour_start(entry.record.t, ctx.offset),
            ))
            .or_default()
            .push(entry);
    }

    let mut totals: BTreeMap<&str, (Tokens, Vec<Valued>)> = BTreeMap::new();
    for entry in entries {
        let (tokens, valued) = totals.entry(entry.host.as_str()).or_default();
        *tokens += Tokens::from(&entry.record);
        valued.push(entry.valued.clone());
    }

    let mut lanes = Vec::with_capacity(hosts.len());
    for host in hosts {
        let mut hours = Vec::with_capacity(168);
        let (mut working, mut idle, mut off, mut unknown) = (0, 0, 0, 0);
        let mut lane_out = 0;
        for step in 0..168 {
            let t = first + Duration::hours(step);
            let key = (host.clone(), t);
            let mut by_model: BTreeMap<&str, u64> = BTreeMap::new();
            let mut sessions: BTreeSet<&str> = BTreeSet::new();
            let mut by_project: BTreeMap<ProjectId, u64> = BTreeMap::new();
            let mut valued = Vec::new();
            let mut has_tokens = false;
            if let Some(bucket) = by_hour.get(&key) {
                for entry in bucket {
                    has_tokens = true;
                    lane_out += entry.record.out;
                    *by_model.entry(entry.record.model.as_str()).or_insert(0) += entry.record.out;
                    sessions.insert(entry.record.session.as_str());
                    let project = ctx
                        .projects
                        .of(&ProjectId::of(&entry.record.project, &entry.host));
                    *by_project.entry(project.key).or_insert(0) += entry.record.out;
                    valued.push(entry.valued.clone());
                }
            }
            let on = up_hours.contains(&key);
            let state = if has_tokens {
                HourState::Work
            } else if on {
                HourState::Idle
            } else if first_up.get(host.as_str()).is_some_and(|first| t >= *first) {
                HourState::Off
            } else {
                HourState::Unknown
            };
            match state {
                HourState::Work => working += 1,
                HourState::Idle => idle += 1,
                HourState::Off => off += 1,
                HourState::Unknown => unknown += 1,
            }
            let mut by_model: Vec<(String, u64)> = by_model
                .into_iter()
                .map(|(model, out)| (model.to_string(), out))
                .collect();
            by_model.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            hours.push(HourCell {
                t,
                state,
                by_model,
                sessions: sessions.len() as u32,
                top_project: most_output(&by_project).map(|key| ctx.projects.of(&key).name),
                values: Values::of(&valued, ctx.settings),
            });
        }

        let per_working_hour = if working > 0 {
            Some(lane_out as f64 / working as f64)
        } else {
            None
        };
        let (tokens, valued) = totals.get(host.as_str()).cloned().unwrap_or_default();
        lanes.push(HostLane {
            host: host.clone(),
            hours,
            working,
            per_working_hour,
            idle,
            off,
            unknown,
            tokens,
            values: Values::of(&valued, ctx.settings),
        });
    }
    lanes
}

/// The output tokens per local date of `window`, the 14 days that end with the
/// date holding the range's last instant, oldest first.
fn days(
    ctx: &Ctx,
    window: &Range,
    entries: &[Entry],
    hosts: &[String],
) -> Result<Vec<DayTokens>, StatsError> {
    let end = (window.to - Duration::days(1))
        .with_timezone(&ctx.offset)
        .date_naive();
    let mut by_day: BTreeMap<(String, NaiveDate), u64> = BTreeMap::new();
    for entry in entries {
        *by_day
            .entry((
                entry.host.clone(),
                entry.record.t.with_timezone(&ctx.offset).date_naive(),
            ))
            .or_insert(0) += entry.record.out;
    }
    let mut days = Vec::with_capacity(14);
    for back in (0..14).rev() {
        let date = end.checked_sub_days(Days::new(back)).ok_or_else(|| {
            StatsError::Message(format!(
                "the date {back} days before {end} lies outside the calendar"
            ))
        })?;
        let out = hosts
            .iter()
            .map(|host| {
                let out = by_day.get(&(host.clone(), date)).copied().unwrap_or(0);
                (host.clone(), out)
            })
            .collect();
        days.push(DayTokens { date, out });
    }
    Ok(days)
}

/// One row per calendar week of the range.
fn activity_weeks(
    ctx: &Ctx,
    range: &Range,
    entries: &[Entry],
    hosts: &[String],
) -> Vec<ActivityWeek> {
    let columns = week_columns(range, ctx.offset);
    let mut by_week: BTreeMap<DateTime<Utc>, Vec<&Entry>> =
        columns.iter().map(|week| (*week, Vec::new())).collect();
    for entry in entries {
        by_week
            .entry(week_start(entry.record.t, ctx.offset))
            .or_default()
            .push(entry);
    }

    let mut weeks = Vec::with_capacity(columns.len());
    for week in columns {
        let records = by_week.get(&week).map(Vec::as_slice).unwrap_or(&[]);
        let mut hours: BTreeMap<&str, BTreeSet<DateTime<Utc>>> = hosts
            .iter()
            .map(|host| (host.as_str(), BTreeSet::new()))
            .collect();
        let mut tokens = Tokens::default();
        let mut valued = Vec::new();
        let mut sessions: BTreeSet<(String, String)> = BTreeSet::new();
        let mut by_folder: BTreeMap<(ProjectId, String), u64> = BTreeMap::new();
        for entry in records {
            hours
                .entry(entry.host.as_str())
                .or_default()
                .insert(local_hour_start(entry.record.t, ctx.offset));
            tokens += Tokens::from(&entry.record);
            valued.push(entry.valued.clone());
            sessions.insert((entry.host.clone(), entry.record.session.clone()));
            let project = ctx
                .projects
                .of(&ProjectId::of(&entry.record.project, &entry.host));
            let folder = folder_name(&project, &entry.host, &entry.record.cwd);
            *by_folder.entry((project.key, folder)).or_insert(0) += entry.record.out;
        }
        let working_hours = hosts
            .iter()
            .map(|host| {
                (
                    host.clone(),
                    hours
                        .get(host.as_str())
                        .map(|hours| hours.len() as u32)
                        .unwrap_or(0),
                )
            })
            .collect();
        let top = most_output(&by_folder).map(|(key, folder)| (ctx.projects.of(&key).name, folder));
        weeks.push(ActivityWeek {
            week,
            working_hours,
            tokens,
            values: Values::of(&valued, ctx.settings),
            sessions: sessions.len() as u32,
            top,
        });
    }
    weeks
}

/// The sessions with records in the range, by output tokens, largest first, at
/// most 20.
fn largest_sessions(ctx: &Ctx, entries: &[Entry]) -> Result<Vec<BigSession>, StatsError> {
    #[derive(Default)]
    struct Sum {
        host: String,
        session: String,
        project: String,
        tokens: Tokens,
        hours: BTreeSet<DateTime<Utc>>,
        by_model: BTreeMap<String, u64>,
        valued: Vec<Valued>,
    }

    let mut sums: BTreeMap<(String, String), Sum> = BTreeMap::new();
    for entry in entries {
        let key = (entry.host.clone(), entry.record.session.clone());
        let sum = sums.entry(key).or_insert_with(|| Sum {
            host: entry.host.clone(),
            session: entry.record.session.clone(),
            project: ctx
                .projects
                .of(&ProjectId::of(&entry.record.project, &entry.host))
                .name,
            ..Sum::default()
        });
        sum.tokens += Tokens::from(&entry.record);
        sum.hours
            .insert(local_hour_start(entry.record.t, ctx.offset));
        *sum.by_model.entry(entry.record.model.clone()).or_insert(0) += entry.record.out;
        sum.valued.push(entry.valued.clone());
    }

    let read = ctx
        .history
        .read::<SessionRecord>(&Range::ALL, &Hosts::All)?;
    let mut agents: BTreeMap<(String, String), (DateTime<Utc>, u32)> = BTreeMap::new();
    for hosted in &read.records {
        let record = &hosted.record;
        agents
            .entry((hosted.host.clone(), record.session.clone()))
            .and_modify(|newest| {
                if record.t > newest.0 {
                    *newest = (record.t, record.agents_started);
                }
            })
            .or_insert((record.t, record.agents_started));
    }

    let mut largest: Vec<BigSession> = sums
        .into_values()
        .map(|sum| {
            let agents_started = agents
                .get(&(sum.host.clone(), sum.session.clone()))
                .map(|(_, started)| *started);
            BigSession {
                label: session_label(&sum.project, &sum.session),
                project: sum.project,
                host: sum.host,
                model: most_output(&sum.by_model),
                worked_hours: sum.hours.len() as u32,
                tokens: sum.tokens,
                values: Values::of(&sum.valued, ctx.settings),
                agents: agents_started,
            }
        })
        .collect();
    largest.sort_by(|a, b| {
        b.tokens
            .out
            .cmp(&a.tokens.out)
            .then_with(|| a.label.cmp(&b.label))
    });
    largest.truncate(20);
    Ok(largest)
}

/// The minutes sessions waited for the user, per local date.
fn waited_minutes(
    ctx: &Ctx,
    range: &Range,
    q: &Query,
) -> Result<Vec<(NaiveDate, f64)>, StatsError> {
    let read = ctx.history.read::<Event>(range, &Hosts::All)?;
    let mut by_day: BTreeMap<NaiveDate, u64> = BTreeMap::new();
    for hosted in &read.records {
        if hosted.record.t < range.from || hosted.record.t >= range.to {
            continue;
        }
        if q.host.as_ref().is_some_and(|host| host != &hosted.host) {
            continue;
        }
        if let EventKind::Waited { secs, .. } = &hosted.record.kind {
            *by_day
                .entry(hosted.record.t.with_timezone(&ctx.offset).date_naive())
                .or_insert(0) += secs;
        }
    }
    Ok(by_day
        .into_iter()
        .map(|(date, secs)| (date, secs as f64 / 60.0))
        .collect())
}

/// The tokens of the hour with the most output tokens, all chosen records
/// together; `Tokens::default()` without a record.
fn peak_hour(ctx: &Ctx, entries: &[Entry]) -> Tokens {
    let mut by_hour: BTreeMap<DateTime<Utc>, Tokens> = BTreeMap::new();
    for entry in entries {
        *by_hour
            .entry(local_hour_start(entry.record.t, ctx.offset))
            .or_default() += Tokens::from(&entry.record);
    }
    let mut best: Option<Tokens> = None;
    let mut best_out = 0;
    for tokens in by_hour.into_values() {
        if best.is_none() || tokens.out > best_out {
            best = Some(tokens);
            best_out = tokens.out;
        }
    }
    best.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use chrono::FixedOffset;

    use crate::history::HistoryDir;
    use crate::project::ProjectKey;
    use crate::stats::tests::{all, tiny, utc};
    use crate::stats::{Range, RangeKey};

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

    fn atlas() -> ProjectKey {
        ProjectKey::Repo("github.com/sam/atlas".to_string())
    }

    /// A token record the test adds to a history.
    fn record(t: &str, session: &str, cwd: &str, project: ProjectKey, out: u64) -> TokenRecord {
        TokenRecord {
            t: utc(t),
            session: session.to_string(),
            cwd: cwd.to_string(),
            project,
            model: "claude-opus-5-5".to_string(),
            input: 0,
            out,
            cw: 0,
            cr: 0,
            answers: 1,
            agent_answers: 0,
        }
    }

    fn lane<'a>(page: &'a ActivityPage, host: &str) -> &'a HostLane {
        page.lanes
            .iter()
            .find(|lane| lane.host == host)
            .unwrap_or_else(|| panic!("the {host} lane"))
    }

    fn hour<'a>(lane: &'a HostLane, t: &str) -> &'a HourCell {
        let at = utc(t);
        lane.hours
            .iter()
            .find(|cell| cell.t == at)
            .unwrap_or_else(|| panic!("the hour {t}"))
    }

    #[test]
    fn hour_states_on_3_nov() {
        let tiny = tiny();
        // A range of two days ends on 3 Nov, so the lane holds that day.
        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-04T00:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        let atlas = lane(&page, "atlas");
        assert_eq!(
            hour(atlas, "2026-11-03T07:00:00Z").state,
            HourState::Unknown
        );
        assert_eq!(hour(atlas, "2026-11-03T08:00:00Z").state, HourState::Idle);
        assert_eq!(hour(atlas, "2026-11-03T09:00:00Z").state, HourState::Work);
        assert_eq!(hour(atlas, "2026-11-03T10:00:00Z").state, HourState::Work);
        assert_eq!(hour(atlas, "2026-11-03T11:00:00Z").state, HourState::Idle);
        assert_eq!(hour(atlas, "2026-11-03T13:00:00Z").state, HourState::Off);
    }

    #[test]
    fn tokens_without_up_records_are_work() {
        let tiny = tiny();
        // A range that ends on 8 Nov, so the lane holds that day.
        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-09T00:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        assert_eq!(
            hour(lane(&page, "atlas"), "2026-11-08T09:00:00Z").state,
            HourState::Work
        );
    }

    #[test]
    fn an_hour_names_models_sessions_and_the_top_project() {
        let tiny = tiny();
        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let cell = hour(lane(&page, "atlas"), "2026-11-14T10:00:00Z");
        assert_eq!(
            cell.by_model,
            vec![("claude-opus-5-5".to_string(), 2_000_000)]
        );
        assert_eq!(cell.sessions, 1);
        assert_eq!(cell.top_project, Some("atlas".to_string()));
        assert_eq!(cell.values.api, Some(40.0));
    }

    #[test]
    fn weeks_sum_per_host() {
        let tiny = tiny();
        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let w1 = page
            .weeks
            .iter()
            .find(|week| week.week == utc("2026-11-02T00:00:00Z"))
            .expect("the week of 2 Nov");
        assert_eq!(w1.working_hours, vec![("atlas".to_string(), 3)]);
        assert_eq!(w1.tokens.out, 3_000_000);
        assert_eq!(w1.top, Some(("atlas".to_string(), "engine".to_string())));

        let w2 = page
            .weeks
            .iter()
            .find(|week| week.week == utc("2026-11-09T00:00:00Z"))
            .expect("the week of 9 Nov");
        assert_eq!(w2.working_hours, vec![("atlas".to_string(), 3)]);
        assert_eq!(w2.top, Some(("atlas".to_string(), "frontend".to_string())));
    }

    #[test]
    fn the_largest_sessions() {
        let tiny = tiny();
        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let first = &page.largest[0];
        assert_eq!(first.label, "atlas-s3");
        assert_eq!(first.tokens.out, 3_000_000);
        assert_eq!(first.worked_hours, 2);
    }

    /// A 30-day range holds tokens of the weeks before the lane too. The
    /// lane's `per_working_hour` divides only the lane's own output tokens by
    /// its working hours; a host without a working hour in the lane has none.
    #[test]
    fn per_working_hour_counts_the_lanes_own_tokens() {
        let tiny = tiny();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&UpRecord {
            t: utc("2026-11-14T10:00:00Z"),
        })
        .expect("the vega up record");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        // 19 Oct .. 19 Nov holds atlas's token records of 3 Nov (2 M),
        // 8 Nov (1 M), 11 Nov (1 M) and 14 Nov (3 M); the lane holds only
        // those of 14 Nov.
        let range = Range {
            from: utc("2026-10-19T00:00:00Z"),
            to: tiny.now,
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();

        let atlas = lane(&page, "atlas");
        assert_eq!(
            atlas.tokens.out, 7_000_000,
            "the lane's tokens cover the whole range"
        );
        assert_eq!(atlas.working, 2);
        assert_eq!(atlas.per_working_hour, Some(1_500_000.0));

        let vega = lane(&page, "vega");
        assert_eq!(vega.working, 0, "vega has no token record in the lane");
        assert_eq!(vega.per_working_hour, None);
    }

    #[test]
    fn waited_minutes_per_day() {
        let tiny = tiny();
        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 14).expect("14 Nov 2026");
        assert_eq!(page.waited, vec![(day, 10.0)]);
    }

    #[test]
    fn the_heat_map_is_in_this_hosts_local_time() {
        let tiny = tiny();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&record(
            "2026-11-03T09:00:00Z",
            "s9",
            "/home/sam/projects/atlas/engine",
            atlas(),
            1_000_000,
        ))
        .expect("the vega record");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let ctx = Ctx {
            offset: FixedOffset::east_opt(3600).expect("+01:00"),
            ..tiny.ctx()
        };
        let page = rhythm_in(&ctx, &two_weeks(), &all()).unwrap();
        // T1 at 09:00 UTC is Tuesday 10:00 local; the vega record lands in the
        // same index. The range has three local week columns.
        assert!(
            close(page.heat[24 + 10], 2_000_000.0 / 3.0),
            "heat {}",
            page.heat[24 + 10]
        );
        assert!(close(page.heat[24 + 9], 0.0));
    }

    #[test]
    fn rhythm_per_day() {
        let tiny = tiny();
        let page = rhythm_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let saturday = &page.per_day[5];
        assert_eq!(saturday.weekday, 5);
        assert!(
            close(saturday.working_hours, 1.0),
            "working {}",
            saturday.working_hours
        );
        assert!(
            close(saturday.blocked_hours, 1.0),
            "blocked {}",
            saturday.blocked_hours
        );
    }

    #[test]
    fn a_session_label_is_the_project_and_two_characters() {
        assert_eq!(
            session_label("atlas", "7c3e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34"),
            "atlas-7c"
        );
        let tiny = tiny();
        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert!(page.largest[0].label.starts_with("atlas-"));
    }

    #[test]
    fn the_account_filter_applies() {
        let tiny = tiny();
        let query = Query {
            range: RangeKey::All,
            accounts: vec![tiny.lb().id.clone()],
            host: None,
        };
        let page = activity_in(&tiny.ctx(), &two_weeks(), &query).unwrap();
        assert!(page.weeks.iter().all(|week| week
            .top
            .as_ref()
            .is_none_or(|(project, _)| project != "acme-kb")));
        assert!(
            !page
                .largest
                .iter()
                .any(|session| session.label == "acme-kb-s2"),
            "the largest sessions: {:#?}",
            page.largest
        );
    }

    // ---- proof tests of the review ----

    /// The lane holds the 168 hours that end with the hour of the range's
    /// end, so it reaches before the range's start. An hour with tokens
    /// there is a working hour (spec §3.9), not an idle one.
    #[test]
    fn an_hour_before_the_range_still_shows_its_work() {
        let tiny = tiny();
        // A range of half a day: the lane still holds the hours of 3 Nov,
        // and T1 (1,000,000 output tokens) lies before the range's start.
        let range = Range {
            from: utc("2026-11-03T12:00:00Z"),
            to: utc("2026-11-04T00:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        let cell = hour(lane(&page, "atlas"), "2026-11-03T09:00:00Z");
        assert_eq!(
            cell.state,
            HourState::Work,
            "the hour of T1, which has 1,000,000 output tokens"
        );
    }

    /// Two projects can have the same name (two repositories with the same
    /// last part). The tokens are counted per project, so the project with
    /// the most tokens wins, not the sum of the projects with one name.
    #[test]
    fn two_projects_with_the_same_name_are_not_merged() {
        let tiny = tiny();
        let omega = ProjectKey::Repo("github.com/sam/omega".to_string());
        let other_omega = ProjectKey::Repo("github.com/other/omega".to_string());
        let beta = ProjectKey::Repo("github.com/sam/beta".to_string());
        tiny.history
            .append_many(&[
                record(
                    "2026-11-03T09:00:00Z",
                    "s9",
                    "/home/sam/projects/omega",
                    omega.clone(),
                    1_500_000,
                ),
                record(
                    "2026-11-03T09:00:00Z",
                    "s9",
                    "/home/sam/projects/omega2",
                    other_omega,
                    1_500_000,
                ),
                record(
                    "2026-11-03T09:00:00Z",
                    "s10",
                    "/home/sam/projects/beta",
                    beta,
                    2_500_000,
                ),
            ])
            .expect("the three records");
        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-04T00:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();

        // The hour of 3 Nov 09:00 holds T1 (atlas, 1,000,000), the two
        // "omega" projects (1,500,000 each) and "beta" (2,500,000).
        let cell = hour(lane(&page, "atlas"), "2026-11-03T09:00:00Z");
        assert_eq!(
            cell.top_project,
            Some("beta".to_string()),
            "the project with the most tokens of the hour"
        );

        // The same for the week of 2 Nov.
        let week = page
            .weeks
            .iter()
            .find(|week| week.week == utc("2026-11-02T00:00:00Z"))
            .expect("the week of 2 Nov");
        assert_eq!(
            week.top,
            Some(("beta".to_string(), "(top folder)".to_string())),
            "the project with the most tokens of the week"
        );
    }

    /// A local offset that is not a whole hour: the hours of the week are
    /// the local ones (the heat map), so the hours with tokens of a day are
    /// local hours too.
    #[test]
    fn the_working_hours_of_a_day_are_local_hours() {
        let tiny = tiny();
        tiny.history
            .append_many(&[
                record(
                    "2026-11-03T09:30:00Z",
                    "s9",
                    "/home/sam/projects/atlas/engine",
                    atlas(),
                    1_000_000,
                ),
                record(
                    "2026-11-03T10:30:00Z",
                    "s10",
                    "/home/sam/projects/atlas/engine",
                    atlas(),
                    1_000_000,
                ),
            ])
            .expect("the two records");
        let ctx = Ctx {
            offset: FixedOffset::east_opt(5 * 3600 + 1800).expect("+05:30"),
            ..tiny.ctx()
        };
        let page = rhythm_in(&ctx, &two_weeks(), &all()).unwrap();
        // Tuesday: T1 is at 14:30 local, T2 (10:00 UTC) and the record of
        // 09:30 UTC are in the local hour 15, the record of 10:30 UTC in the
        // local hour 16. The range touches three local weeks with this
        // offset.
        assert!(
            close(page.heat[24 + 15], 2_000_000.0 / 3.0),
            "heat {}",
            page.heat[24 + 15]
        );
        assert!(
            close(page.per_day[1].working_hours, 3.0 / 3.0),
            "working {}",
            page.per_day[1].working_hours
        );
    }

    /// The days reach before the range too: a day before the range's start
    /// shows the tokens of its own window.
    #[test]
    fn a_day_before_the_range_shows_its_tokens() {
        let tiny = tiny();
        // A range of half a day: 3 Nov still lies in the last 14 days, and
        // T1 and T2 (1,000,000 output tokens each) lie before its start.
        let range = Range {
            from: utc("2026-11-03T12:00:00Z"),
            to: utc("2026-11-04T00:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 11, 3).expect("3 Nov 2026");
        let day = page
            .days
            .iter()
            .find(|day| day.date == date)
            .expect("the day of 3 Nov");
        assert_eq!(day.out, vec![("atlas".to_string(), 2_000_000)]);
    }

    /// With an offset that is not a whole hour, a lane cell starts at the
    /// local hour start: the cell that holds 09:30 UTC starts at 09:30 UTC.
    #[test]
    fn a_lane_hour_is_a_local_hour() {
        let tiny = tiny();
        tiny.history
            .append(&record(
                "2026-11-03T09:30:00Z",
                "s9",
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the record");
        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-04T00:00:00Z"),
        };
        let ctx = Ctx {
            offset: FixedOffset::east_opt(5 * 3600 + 1800).expect("+05:30"),
            ..tiny.ctx()
        };
        let page = activity_in(&ctx, &range, &all()).unwrap();
        let cell = hour(lane(&page, "atlas"), "2026-11-03T09:30:00Z");
        assert_eq!(cell.state, HourState::Work);
        // The local hour 15:00..16:00 holds T2 (10:00 UTC) and the record of
        // 09:30 UTC.
        assert_eq!(
            cell.by_model,
            vec![("claude-opus-5-5".to_string(), 2_000_000)]
        );
    }

    /// A session lives on one host: the same id on two hosts is two sessions,
    /// and each reads the agents of its own host's `SessionRecord`.
    #[test]
    fn the_same_session_id_on_two_hosts_is_two_sessions() {
        let tiny = tiny();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&record(
            "2026-11-14T10:00:00Z",
            "s3",
            "/home/sam/projects/atlas/frontend",
            atlas(),
            2_000_000,
        ))
        .expect("the vega record");
        vega.append(&SessionRecord {
            t: utc("2026-11-14T12:06:00Z"),
            session: "s3".to_string(),
            cwd: "/home/sam/projects/atlas/frontend".to_string(),
            project: atlas(),
            entrypoint: Some("cli".to_string()),
            first: utc("2026-11-14T10:00:00Z"),
            last: utc("2026-11-14T12:00:00Z"),
            agents_started: 2,
            agents_finished: 0,
            closed: true,
        })
        .expect("the vega session");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let range = Range {
            from: utc("2026-11-13T00:00:00Z"),
            to: utc("2026-11-15T00:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        let week = page
            .weeks
            .iter()
            .find(|week| week.week == utc("2026-11-09T00:00:00Z"))
            .expect("the week of 9 Nov");
        assert_eq!(week.sessions, 2, "atlas s3 and vega s3");
        let atlas = page
            .largest
            .iter()
            .find(|session| session.host == "atlas")
            .expect("the atlas s3");
        let vega = page
            .largest
            .iter()
            .find(|session| session.host == "vega")
            .expect("the vega s3");
        assert_eq!(
            atlas.agents,
            Some(0),
            "atlas's own s3 record has started no agent"
        );
        assert_eq!(vega.agents, Some(2));
    }

    /// A running session has tokens but no [`SessionRecord`] yet, so its
    /// agents are not known: `None`, not 0.
    #[test]
    fn a_session_without_a_record_has_no_agents() {
        let tiny = tiny();
        tiny.history
            .append(&record(
                "2026-11-15T10:00:00Z",
                "s9",
                "/home/sam/projects/atlas/engine",
                atlas(),
                5_000_000,
            ))
            .expect("the s9 record");
        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let s9 = page
            .largest
            .iter()
            .find(|session| session.label == "atlas-s9")
            .expect("the s9 session");
        assert_eq!(s9.agents, None);
    }

    /// A range that ends at midnight (Last week) ends its lanes with the hour
    /// before and its days with the date before: `range.to` is outside the
    /// range.
    #[test]
    fn a_range_that_ends_at_midnight_ends_with_the_day_before() {
        let tiny = tiny();
        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-09T00:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        let cells = &lane(&page, "atlas").hours;
        assert_eq!(cells.len(), 168);
        assert_eq!(cells[167].t, utc("2026-11-08T23:00:00Z"));
        assert_eq!(cells[0].t, utc("2026-11-02T00:00:00Z"));
        assert_eq!(
            page.days.last().map(|day| day.date.to_string()),
            Some("2026-11-08".to_string())
        );
    }
}
