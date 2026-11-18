//! The Rhythm and Activity pages (spec §3.5, §3.9): when the hours of the week
//! are worked, and when each host worked, idled or was off. Everything here
//! only reads.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, TimeZone, Timelike, Utc};
use serde::Serialize;

use crate::history::{
    Event, EventKind, Hosted, Hosts, OllamaHourRecord, Range, SessionRecord, TokenRecord, UpRecord,
};
use crate::live_sessions::{most_output, session_label, Values};
use crate::money::{Company, Valued, Valuer};
use crate::projects::ProjectId;
use crate::transcripts::Tokens;

use super::projects::folder_name;
use super::{
    blocked_by_weekday, blocked_spans, keep_warnings, local_date_start, local_day_start,
    local_hour_start, next_local_day_start, range_of, record_chosen, week_columns, week_start,
    working_rhythm, BlockedSpan, Ctx, Query, StatsError,
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
    /// The distinct hours with Claude tokens on that weekday, all chosen hosts
    /// together, divided by the number of that weekday's whole local days
    /// inside the range. A provider's hours do not count: the blocked hours
    /// beside it are Claude's too (decision P-2 5). `None` when no whole day of
    /// that weekday lies inside the range: it has not ended there.
    pub working_hours: Option<f64>,
    /// The hours Claude was blocked on that weekday, inside Claude's working
    /// hours, in the same whole days and divided by the same number; `None`
    /// when the weekday has not ended in the range.
    pub blocked_hours: Option<f64>,
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
    /// Whether this is the week that holds [`Ctx::now`], the unfinished week.
    pub running: bool,
}

/// One session of the range, largest by output tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BigSession {
    /// `<project>-<first two characters of the session id>`.
    pub label: String,
    /// Claude Code's own title of the session from its newest
    /// [`SessionRecord`]; `None` without one or without a title. The page
    /// names the session by it, with the label as a tag.
    pub title: Option<String>,
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

/// What Ollama counted beyond the sessions seatswap saw, for the Activity
/// page's one-line note under the hour chart. The chart itself draws only the
/// hosts' lanes: Ollama's account total is never a lane of its own.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaUse {
    /// Summed over the UTC hours whose start is inside the page's range: the
    /// input + output of Ollama's largest line for the hour minus the input +
    /// output of the known sessions on Ollama models in it. An hour whose
    /// known sessions show more than Ollama's figure adds nothing (never a
    /// negative); an hour without an Ollama line adds nothing. 0 when Ollama
    /// counted nothing beyond the known sessions, or reported no line at all:
    /// the page then shows no note.
    pub other_tokens: u64,
}

/// The Activity page: the lanes, the days, the weeks, the largest sessions,
/// the waited minutes and the peak hour.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPage {
    pub lanes: Vec<HostLane>,
    pub days: Vec<DayTokens>,
    /// One row per calendar week of the range; empty when the range holds no
    /// token record of the chosen accounts and hosts.
    pub weeks: Vec<ActivityWeek>,
    /// At most 20 sessions, largest by output tokens.
    pub largest: Vec<BigSession>,
    /// The minutes sessions waited for the user, per local date, oldest first;
    /// days without a `Waited` event are left out.
    pub waited: Vec<(NaiveDate, f64)>,
    /// The tokens of the hour with the most output tokens, all chosen hosts
    /// together; `Tokens::default()` when the range has no record.
    pub peak_hour: Tokens,
    /// The tokens of the whole range, summed over all chosen hosts.
    pub total_tokens: Tokens,
    /// The two values of the whole range, summed over all chosen hosts. A sum
    /// is all or nothing, like every [`Values::sum`].
    pub total_values: Values,
    /// All chosen hosts' output tokens of the last 7 days over their working
    /// hours of those days; `None` without a working hour in the lanes.
    pub per_working_hour: Option<f64>,
    /// The "What it shows" sentence, computed from the lanes: a night run and
    /// a host used in the evening and at the weekend.
    pub what_it_shows: String,
    /// What Ollama counted beyond the known sessions on Ollama models (N3).
    pub ollama: OllamaUse,
}

/// The Rhythm page of the range the query names.
pub fn rhythm<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, q: &Query) -> Result<RhythmPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    rhythm_in(ctx, &range, q)
}

/// The Rhythm page of `range`; the page's [`rhythm`] takes the range from its
/// [`Query`], the tests name one.
pub(crate) fn rhythm_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
) -> Result<RhythmPage, StatsError> {
    let valuer = valuer(ctx, range)?;
    let tokens = keep_warnings(ctx.warnings, || {
        ctx.history.read::<TokenRecord>(range, &Hosts::All)
    })?;
    // The heat map counts every company's work, as it did before P-2: only the
    // per-weekday working hours and the blocked hours use the Claude filter
    // (review I, decision P-2 5).
    let entries = chosen_entries(ctx, q, range, &valuer, &tokens.records);

    let columns = week_columns(range, &ctx.zone);
    let weeks = columns.len();
    if weeks == 0 {
        return Err(StatsError::Message(format!(
            "the range {} .. {} holds no calendar week",
            range.from, range.to
        )));
    }
    // A weekday's divisor is the number of that weekday's whole local days
    // inside `range`: from its 00:00 to the next 00:00, both within the range.
    // A weekday without such a day has not ended in the range: it is `None`,
    // not 0. The whole days found are always one contiguous run (consecutive
    // dates, increasing starts), so `ended_from..ended_to` is their union. The
    // numerator below counts only the hours inside these same days, so
    // numerator and divisor cover the same days.
    let mut whole_days = [0u32; 7];
    let mut ended_from: Option<DateTime<Utc>> = None;
    let mut ended_to: Option<DateTime<Utc>> = None;
    let mut date = range.from.with_timezone(&ctx.zone).date_naive();
    loop {
        let start = local_date_start(date, &ctx.zone);
        if start >= range.to {
            break;
        }
        let end = next_local_day_start(start, &ctx.zone);
        if start >= range.from && end <= range.to {
            whole_days[date.weekday().num_days_from_monday() as usize] += 1;
            ended_from.get_or_insert(start);
            ended_to = Some(end);
        }
        date += Duration::days(1);
    }
    let mut heat = vec![0.0; 168];
    let mut hours_by_weekday: Vec<BTreeSet<DateTime<Utc>>> =
        (0..7).map(|_| BTreeSet::new()).collect();
    for entry in &entries {
        let hour = local_hour_start(entry.record.t, &ctx.zone);
        let local = hour.with_timezone(&ctx.zone);
        let index = local.weekday().num_days_from_monday() as usize * 24 + local.hour() as usize;
        heat[index] += entry.record.out as f64;
        if let (Some(from), Some(to)) = (ended_from, ended_to) {
            if entry.record.t >= from
                && entry.record.t < to
                && entry.valued.company == Company::Anthropic
            {
                hours_by_weekday[local.weekday().num_days_from_monday() as usize].insert(hour);
            }
        }
    }
    for value in &mut heat {
        *value /= weeks as f64;
    }

    let spans = blocked_spans(ctx, range, q, Company::Anthropic)?;
    let rhythm = working_rhythm(ctx, range, Company::Anthropic)?;
    // The blocked hours use the same whole days as the working hours: a block
    // outside them (in a day that has not ended) counts for nothing.
    let ended_spans: Vec<BlockedSpan> = match (ended_from, ended_to) {
        (Some(from), Some(to)) => spans
            .iter()
            .filter_map(|span| {
                let start = span.from.max(from);
                let end = span.to.min(to);
                (start < end).then(|| BlockedSpan {
                    host: span.host.clone(),
                    account: span.account.clone(),
                    from: start,
                    to: end,
                    // `blocked_by_weekday` recomputes the hours from `from`
                    // and `to`; it does not read these two fields.
                    hours: 0.0,
                    avoidable_hours: 0.0,
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    let blocked = blocked_by_weekday(&ended_spans, &rhythm, &ctx.zone);

    let mut per_day = Vec::with_capacity(7);
    for (weekday, hours) in hours_by_weekday.iter().enumerate() {
        let divisor = whole_days[weekday] as f64;
        let (working_hours, blocked_hours) = if whole_days[weekday] == 0 {
            (None, None)
        } else {
            (
                Some(hours.len() as f64 / divisor),
                Some(blocked[weekday] / divisor),
            )
        };
        per_day.push(DayRow {
            weekday: weekday as u8,
            working_hours,
            blocked_hours,
        });
    }

    Ok(RhythmPage { heat, per_day })
}

/// The Activity page of the range the query names.
pub fn activity<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, q: &Query) -> Result<ActivityPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    activity_in(ctx, &range, q)
}

/// The Activity page of `range`; the page's [`activity`] takes the range from
/// its [`Query`], the tests name one.
pub(crate) fn activity_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
) -> Result<ActivityPage, StatsError> {
    let valuer = valuer(ctx, range)?;
    // The lanes and the days end with the hour and the date that hold the
    // range's last instant (`range.to` itself is outside the range).
    let last = range.to - Duration::nanoseconds(1);
    let lane_end = local_hour_start(last, &ctx.zone) + Duration::hours(1);
    let lane = Range {
        from: lane_end - Duration::hours(168),
        to: lane_end,
    };
    let last_date = local_day_start(last, &ctx.zone)
        .with_timezone(&ctx.zone)
        .date_naive();
    let day_end = next_local_day_start(last, &ctx.zone);
    let day_window = Range {
        from: local_date_start(last_date - Duration::days(13), &ctx.zone),
        to: day_end,
    };
    let read = Range {
        from: range.from.min(lane.from).min(day_window.from),
        to: range.to.max(lane.to).max(day_window.to),
    };
    let tokens = keep_warnings(ctx.warnings, || {
        ctx.history.read::<TokenRecord>(&read, &Hosts::All)
    })?;
    let ups = keep_warnings(ctx.warnings, || {
        ctx.history.read::<UpRecord>(&Range::ALL, &Hosts::All)
    })?;
    let ollama = keep_warnings(ctx.warnings, || {
        ctx.history.read::<OllamaHourRecord>(&read, &Hosts::All)
    })?;
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
    let (total_tokens, total_values, per_working_hour) = lane_totals(&lanes);
    let what_it_shows = what_it_shows_text(ctx, &lanes);
    let ollama = ollama_use(range, &ollama.records, &tokens.records);

    Ok(ActivityPage {
        lanes,
        days,
        weeks,
        largest,
        waited,
        peak_hour,
        total_tokens,
        total_values,
        per_working_hour,
        what_it_shows,
        ollama,
    })
}

/// The sums over the lanes: the whole range's tokens and values, and the last
/// 7 days' output tokens over the lanes' working hours.
fn lane_totals(lanes: &[HostLane]) -> (Tokens, Values, Option<f64>) {
    let mut tokens = Tokens::default();
    let mut values = Vec::with_capacity(lanes.len());
    let mut lane_out = 0u64;
    let mut working = 0u32;
    for lane in lanes {
        tokens += lane.tokens;
        values.push(&lane.values);
        lane_out += lane
            .hours
            .iter()
            .flat_map(|cell| cell.by_model.iter().map(|(_, out)| *out))
            .sum::<u64>();
        working += lane.working;
    }
    let per_working_hour = if working > 0 {
        Some(lane_out as f64 / working as f64)
    } else {
        None
    };
    (tokens, Values::sum(&values), per_working_hour)
}

/// "What it shows": the work the lanes show. A night is an hour of work before
/// 05:00 local time, and one host is named for its nights; every other host is
/// named for its evening work (a weekday from 19:00 on) and its weekend use.
/// The lanes carry no agent data, so the sentence never claims an agent ran.
/// Lanes without any hour of work say so instead of "nothing stands out".
fn what_it_shows_text<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, lanes: &[HostLane]) -> String {
    let mut parts = Vec::new();
    let mut night_host: Option<&str> = None;
    for lane in lanes {
        let mut nights: BTreeSet<NaiveDate> = BTreeSet::new();
        for cell in &lane.hours {
            if cell.state != HourState::Work {
                continue;
            }
            let local = cell.t.with_timezone(&ctx.zone);
            if local.hour() < 5 {
                nights.insert(local.date_naive());
            }
        }
        if !nights.is_empty() {
            let count = nights.len();
            parts.push(format!(
                "{} night{} with work on {}.",
                count_word(count, true),
                if count == 1 { "" } else { "s" },
                lane.host
            ));
            night_host = Some(lane.host.as_str());
            break;
        }
    }
    for lane in lanes {
        if night_host == Some(lane.host.as_str()) {
            continue;
        }
        let mut evenings: BTreeSet<usize> = BTreeSet::new();
        let mut weekend = false;
        for cell in &lane.hours {
            if cell.state != HourState::Work {
                continue;
            }
            let local = cell.t.with_timezone(&ctx.zone);
            let weekday = local.weekday().num_days_from_monday() as usize;
            if weekday >= 5 {
                weekend = true;
            } else if local.hour() >= 19 {
                evenings.insert(weekday);
            }
        }
        let evenings_text = match evenings.len() {
            0 => None,
            count => Some(format!(
                "on {} evening{}",
                count_word(count, false),
                if count == 1 { "" } else { "s" }
            )),
        };
        match (evenings_text, weekend) {
            (Some(evenings), true) => parts.push(format!(
                "{} is used {} and at the weekend.",
                lane.host, evenings
            )),
            (Some(evenings), false) => parts.push(format!("{} is used {}.", lane.host, evenings)),
            (None, true) => parts.push(format!("{} is used at the weekend.", lane.host)),
            (None, false) => {}
        }
    }
    if !parts.is_empty() {
        return parts.join(" ");
    }
    let worked = lanes
        .iter()
        .any(|lane| lane.hours.iter().any(|cell| cell.state == HourState::Work));
    if worked {
        "No night work, evening or weekend use stands out in the last 7 days.".to_string()
    } else {
        "No work recorded in the last 7 days.".to_string()
    }
}

/// A small count as a word, so the sentence reads like the mock-up's; bigger
/// counts keep their digits.
fn count_word(count: usize, capital: bool) -> String {
    const WORDS: [&str; 10] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    let word = if count < WORDS.len() {
        WORDS[count].to_string()
    } else {
        count.to_string()
    };
    if capital {
        let mut chars = word.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => word,
        }
    } else {
        word
    }
}

fn valuer<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, range: &Range) -> Result<Valuer, StatsError> {
    Ok(Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
        ctx.provider_subscriptions,
        ctx.warnings,
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
fn chosen_entries<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
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
        if !record_chosen(ctx, q, &valued) {
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
fn host_lanes<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
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
                local_hour_start(hosted.record.t, &ctx.zone),
            ));
        }
    }

    let mut by_hour: BTreeMap<(String, DateTime<Utc>), Vec<&Entry>> = BTreeMap::new();
    for entry in lane_entries {
        by_hour
            .entry((
                entry.host.clone(),
                local_hour_start(entry.record.t, &ctx.zone),
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
            let t = local_hour_start(first + Duration::hours(step), &ctx.zone);
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
                values: Values::of(&valued, ctx.settings, ctx.fx, ctx.now),
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
            values: Values::of(&valued, ctx.settings, ctx.fx, ctx.now),
        });
    }
    lanes
}

/// The output tokens per local date of `window`, the 14 days that end with the
/// date holding the range's last instant, oldest first.
fn days<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    window: &Range,
    entries: &[Entry],
    hosts: &[String],
) -> Result<Vec<DayTokens>, StatsError> {
    let end = (window.to - Duration::nanoseconds(1))
        .with_timezone(&ctx.zone)
        .date_naive();
    let mut by_day: BTreeMap<(String, NaiveDate), u64> = BTreeMap::new();
    for entry in entries {
        *by_day
            .entry((
                entry.host.clone(),
                entry.record.t.with_timezone(&ctx.zone).date_naive(),
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

/// One row per calendar week of the range; no row at all when the range holds
/// no token record, so the table says that instead of showing a row of zeros.
fn activity_weeks<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    entries: &[Entry],
    hosts: &[String],
) -> Vec<ActivityWeek> {
    if entries.is_empty() {
        return Vec::new();
    }
    let columns = week_columns(range, &ctx.zone);
    let mut by_week: BTreeMap<DateTime<Utc>, Vec<&Entry>> =
        columns.iter().map(|week| (*week, Vec::new())).collect();
    for entry in entries {
        by_week
            .entry(week_start(entry.record.t, &ctx.zone))
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
                .insert(local_hour_start(entry.record.t, &ctx.zone));
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
            values: Values::of(&valued, ctx.settings, ctx.fx, ctx.now),
            sessions: sessions.len() as u32,
            top,
            running: week == week_start(ctx.now, &ctx.zone),
        });
    }
    weeks
}

/// The sessions with records in the range, by output tokens, largest first, at
/// most 20.
fn largest_sessions<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    entries: &[Entry],
) -> Result<Vec<BigSession>, StatsError> {
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
            .insert(local_hour_start(entry.record.t, &ctx.zone));
        *sum.by_model.entry(entry.record.model.clone()).or_insert(0) += entry.record.out;
        sum.valued.push(entry.valued.clone());
    }

    let read = keep_warnings(ctx.warnings, || {
        ctx.history.read::<SessionRecord>(&Range::ALL, &Hosts::All)
    })?;
    // The newest session record of each session: its time, agents and title.
    let mut newest_records: BTreeMap<(String, String), &SessionRecord> = BTreeMap::new();
    for hosted in &read.records {
        let record = &hosted.record;
        newest_records
            .entry((hosted.host.clone(), record.session.clone()))
            .and_modify(|newest| {
                if record.t > newest.t {
                    *newest = record;
                }
            })
            .or_insert(record);
    }

    let mut largest: Vec<BigSession> = sums
        .into_values()
        .map(|sum| {
            let newest = newest_records.get(&(sum.host.clone(), sum.session.clone()));
            BigSession {
                label: session_label(&sum.project, &sum.session),
                title: newest.and_then(|record| record.title.clone()),
                project: sum.project,
                host: sum.host,
                model: most_output(&sum.by_model),
                worked_hours: sum.hours.len() as u32,
                tokens: sum.tokens,
                values: Values::of(&sum.valued, ctx.settings, ctx.fx, ctx.now),
                agents: newest.map(|record| record.agents_started),
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
fn waited_minutes<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
) -> Result<Vec<(NaiveDate, f64)>, StatsError> {
    let read = keep_warnings(ctx.warnings, || {
        ctx.history.read_claude::<Event>(range, &Hosts::All)
    })?;
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
                .entry(hosted.record.t.with_timezone(&ctx.zone).date_naive())
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
fn peak_hour<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, entries: &[Entry]) -> Tokens {
    let mut by_hour: BTreeMap<DateTime<Utc>, Tokens> = BTreeMap::new();
    for entry in entries {
        *by_hour
            .entry(local_hour_start(entry.record.t, &ctx.zone))
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

/// Whether a token record's model is one of Ollama's. The history's
/// [`TokenRecord`] keeps the model and no provider field, so the model name is
/// the one per-row signal: a model ending in `:cloud` is an Ollama cloud model
/// (the names H-1 records, e.g. `deepseek-v4.1-flash:cloud`).
fn is_ollama_model(model: &str) -> bool {
    model.ends_with(":cloud")
}

/// The hour's largest Ollama line across the hosts that wrote one.
///
/// A bucket only grows while its hour runs, so a later answer has no smaller
/// count; the largest line is therefore the newest. Several lines for one hour
/// (the same host writing again, or two hosts polling one Ollama account) are
/// never summed. Ties fall back to the larger cached input, requests and USD,
/// then to the host name that sorts first.
fn largest_ollama_line<'a>(lines: &[&'a Hosted<OllamaHourRecord>]) -> Option<&'a OllamaHourRecord> {
    let total = |hosted: &Hosted<OllamaHourRecord>| hosted.record.input + hosted.record.output;
    lines
        .iter()
        .copied()
        .max_by(|a, b| {
            total(a)
                .cmp(&total(b))
                .then_with(|| a.record.cached_input.cmp(&b.record.cached_input))
                .then_with(|| a.record.requests.cmp(&b.record.requests))
                .then_with(|| a.record.usd.total_cmp(&b.record.usd))
                .then_with(|| b.host.cmp(&a.host))
        })
        .map(|hosted| &hosted.record)
}

/// The start of the whole UTC hour that holds `t`.
fn utc_hour_start(t: DateTime<Utc>) -> DateTime<Utc> {
    local_hour_start(t, &Utc)
}

/// The input + output of the known sessions on Ollama models, per UTC hour.
///
/// Ollama's buckets are whole UTC hours, so a session's tokens belong to the
/// UTC hour that holds its instant, not to the local hour the lanes draw.
fn tracked_ollama(records: &[Hosted<TokenRecord>]) -> BTreeMap<DateTime<Utc>, u64> {
    let mut by_hour: BTreeMap<DateTime<Utc>, u64> = BTreeMap::new();
    for hosted in records {
        if !is_ollama_model(&hosted.record.model) {
            continue;
        }
        *by_hour.entry(utc_hour_start(hosted.record.t)).or_insert(0) +=
            hosted.record.input + hosted.record.out;
    }
    by_hour
}

/// What Ollama counted beyond the known sessions on Ollama models (N3).
///
/// `records` are every host's [`OllamaHourRecord`] over the window the page
/// reads, which holds the page's range. `sessions` is every host's and every
/// account's [`TokenRecord`] over the same window, not the page's chosen
/// entries: Ollama's line belongs to the whole Ollama account, so the known
/// sessions subtracted from it are every session on an Ollama model, whatever
/// host or account the page filters to.
///
/// Ollama's lines are whole UTC hours: the known sessions of a UTC hour are
/// subtracted from that same UTC hour's bucket, and only the UTC hours whose
/// start is inside `range`, the page's chosen range, are summed. An hour whose
/// known sessions exceed Ollama's figure adds nothing, never a negative.
///
/// Ollama's line for an hour is the LARGEST line across all hosts, never a sum
/// over hosts: two hosts may poll one Ollama account and write the same bucket.
fn ollama_use(
    range: &Range,
    records: &[Hosted<OllamaHourRecord>],
    sessions: &[Hosted<TokenRecord>],
) -> OllamaUse {
    let mut lines: BTreeMap<DateTime<Utc>, Vec<&Hosted<OllamaHourRecord>>> = BTreeMap::new();
    for hosted in records {
        lines
            .entry(utc_hour_start(hosted.record.hour))
            .or_default()
            .push(hosted);
    }
    let tracked = tracked_ollama(sessions);

    let mut other_tokens = 0;
    for (hour, lines) in &lines {
        if *hour < range.from || *hour >= range.to {
            continue;
        }
        let Some(record) = largest_ollama_line(lines) else {
            continue;
        };
        let total = record.input + record.output;
        let tracked = tracked.get(hour).copied().unwrap_or(0);
        // The known sessions showing more than Ollama's figure is a mismatch
        // of the two counts (Review Focus 5), not use beyond them.
        if tracked <= total {
            other_tokens += total - tracked;
        }
    }

    OllamaUse { other_tokens }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use chrono::FixedOffset;

    use crate::api::LimitKind;
    use crate::history::HistoryDir;
    use crate::project::ProjectKey;
    use crate::stats::tests::{all, codex_token, tiny, utc};
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
            minutes: 60,
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

    /// An Ollama hour line, as the provider thread writes it.
    fn ollama_line(hour: &str, input: u64, output: u64) -> OllamaHourRecord {
        OllamaHourRecord {
            hour: utc(hour),
            requests: 1,
            input,
            cached_input: 0,
            output,
            usd: 0.01,
        }
    }

    /// A token record of a known session on an Ollama cloud model.
    fn ollama_record(t: &str, session: &str, input: u64, out: u64) -> TokenRecord {
        TokenRecord {
            t: utc(t),
            minutes: 60,
            session: session.to_string(),
            cwd: "/home/sam/projects/atlas/engine".to_string(),
            project: atlas(),
            model: "deepseek-v4.1-flash:cloud".to_string(),
            input,
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

    /// A query for one host: the sentence test reads only that host's lane.
    fn host_query(host: &str) -> Query {
        Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: Some(host.to_string()),
        }
    }

    /// A Codex record still appears in the Rhythm heat map, as every company's
    /// work did before P-2; only the per-weekday working hours and the blocked
    /// hours use the Claude filter (review I).
    #[test]
    fn a_codex_record_still_appears_in_the_heat_map() {
        let tiny = tiny();
        let mut record = codex_token("2026-11-11T14:00:00Z", "gpt-6.2-codex", 1_000_000);
        record.out = 2_000_000;
        tiny.history.append(&record).expect("the Codex record");

        let page = rhythm_in(&tiny.ctx(), &two_weeks(), &all()).expect("the Rhythm page");
        let index = 2 * 24 + 14; // Wednesday 14:00 UTC
        assert!(
            close(page.heat[index], 1_000_000.0),
            "the Codex hour is in the heat map over its two weeks: {:?}",
            page.heat[index]
        );
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

    /// The page's total is the sum of every chosen host's tokens and values,
    /// and a total has its own number in the view: the page never adds two
    /// hosts itself.
    #[test]
    fn the_total_is_the_sum_over_hosts() {
        let tiny = tiny();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&record(
            "2026-11-14T10:00:00Z",
            "v-1",
            "/home/sam/projects/atlas/engine",
            atlas(),
            2_000_000,
        ))
        .expect("the vega record");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(page.lanes.len(), 2, "the page sums over both hosts");

        let mut tokens = Tokens::default();
        let mut lane_out = 0u64;
        let mut working = 0u32;
        for lane in &page.lanes {
            tokens += lane.tokens;
            lane_out += lane
                .hours
                .iter()
                .flat_map(|cell| cell.by_model.iter().map(|(_, out)| *out))
                .sum::<u64>();
            working += lane.working;
        }
        // A `Values` sum keeps the known part when a host misses a half (R3):
        // the total is the sum of the hosts that have it, marked partial.
        let known_subscription: f64 = page
            .lanes
            .iter()
            .filter_map(|lane| lane.values.subscription)
            .sum();
        let known_api: f64 = page.lanes.iter().filter_map(|lane| lane.values.api).sum();
        assert_eq!(page.total_tokens, tokens);
        assert_eq!(
            page.total_tokens.out,
            lane(&page, "atlas").tokens.out + lane(&page, "vega").tokens.out
        );
        assert!(
            close(
                page.total_values
                    .subscription
                    .expect("a known subscription part"),
                known_subscription
            ),
            "the subscription total is the known part over hosts {}",
            known_subscription
        );
        assert!(
            page.total_values.subscription_partial,
            "a host without a subscription part makes the total partial"
        );
        assert!(
            close(page.total_values.api.expect("a known API part"), known_api),
            "the API total is the known part over hosts"
        );
        assert!(
            close(
                page.per_working_hour.expect("a working hour in the lanes"),
                lane_out as f64 / working as f64
            ),
            "the per-working-hour total is all hosts' lane output over their working hours"
        );
    }

    /// The unfinished week is the one that holds `Ctx::now`; an earlier week
    /// of the same page is not running.
    #[test]
    fn the_current_week_is_marked_running() {
        let tiny = tiny();
        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: tiny.now,
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        let current = page
            .weeks
            .iter()
            .find(|week| week.week == utc("2026-11-16T00:00:00Z"))
            .expect("the week that holds now");
        assert!(current.running, "the unfinished week is running");
        for week in &page.weeks {
            if week.week == current.week {
                continue;
            }
            assert!(
                !week.running,
                "the finished week {} is not running",
                week.week
            );
        }
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
            zone: FixedOffset::east_opt(3600).expect("+01:00"),
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

    /// The Rhythm page's working hours and blocked hours use one company
    /// filter (decision P-2 5): a Codex hour is neither a working hour nor a
    /// blocked one, while Claude's own block in a Claude hour counts.
    #[test]
    fn rhythm_working_hours_and_blocked_hours_use_one_company_filter() {
        let mut tiny = tiny();
        let codex = crate::stats::tests::codex_ref();
        crate::stats::tests::codex_prices(&mut tiny.prices);
        // Codex works on Wednesday at 10:00, when no Claude record is.
        tiny.history
            .append(&crate::stats::tests::codex_token(
                "2026-11-04T10:00:00Z",
                "gpt-6.2-codex",
                1_000_000,
            ))
            .expect("the Codex record");
        // A Claude block at Tuesday 10:00, a Claude working hour, and a Codex
        // block at Wednesday 10:00, a Codex working hour.
        let block = |at: &str, account: &crate::history::AccountRef, host: &HistoryDir| {
            host.append(&Event {
                t: utc(at),
                kind: EventKind::Blocked {
                    account: account.clone(),
                    k: LimitKind::Session,
                    m: None,
                },
            })
            .expect("the block");
            host.append(&Event {
                t: utc(at) + Duration::hours(1),
                kind: EventKind::Unblocked {
                    account: account.clone(),
                    k: LimitKind::Session,
                    m: None,
                },
            })
            .expect("the unblock");
        };
        block("2026-11-03T10:00:00Z", &tiny.lb_ref(), &tiny.history);
        block("2026-11-04T10:00:00Z", &codex, &tiny.history);

        // One calendar week, so the page divides by 1: Monday 2 Nov .. Monday
        // 9 Nov.
        let week = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-09T00:00:00Z"),
        };
        let page = rhythm_in(&tiny.ctx(), &week, &all()).unwrap();

        // Tuesday holds Claude's two token hours, 09:00 and 10:00, and the
        // Claude block in the 10:00 hour. One whole Tuesday is in the week.
        assert_eq!(page.per_day[1].working_hours, Some(2.0));
        assert_eq!(page.per_day[1].blocked_hours, Some(1.0));
        // Wednesday holds Codex work and a Codex block alone.
        assert_eq!(page.per_day[2].working_hours, Some(0.0));
        assert_eq!(page.per_day[2].blocked_hours, Some(0.0));
    }

    #[test]
    fn rhythm_per_day() {
        let tiny = tiny();
        let page = rhythm_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let saturday = &page.per_day[5];
        assert_eq!(saturday.weekday, 5);
        assert!(
            close(saturday.working_hours.expect("two whole Saturdays"), 1.0),
            "working {:?}",
            saturday.working_hours
        );
        assert!(
            close(saturday.blocked_hours.expect("two whole Saturdays"), 1.0),
            "blocked {:?}",
            saturday.blocked_hours
        );
    }

    /// Monday and Tuesday have each fully ended inside this range, so they
    /// divide by 1; Wednesday ends at Thursday 00:00, after the range's end,
    /// so it has not ended and is `None`, not 0.
    #[test]
    fn a_weekday_counts_once_per_time_it_fully_ended_in_the_range() {
        let tiny = tiny();
        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-04T12:00:00Z"),
        };
        let page = rhythm_in(&tiny.ctx(), &range, &all()).unwrap();

        assert_eq!(page.per_day[0].working_hours, Some(0.0));
        assert_eq!(page.per_day[1].working_hours, Some(2.0));
        assert_eq!(page.per_day[2].working_hours, None);
    }

    /// Blocked hours divide by the same count of whole weekdays: Tuesday's
    /// two-hour block over the one Tuesday in the range. Wednesday has work
    /// and a block in an unfinished day; both count for nothing, so the day is
    /// `None`, not a partial average.
    #[test]
    fn blocked_hours_use_the_same_divisor() {
        let tiny = tiny();
        tiny.history
            .append(&record(
                "2026-11-04T09:00:00Z",
                "s9",
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the unfinished day's record");
        let block = |from: &str, to: &str| {
            tiny.history
                .append(&Event {
                    t: utc(from),
                    kind: EventKind::Blocked {
                        account: tiny.lb_ref(),
                        k: LimitKind::Session,
                        m: None,
                    },
                })
                .expect("the block");
            tiny.history
                .append(&Event {
                    t: utc(to),
                    kind: EventKind::Unblocked {
                        account: tiny.lb_ref(),
                        k: LimitKind::Session,
                        m: None,
                    },
                })
                .expect("the unblock");
        };
        block("2026-11-03T09:00:00Z", "2026-11-03T11:00:00Z");
        block("2026-11-04T09:00:00Z", "2026-11-04T10:00:00Z");

        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-04T12:00:00Z"),
        };
        let page = rhythm_in(&tiny.ctx(), &range, &all()).unwrap();

        assert_eq!(page.per_day[1].working_hours, Some(2.0));
        assert_eq!(page.per_day[1].blocked_hours, Some(2.0));
        assert_eq!(page.per_day[2].working_hours, None);
        assert_eq!(page.per_day[2].blocked_hours, None);
    }

    /// Two lines five minutes apart lie in one local hour: the weekday counts
    /// one working hour, not two.
    #[test]
    fn hours_stay_whole_hours() {
        let tiny = tiny();
        let mut first = record(
            "2026-11-02T09:00:00Z",
            "m1",
            "/home/sam/projects/atlas/engine",
            atlas(),
            1_000_000,
        );
        first.minutes = 5;
        let mut second = record(
            "2026-11-02T09:05:00Z",
            "m1",
            "/home/sam/projects/atlas/engine",
            atlas(),
            1_000_000,
        );
        second.minutes = 5;
        tiny.history
            .append_many(&[first, second])
            .expect("the two lines");

        let range = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-04T12:00:00Z"),
        };
        let page = rhythm_in(&tiny.ctx(), &range, &all()).unwrap();
        assert_eq!(
            page.per_day[0].working_hours,
            Some(1.0),
            "two 5-minute lines in one hour are one working hour"
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
            zone: FixedOffset::east_opt(5 * 3600 + 1800).expect("+05:30"),
            ..tiny.ctx()
        };
        let page = rhythm_in(&ctx, &two_weeks(), &all()).unwrap();
        // Tuesday: T1 is at 14:30 local, T2 (10:00 UTC) and the record of
        // 09:30 UTC are in the local hour 15, the record of 10:30 UTC in the
        // local hour 16. The range touches three local weeks with this
        // offset; only two whole Tuesdays lie inside it, so the weekday
        // divides by 2 (the heat map keeps its three calendar weeks).
        assert!(
            close(page.heat[24 + 15], 2_000_000.0 / 3.0),
            "heat {}",
            page.heat[24 + 15]
        );
        assert!(
            close(
                page.per_day[1].working_hours.expect("two whole Tuesdays"),
                3.0 / 2.0
            ),
            "working {:?}",
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
            zone: FixedOffset::east_opt(5 * 3600 + 1800).expect("+05:30"),
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
            title: Some("Fix the invoice export".to_string()),
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
        // The title comes from the session's own host's newest record, too.
        assert_eq!(vega.title.as_deref(), Some("Fix the invoice export"));
        assert_eq!(atlas.title, None);
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

    /// A second host that runs the same account: its records share the account's
    /// week, so the week's used money is split between the hosts and the sum
    /// over the hosts counts it once.
    #[test]
    fn the_total_over_hosts_counts_the_used_money_of_a_week_once() {
        let tiny = tiny();
        // vega runs the same account (LB) as atlas and adds one record to the
        // same week (9 Nov 17:00 .. 16 Nov 17:00, 80 % used).
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&Event {
            t: utc("2026-11-10T08:00:00Z"),
            kind: EventKind::Live {
                account: Some(tiny.lb_ref()),
            },
        })
        .expect("the vega live event");
        vega.append(&record(
            "2026-11-12T09:00:00Z",
            "s5",
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

        let range = Range {
            from: utc("2026-11-10T00:00:00Z"),
            to: utc("2026-11-12T12:00:00Z"),
        };
        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        // The week's used money: 80 % of the week's price of 180 EUR a month.
        let week = 80.0 * 180.0 * 12.0 / 52.0 / 100.0;
        let total = page
            .total_values
            .subscription
            .expect("a subscription value");
        assert!(
            close(total, week),
            "the total over the hosts is {total}, the week's used money is {week}"
        );
        // The API value of the two records of the week: 20 + 20.
        assert!(close(page.total_values.api.expect("an API value"), 40.0));
        // Each host's share is a part of that one week's money.
        let atlas = lane(&page, "atlas")
            .values
            .subscription
            .expect("the atlas value");
        let vega = lane(&page, "vega")
            .values
            .subscription
            .expect("the vega value");
        assert!(
            close(atlas + vega, week),
            "the two hosts' values add up to {} , not to {week}",
            atlas + vega
        );
        assert!(close(atlas, 0.5 * week), "atlas's share is {atlas}");
        assert!(close(vega, 0.5 * week), "vega's share is {vega}");
    }

    /// The totals follow the range: the lanes are the same 168 hours for every
    /// range, but the tokens and values of the range are not.
    #[test]
    fn the_totals_follow_the_range() {
        let tiny = tiny();
        let week = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };
        let wider = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };
        let one = activity_in(&tiny.ctx(), &week, &all()).unwrap();
        let two = activity_in(&tiny.ctx(), &wider, &all()).unwrap();
        assert!(
            one.total_tokens.out < two.total_tokens.out,
            "the wider range holds more tokens: {} and {}",
            one.total_tokens.out,
            two.total_tokens.out
        );
        assert!(
            one.total_values.subscription.expect("a value")
                < two.total_values.subscription.expect("a value"),
            "the wider range holds more value"
        );
        // The lanes are the same 168 hours for both ranges.
        assert_eq!(one.lanes[0].hours[0].t, two.lanes[0].hours[0].t);
    }

    /// The "What it shows" sentence: two nights on atlas, and vega's evenings
    /// and weekend use.
    #[test]
    fn the_what_it_shows_sentence_names_the_lanes() {
        let tiny = tiny();
        // atlas works before 05:00 on two days.
        tiny.history
            .append(&record(
                "2026-11-12T02:00:00Z",
                "s7",
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the first night");
        tiny.history
            .append(&record(
                "2026-11-13T03:00:00Z",
                "s8",
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the second night");
        // vega runs the same account and works on three evenings and at the
        // weekend.
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&Event {
            t: utc("2026-11-10T08:00:00Z"),
            kind: EventKind::Live {
                account: Some(tiny.lb_ref()),
            },
        })
        .expect("the vega live event");
        for (at, session) in [
            ("2026-11-10T19:00:00Z", "s10"),
            ("2026-11-11T19:00:00Z", "s11"),
            ("2026-11-12T19:00:00Z", "s12"),
            ("2026-11-14T13:00:00Z", "s13"),
        ] {
            vega.append(&record(
                at,
                session,
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the vega record");
        }
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(
            page.what_it_shows,
            "Two nights with work on atlas. vega is used on three evenings and at the weekend."
        );
    }

    /// A host without weekend work still gets its evenings named: work from
    /// 19:00 on a weekday is what the sentence shows.
    #[test]
    fn the_what_it_shows_sentence_names_evenings_without_a_weekend() {
        let tiny = tiny();
        tiny.history
            .append(&record(
                "2026-11-12T02:00:00Z",
                "s7",
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the night record");
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&UpRecord {
            t: utc("2026-11-10T08:00:00Z"),
        })
        .expect("the vega up record");
        for (at, session) in [
            ("2026-11-10T19:00:00Z", "s10"),
            ("2026-11-11T19:00:00Z", "s11"),
            ("2026-11-12T19:00:00Z", "s12"),
        ] {
            vega.append(&record(
                at,
                session,
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the vega record");
        }
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(
            page.what_it_shows,
            "One night with work on atlas. vega is used on three evenings."
        );
    }

    /// Weekend work without an evening is named on its own.
    #[test]
    fn the_what_it_shows_sentence_names_weekend_use_without_evenings() {
        let tiny = tiny();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&UpRecord {
            t: utc("2026-11-14T08:00:00Z"),
        })
        .expect("the vega up record");
        vega.append(&record(
            "2026-11-14T13:00:00Z",
            "s13",
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

        let page = activity_in(&tiny.ctx(), &two_weeks(), &host_query("vega")).unwrap();
        assert_eq!(page.what_it_shows, "vega is used at the weekend.");
    }

    /// A host with neither a night, an evening nor weekend work is not named.
    #[test]
    fn the_what_it_shows_sentence_leaves_out_a_host_with_neither() {
        let tiny = tiny();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&UpRecord {
            t: utc("2026-11-11T08:00:00Z"),
        })
        .expect("the vega up record");
        vega.append(&record(
            "2026-11-11T10:00:00Z",
            "s14",
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

        let page = activity_in(&tiny.ctx(), &two_weeks(), &host_query("vega")).unwrap();
        assert_eq!(
            page.what_it_shows,
            "No night work, evening or weekend use stands out in the last 7 days."
        );
        assert!(
            !page.what_it_shows.contains("vega"),
            "a host with neither is not named: {}",
            page.what_it_shows
        );
    }

    /// Ollama's total input + output for an hour minus the known sessions on
    /// Ollama models in it is what Ollama counted beyond them. A Claude record
    /// of the same hour is not subtracted.
    #[test]
    fn other_use_is_the_rest_of_the_hour() {
        let tiny = tiny();
        // Ollama's line for 14 Nov 10:00: 600,000 input + 400,000 output.
        tiny.history
            .append(&ollama_line("2026-11-14T10:00:00Z", 600_000, 400_000))
            .expect("the Ollama line");
        // A known session on an Ollama model in the same hour: 100,000 input
        // + 150,000 output. The fixture's Claude record of 2,000,000 output
        // tokens in that hour is not Ollama's.
        tiny.history
            .append(&ollama_record(
                "2026-11-14T10:05:00Z",
                "a9",
                100_000,
                150_000,
            ))
            .expect("the Ollama token record");

        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(page.ollama.other_tokens, 750_000);
    }

    /// Review Focus 5: an hour whose known sessions used more Ollama tokens
    /// than Ollama reported adds nothing (never a negative that would eat
    /// another hour's rest). An hour with sessions on Ollama models but no
    /// Ollama line adds nothing either.
    #[test]
    fn more_tracked_than_reported_adds_nothing() {
        let tiny = tiny();
        // 14 Nov 09:00: Ollama reported 300,000 tokens, no known session.
        tiny.history
            .append(&ollama_line("2026-11-14T09:00:00Z", 200_000, 100_000))
            .expect("the Ollama line with a rest");
        // 14 Nov 10:00: Ollama reported 100,000 tokens; the known session
        // shows 150,000.
        tiny.history
            .append(&ollama_line("2026-11-14T10:00:00Z", 60_000, 40_000))
            .expect("the Ollama line");
        tiny.history
            .append(&ollama_record(
                "2026-11-14T10:20:00Z",
                "a9",
                100_000,
                50_000,
            ))
            .expect("the more tracked record");
        // 14 Nov 11:00: an Ollama session with no Ollama line (seatswap was
        // off or no Ollama account was configured).
        tiny.history
            .append(&ollama_record("2026-11-14T11:10:00Z", "a10", 10_000, 5_000))
            .expect("the record without an Ollama line");

        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(
            page.ollama.other_tokens, 300_000,
            "only 09:00's rest: 10:00 shows more tracked than reported, 11:00 has no line"
        );
    }

    /// Two hosts polling the same Ollama account each write a line for the
    /// same hour. The hour's figure is the largest line, never the sum over
    /// hosts (a bucket only grows, so the largest is the newest).
    #[test]
    fn two_hosts_one_hour_take_the_largest() {
        let tiny = tiny();
        tiny.history
            .append(&ollama_line("2026-11-14T10:00:00Z", 600_000, 400_000))
            .expect("atlas's Ollama line");
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&ollama_line("2026-11-14T10:00:00Z", 900_000, 600_000))
            .expect("vega's Ollama line");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(
            page.ollama.other_tokens, 1_500_000,
            "the larger line (input + output), not 1,000,000 + 1,500,000"
        );
    }

    /// Ollama's buckets are whole UTC hours, while the page's hours are local.
    /// In a zone with a half-hour offset the UTC hour that holds a known
    /// session's tokens is not the hour the session looks like in the lanes;
    /// the session is still subtracted from the UTC hour that holds it.
    #[test]
    fn an_ollama_session_is_subtracted_from_the_hour_that_holds_it() {
        let tiny = tiny();
        // Ollama's bucket 10:00..11:00 UTC: 600,000 input + 400,000 output.
        tiny.history
            .append(&ollama_line("2026-11-14T10:00:00Z", 600_000, 400_000))
            .expect("the Ollama line");
        // A known session on an Ollama model inside that bucket: 10:45 UTC.
        tiny.history
            .append(&ollama_record(
                "2026-11-14T10:45:00Z",
                "a9",
                100_000,
                150_000,
            ))
            .expect("the Ollama token record");

        let ctx = Ctx {
            zone: FixedOffset::east_opt(5 * 3600 + 1800).expect("+05:30"),
            ..tiny.ctx()
        };
        let page = activity_in(&ctx, &two_weeks(), &all()).unwrap();

        // The known session's tokens are in Ollama's figure, so the rest of
        // the figure is 1,000,000 - 250,000.
        assert_eq!(
            page.ollama.other_tokens, 750_000,
            "the known session's 250,000 tokens are part of Ollama's figure"
        );
    }

    /// The range's first UTC hour counts with its own session at −03:30,
    /// where that hour starts at 20:30 local on the day before: the sum
    /// follows UTC hours, not the local hours of the lanes.
    #[test]
    fn ollama_hours_are_utc_hours_in_a_half_hour_zone() {
        let tiny = tiny();
        tiny.history
            .append(&ollama_line("2026-11-09T00:00:00Z", 600_000, 400_000))
            .expect("the Ollama line of the range's first hour");
        tiny.history
            .append(&ollama_record(
                "2026-11-09T00:45:00Z",
                "a9",
                100_000,
                150_000,
            ))
            .expect("the Ollama token record");
        let range = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-15T23:00:00Z"),
        };

        let ctx = Ctx {
            zone: FixedOffset::west_opt(3 * 3600 + 1800).expect("-03:30"),
            ..tiny.ctx()
        };
        let page = activity_in(&ctx, &range, &all()).unwrap();

        assert_eq!(page.ollama.other_tokens, 750_000);
    }

    /// Ollama's line is the whole account's, so every session on an Ollama
    /// model of every host is subtracted, whatever host the page filters to.
    #[test]
    fn ollama_sessions_of_every_host_are_subtracted() {
        let tiny = tiny();
        tiny.history
            .append(&ollama_record(
                "2026-11-14T10:05:00Z",
                "a9",
                100_000,
                150_000,
            ))
            .expect("atlas's Ollama token record");
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&ollama_record("2026-11-14T10:15:00Z", "v1", 50_000, 50_000))
            .expect("vega's Ollama token record");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");
        tiny.history
            .append(&ollama_line("2026-11-14T10:00:00Z", 600_000, 400_000))
            .expect("the Ollama line");

        let all_hosts = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let one_host = activity_in(&tiny.ctx(), &two_weeks(), &host_query("atlas")).unwrap();
        assert_eq!(
            one_host.ollama, all_hosts.ollama,
            "the Ollama numbers do not follow the host filter"
        );
        assert_eq!(
            all_hosts.ollama.other_tokens, 650_000,
            "both hosts' 350,000 session tokens are in the same UTC hour"
        );
    }

    /// `other_tokens` sums the UTC hours whose start is inside the page's
    /// range, also when the window read for the lanes and days is wider.
    #[test]
    fn other_tokens_counts_the_utc_hours_of_the_range() {
        let tiny = tiny();
        // Read for the lane and the day window, but before the range.
        tiny.history
            .append(&ollama_line("2026-11-03T00:00:00Z", 60_000, 40_000))
            .expect("the line before the range");
        tiny.history
            .append(&ollama_line("2026-11-09T00:00:00Z", 600_000, 400_000))
            .expect("the range's first hour");
        tiny.history
            .append(&ollama_line("2026-11-15T23:00:00Z", 300_000, 200_000))
            .expect("the range's last hour");
        let range = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };

        let page = activity_in(&tiny.ctx(), &range, &all()).unwrap();
        assert_eq!(
            page.ollama.other_tokens, 1_500_000,
            "the line of 3 Nov is outside the range"
        );
    }

    /// The lanes carry no agent data: an hour is "work" when it has tokens.
    /// The sentence names work, never an agent run.
    #[test]
    fn the_what_it_shows_sentence_names_work_not_an_agent_run() {
        let tiny = tiny();
        tiny.history
            .append(&record(
                "2026-11-12T02:00:00Z",
                "s7",
                "/home/sam/projects/atlas/engine",
                atlas(),
                1_000_000,
            ))
            .expect("the night record");
        let page = activity_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(page.what_it_shows, "One night with work on atlas.");
        assert!(
            !page.what_it_shows.contains("agent"),
            "the lanes have no agent data, so the sentence must not claim one: {}",
            page.what_it_shows
        );
    }

    /// A host that was on but recorded no token (a new install) has nothing
    /// that could stand out: the sentence says no work was recorded, and the
    /// Per week table has no row of zeros.
    #[test]
    fn a_host_without_work_says_so_and_has_no_week_rows() {
        let tiny = tiny();
        let vega = HistoryDir::at(tiny.dir.path(), "vega");
        vega.append(&UpRecord {
            t: utc("2026-11-14T08:00:00Z"),
        })
        .expect("the vega up record");
        fs::write(
            tiny.dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the vega marker");

        let page = activity_in(&tiny.ctx(), &two_weeks(), &host_query("vega")).unwrap();
        assert_eq!(page.what_it_shows, "No work recorded in the last 7 days.");
        assert!(
            page.weeks.is_empty(),
            "no token record, no week row: {:?}",
            page.weeks
        );
        assert_eq!(page.lanes.len(), 1, "the host keeps its lane");
        assert!(
            page.lanes[0]
                .hours
                .iter()
                .any(|cell| cell.state == HourState::Idle),
            "the up record still shows as on but idle"
        );
    }
}
