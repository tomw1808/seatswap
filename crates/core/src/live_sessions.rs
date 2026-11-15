//! The rows of the Sessions area (spec §3.8): the running sessions of this host
//! and the closed ones from the history, with their agents, tokens and the two
//! values of those tokens.

use std::collections::BTreeMap;
use std::fs;
use std::io;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::atomic::{self, Access};
use crate::burn::tied::thinking_tied;
use crate::history::{HistoryDir, HistoryError, Hosted, Hosts, Range, SessionRecord, TokenRecord};
use crate::money::{Valued, Valuer};
use crate::project::ProjectKey;
use crate::projects::{ProjectId, Projects};
use crate::sessions::{SessionFile, SessionStatus};
use crate::store::Settings;
use crate::transcripts::{self, AgentMeta, AgentSeen, ScanState, Tokens};

/// A session idle longer than this pays nothing extra for a switch (spec V11).
pub const CACHE_LIFE_MIN: i64 = 60;

/// The report is rewritten after this many seconds when the rows changed.
pub const REPORT_EVERY_SECS: i64 = 30;

/// A report older than this many minutes is stale (spec §3.8).
pub const REPORT_STALE_MIN: i64 = 15;

/// What the provider of every row is today (spec §3.8).
const PROVIDER: &str = "Claude Code";

/// The reason both values have when a session or an agent has no token record.
const NO_TOKENS: &str = "no tokens yet";

/// The reason both values have while an agent's model is not known.
const NO_AGENT_MODEL: &str = "the agent's model is not known yet";

/// Why a sum has no API value when its parts are in two currencies.
const TWO_CURRENCIES: &str =
    "the dollar rate is missing for some of these days, so the values are in two currencies";

/// Why a sum has no subscription value when its parts are valued in two
/// currencies (each host can set its own settings' currency).
const TWO_SUBSCRIPTION_CURRENCIES: &str = "these sessions are valued in two currencies";

/// What a session is doing, as the Sessions area shows it (spec §3.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LiveStatus {
    Busy,
    Idle,
    Waiting,
    Closed,
    Other(String),
}

/// The two values of some tokens, as a page shows them (spec §1.4, §3.6).
///
/// `api` is in `api_currency`: the settings' currency when the record's day has
/// a dollar rate, and "USD" when a currency is set but its rate is not known
/// yet. `subscription` is always in `currency`, the settings' currency. A
/// missing half names its reason with the text of [`crate::money::Missing`];
/// the two halves are never mixed and a sum is all or nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Values {
    pub api: Option<f64>,
    pub api_currency: Option<String>,
    pub api_missing: Option<String>,
    pub subscription: Option<f64>,
    pub currency: Option<String>,
    pub subscription_missing: Option<String>,
}

impl Values {
    /// The one way a list of [`Valued`] becomes what a page shows: one
    /// [`Values`] per record, then [`Values::sum`]. An empty list has both
    /// reasons "no tokens yet" and the settings' currency.
    pub fn of(valued: &[Valued], settings: &Settings) -> Values {
        let parts: Vec<Values> = valued
            .iter()
            .map(|part| Values {
                api: part.api,
                api_currency: part.api_currency.clone(),
                api_missing: part.api_missing.as_ref().map(ToString::to_string),
                subscription: part.subscription,
                currency: settings.currency.clone(),
                subscription_missing: part.subscription_missing.as_ref().map(ToString::to_string),
            })
            .collect();
        let parts: Vec<&Values> = parts.iter().collect();
        let mut sum = Values::sum(&parts);
        // Every part was built with the settings' currency; an empty list has
        // no part to take it from.
        sum.currency = settings.currency.clone();
        sum
    }

    /// The sum of built values, with the same rule as [`Values::of`]: each
    /// half is added only when every part has it; a missing part makes the sum
    /// unknown with the first part's reason; parts whose `api_currency` or
    /// `currency` differ are not added. The currency is the first part's; the
    /// view is built with one settings' currency, so all parts agree. An empty
    /// list has both reasons "no tokens yet".
    pub fn sum(parts: &[&Values]) -> Values {
        let mut api = Some(0.0);
        let mut api_currency: Option<String> = None;
        let mut api_missing: Option<String> = None;
        let mut two_currencies = false;
        let mut subscription = Some(0.0);
        let mut subscription_missing: Option<String> = None;
        let mut currency: Option<String> = None;
        let mut two_subscription_currencies = false;
        for part in parts {
            if let Some(part_currency) = &part.currency {
                match &currency {
                    None => currency = Some(part_currency.clone()),
                    Some(first) if first != part_currency => two_subscription_currencies = true,
                    Some(_) => {}
                }
            }
            match part.api {
                None => {
                    api = None;
                    if api_missing.is_none() {
                        api_missing = part.api_missing.clone();
                    }
                }
                Some(value) => {
                    if let Some(sum) = &mut api {
                        *sum += value;
                    }
                    match &api_currency {
                        None => api_currency = part.api_currency.clone(),
                        Some(first) if Some(first) != part.api_currency.as_ref() => {
                            two_currencies = true;
                        }
                        Some(_) => {}
                    }
                }
            }
            match part.subscription {
                None => {
                    subscription = None;
                    if subscription_missing.is_none() {
                        subscription_missing = part.subscription_missing.clone();
                    }
                }
                Some(value) => {
                    if let Some(sum) = &mut subscription {
                        *sum += value;
                    }
                }
            }
        }
        if api.is_some() && two_currencies {
            api = None;
            api_missing = Some(TWO_CURRENCIES.to_string());
        }
        if api.is_none() {
            api_currency = None;
        }
        if two_subscription_currencies {
            if subscription.is_some() {
                subscription = None;
                subscription_missing = Some(TWO_SUBSCRIPTION_CURRENCIES.to_string());
            }
            currency = None;
        }
        if parts.is_empty() {
            api = None;
            subscription = None;
            api_missing = Some(NO_TOKENS.to_string());
            subscription_missing = Some(NO_TOKENS.to_string());
        }
        Values {
            api,
            api_currency,
            api_missing,
            subscription,
            currency,
            subscription_missing,
        }
    }

    /// Both halves missing with the same reason, and the settings' currency.
    fn missing(reason: &str, settings: &Settings) -> Values {
        Values {
            api: None,
            api_currency: None,
            api_missing: Some(reason.to_string()),
            subscription: None,
            currency: settings.currency.clone(),
            subscription_missing: Some(reason.to_string()),
        }
    }

    /// Both halves missing with the same reason and no currency: the pricing
    /// itself is not known, so there is no settings' currency either.
    fn unknown(reason: &str) -> Values {
        Values {
            api: None,
            api_currency: None,
            api_missing: Some(reason.to_string()),
            subscription: None,
            currency: None,
            subscription_missing: Some(reason.to_string()),
        }
    }
}

/// How the rows' values are built (spec §3.8): with a valuer and the settings'
/// currency, or not at all because the history, the prices or the dollar rate
/// cannot be read. `Unknown` gives every row's and agent's [`Values`] both
/// halves missing with `reason` and no currency: the two facts the session
/// files alone cannot provide do not keep the rows from being built.
pub enum Pricing<'a> {
    Known {
        valuer: &'a Valuer,
        settings: &'a Settings,
    },
    Unknown {
        reason: String,
    },
}

impl Pricing<'_> {
    /// The values of a list of records for one row or agent.
    fn of(&self, valued: &[Valued]) -> Values {
        match self {
            Pricing::Known { settings, .. } => Values::of(valued, settings),
            Pricing::Unknown { reason } => Values::unknown(reason),
        }
    }

    /// The settings' currency, for the one `Values` built without a valuer.
    fn missing(&self, reason: &str) -> Values {
        match self {
            Pricing::Known { settings, .. } => Values::missing(reason, settings),
            Pricing::Unknown { reason } => Values::unknown(reason),
        }
    }
}

/// One agent of a running session (spec §3.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveAgent {
    pub agent: String,
    pub parent: Option<String>,
    pub kind: Option<String>,
    /// The meta's description, kept in memory only: this module never writes a file.
    pub task: Option<String>,
    pub model: Option<String>,
    pub running: bool,
    /// The agent's first answer; `None` until it answered.
    pub first: Option<DateTime<Utc>>,
    /// The agent's last answer; `None` until it answered.
    pub last: Option<DateTime<Utc>>,
    pub tokens: Tokens,
    pub values: Values,
}

/// One row of the Sessions area (spec §3.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSession {
    pub session: String,
    pub host: String,
    pub label: String,
    /// The project the row is counted under after joins (spec §3.6). Its name
    /// is `project`; two projects with the same name have different ids.
    pub project_id: ProjectId,
    /// The project's name after joins (spec §3.6).
    pub project: String,
    pub org: Option<String>,
    pub cwd: String,
    /// "Claude Code" today.
    pub provider: String,
    pub model: Option<String>,
    pub tied: bool,
    pub status: LiveStatus,
    /// The status's time; for a closed session, when it closed.
    pub since: DateTime<Utc>,
    pub started: DateTime<Utc>,
    pub agents_running: u32,
    pub agents_done: u32,
    pub agents: Vec<LiveAgent>,
    pub tokens: Tokens,
    pub last_hour: Tokens,
    pub values: Values,
}

/// The rows of the Sessions area, and the rows that were left out because
/// something they hold is not a valid value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rows {
    pub sessions: Vec<LiveSession>,
    pub warnings: Vec<String>,
}

/// What a switch costs a running session (spec §2.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchKind {
    Free,
    Resend,
    LosesThinking,
}

/// Closed, idle or waiting for more than [`CACHE_LIFE_MIN`], or without
/// answers → Free.
/// tied and thinking in its current task (`thinking_since_user > 0`) and not
/// Free → LosesThinking. Otherwise Resend.
pub fn switch_kind(
    session: &LiveSession,
    thinking_since_user: u32,
    now: DateTime<Utc>,
) -> SwitchKind {
    if session.status == LiveStatus::Closed {
        return SwitchKind::Free;
    }
    if session.model.is_none() {
        return SwitchKind::Free;
    }
    // A session that waits for the user sends nothing either: its stored copy
    // ages the same way as an idle one's.
    if matches!(session.status, LiveStatus::Idle | LiveStatus::Waiting)
        && now.signed_duration_since(session.since) > Duration::minutes(CACHE_LIFE_MIN)
    {
        return SwitchKind::Free;
    }
    if session.tied && thinking_since_user > 0 {
        return SwitchKind::LosesThinking;
    }
    SwitchKind::Resend
}

/// The running sessions of this host (spec §3.8). An agent is running while its
/// meta says `finished == false`.
///
/// `project_of` turns a session's `cwd` into its project key; the sessions
/// thread already caches that look-up. `metas` returns the `AgentMeta` files of
/// one session, and `host` names the history host whose timeline values these
/// records.
///
/// The values are the live view's approximation. The Statistics area values
/// every hourly history record exactly; the live view values each model's sum
/// in `seen.by_model` as one record dated at the session's last answer
/// (`seen.last`) and passes it to [`Valuer::value`]. An agent's tokens are one
/// record with its model, dated at its last answer; an agent without a model
/// has both values missing. An agent that has not answered has no time at all.
/// A session without a [`SessionSeen`] is a row with zero tokens and the values
/// of an empty list.
///
/// A session file whose `startedAt` or `statusUpdatedAt` is not a valid time is
/// not a row: the reason goes into [`Rows::warnings`], never a default time.
///
/// `projects` is `None` when the history cannot be read: a row's project then
/// has its key's default name and no organization, so the session files alone
/// still give every waiting session its label. `pricing` says how the values
/// are built, or why they are missing; it never keeps a row from being built.
#[allow(clippy::too_many_arguments)] // the plan's signature plus `project_of`
pub fn running(
    files: &[SessionFile],
    state: &ScanState,
    metas: &dyn Fn(&str) -> Vec<AgentMeta>,
    projects: Option<&Projects>,
    pricing: &Pricing,
    host: &str,
    now: DateTime<Utc>,
    project_of: &dyn Fn(&str) -> ProjectKey,
) -> Rows {
    let mut sessions = Vec::with_capacity(files.len());
    let mut warnings = Vec::new();
    for file in files {
        let Some(started) = time_ms(file.started_at) else {
            warnings.push(format!(
                "session {}: startedAt {} is not a valid time",
                file.session_id, file.started_at
            ));
            continue;
        };
        let Some(since) = time_ms(file.status_updated_at) else {
            warnings.push(format!(
                "session {}: statusUpdatedAt {} is not a valid time",
                file.session_id, file.status_updated_at
            ));
            continue;
        };
        let seen = state.sessions.get(&file.session_id);
        let key = project_of(&file.cwd);
        let id = ProjectId::of(&key, host);
        let (project_id, project_name, org) = named_project(projects, &id);

        let mut live_agents = Vec::new();
        let mut agents_running = 0u32;
        let mut agents_done = 0u32;
        let mut agents: BTreeMap<String, (Option<AgentMeta>, Option<&AgentSeen>)> = BTreeMap::new();
        for meta in metas(&file.session_id) {
            let agent = meta.agent.clone();
            agents.entry(agent).or_insert((None, None)).0 = Some(meta);
        }
        if let Some(seen) = seen {
            for (agent, agent_seen) in &seen.agents {
                agents.entry(agent.clone()).or_insert((None, None)).1 = Some(agent_seen);
            }
        }
        for (agent, (meta, agent_seen)) in agents {
            let first = agent_seen.map(|agent| agent.first);
            let last = agent_seen.map(|agent| agent.last);
            let model = agent_seen.and_then(|agent| agent.model.clone());
            let tokens = agent_seen.map(|agent| agent.tokens).unwrap_or_default();
            let is_running = meta.as_ref().map(|meta| !meta.finished).unwrap_or(false);
            if is_running {
                agents_running += 1;
            } else {
                agents_done += 1;
            }
            let values = match pricing {
                Pricing::Known { valuer, .. } => match (model.as_deref(), last) {
                    (Some(model), Some(last)) => {
                        let record =
                            token_record(&file.session_id, &file.cwd, &key, model, last, tokens);
                        pricing.of(&[valuer.value(host, &record)])
                    }
                    _ => pricing.missing(NO_AGENT_MODEL),
                },
                Pricing::Unknown { reason } => Values::unknown(reason),
            };
            live_agents.push(LiveAgent {
                agent,
                parent: meta.as_ref().and_then(|meta| meta.parent.clone()),
                kind: meta.as_ref().and_then(|meta| meta.agent_type.clone()),
                task: meta.as_ref().and_then(|meta| meta.description.clone()),
                model,
                running: is_running,
                first,
                last,
                tokens,
                values,
            });
        }

        let model = seen.and_then(|seen| seen.last_model.clone());
        let tokens = seen.map(|seen| seen.totals).unwrap_or_default();
        let last_hour = seen
            .map(|seen| transcripts::last_hour(seen, now))
            .unwrap_or_default();
        let values = match pricing {
            Pricing::Known { valuer, .. } => match seen {
                None => pricing.of(&[]),
                Some(seen) => {
                    let valued: Vec<Valued> = seen
                        .by_model
                        .iter()
                        .map(|(model, tokens)| {
                            let record = token_record(
                                &file.session_id,
                                &file.cwd,
                                &key,
                                model,
                                seen.last,
                                *tokens,
                            );
                            valuer.value(host, &record)
                        })
                        .collect();
                    pricing.of(&valued)
                }
            },
            Pricing::Unknown { reason } => Values::unknown(reason),
        };
        let label = file
            .name
            .clone()
            .unwrap_or_else(|| format!("{}-{}", project_name, short_id(&file.session_id)));
        sessions.push(LiveSession {
            session: file.session_id.clone(),
            host: host.to_string(),
            label,
            project_id,
            project: project_name,
            org,
            cwd: file.cwd.clone(),
            provider: PROVIDER.to_string(),
            tied: model.as_deref().map(thinking_tied).unwrap_or(false),
            model,
            status: live_status(&file.status),
            since,
            started,
            agents_running,
            agents_done,
            agents: live_agents,
            tokens,
            last_hour,
            values,
        });
    }
    Rows { sessions, warnings }
}

/// The sessions of every host that closed in `[since, now)` (spec §3.8). A row
/// is a session whose last `SessionRecord` has `closed: true` and whose `last`
/// is in the range; its tokens are the sum of every `TokenRecord` of the
/// session from the earliest `first` of the rows, because one hour and model
/// can have several lines, and its last hour the sum of those after an hour
/// before `now`. Its agents list is empty; the counts come from the
/// record. `label` is `<project>-<first 2 characters of the session id>`.
///
/// The history read's warnings are passed on in [`Rows::warnings`]. `projects`
/// and `pricing` mean what they do in [`running`].
pub fn closed(
    history: &HistoryDir,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    projects: Option<&Projects>,
    pricing: &Pricing,
) -> Result<Rows, HistoryError> {
    let range = Range::between(Some(since), Some(now))?;
    let read = history.read::<SessionRecord>(&range, &Hosts::All)?;
    let mut warnings = read.warnings;
    let mut last_lines: BTreeMap<(String, String), Hosted<SessionRecord>> = BTreeMap::new();
    for hosted in read.records {
        last_lines.insert((hosted.host.clone(), hosted.record.session.clone()), hosted);
    }
    let rows: Vec<&Hosted<SessionRecord>> = last_lines
        .values()
        .filter(|hosted| {
            hosted.record.closed && hosted.record.last >= since && hosted.record.last < now
        })
        .collect();
    let Some(earliest) = rows.iter().map(|hosted| hosted.record.first).min() else {
        return Ok(Rows {
            sessions: Vec::new(),
            warnings,
        });
    };
    let token_range = Range::between(Some(earliest), Some(now))?;
    let token_read = history.read::<TokenRecord>(&token_range, &Hosts::All)?;
    let token_records = token_read.records;
    warnings.extend(token_read.warnings);
    let mut tokens_of: BTreeMap<(String, String), Vec<&TokenRecord>> = BTreeMap::new();
    for hosted in &token_records {
        tokens_of
            .entry((hosted.host.clone(), hosted.record.session.clone()))
            .or_default()
            .push(&hosted.record);
    }

    let mut sessions = Vec::with_capacity(rows.len());
    for hosted in rows {
        let record = &hosted.record;
        let of_session = tokens_of
            .get(&(hosted.host.clone(), record.session.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut tokens = Tokens::default();
        let mut last_hour = Tokens::default();
        let hour_ago = now - Duration::minutes(60);
        let mut out_by_model: BTreeMap<String, u64> = BTreeMap::new();
        let mut valued = Vec::with_capacity(of_session.len());
        for token in of_session {
            let of_record = Tokens {
                input: token.input,
                out: token.out,
                cw: token.cw,
                cr: token.cr,
            };
            tokens += of_record;
            // The same window as `transcripts::last_hour`: after an hour ago.
            if token.t > hour_ago {
                last_hour += of_record;
            }
            *out_by_model.entry(token.model.clone()).or_insert(0) += token.out;
            if let Pricing::Known { valuer, .. } = pricing {
                valued.push(valuer.value(&hosted.host, token));
            }
        }
        let model = most_output(&out_by_model);
        let id = ProjectId::of(&record.project, &hosted.host);
        let (project_id, project_name, org) = named_project(projects, &id);
        let label = format!("{}-{}", project_name, short_id(&record.session));
        sessions.push(LiveSession {
            session: record.session.clone(),
            host: hosted.host.clone(),
            label,
            project_id,
            project: project_name,
            org,
            cwd: record.cwd.clone(),
            provider: PROVIDER.to_string(),
            tied: model.as_deref().map(thinking_tied).unwrap_or(false),
            model,
            status: LiveStatus::Closed,
            since: record.last,
            started: record.first,
            agents_running: 0,
            agents_done: record.agents_finished,
            agents: Vec::new(),
            tokens,
            last_hour,
            values: pricing.of(&valued),
        });
    }
    Ok(Rows { sessions, warnings })
}

/// A project's key, name and organization after the joins. Without a project
/// look-up (the history cannot be read) the row's own key, the key's default
/// name and no organization are used, so a row can always name its project.
fn named_project(
    projects: Option<&Projects>,
    id: &ProjectId,
) -> (ProjectId, String, Option<String>) {
    match projects {
        Some(projects) => {
            let project = projects.of(id);
            (project.key, project.name, project.org)
        }
        None => (id.clone(), crate::projects::default_name(&id.key), None),
    }
}

/// What this host writes for the other hosts (spec §3.8): the rows of the
/// Sessions area at one moment. No agent's `task` is ever in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostReport {
    pub at: DateTime<Utc>,
    pub sessions: Vec<LiveSession>,
}

/// One other host's report, as this host reads it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherHost {
    pub host: String,
    pub at: DateTime<Utc>,
    /// Older than [`REPORT_STALE_MIN`]: its sessions say `Other("no report")`.
    pub stale: bool,
    pub sessions: Vec<LiveSession>,
}

/// Every other host's report, and the reports that could not be read.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherHosts {
    pub hosts: Vec<OtherHost>,
    pub warnings: Vec<String>,
}

/// This host's rows as the report other hosts read (spec §3.8). Every agent's
/// `task` is set to `None` first, so no task text can reach the file. The file
/// lives in this host's own folder, written with [`atomic::write_atomic`] and
/// [`Access::Private`].
pub fn write_report(
    history: &HistoryDir,
    sessions: &[LiveSession],
    at: DateTime<Utc>,
) -> Result<HostReport, HistoryError> {
    let mut sessions = sessions.to_vec();
    for session in &mut sessions {
        for agent in &mut session.agents {
            agent.task = None;
        }
    }
    let report = HostReport { at, sessions };
    let path = history.sessions_now_file(history.host());
    let bytes = serde_json::to_vec_pretty(&report).map_err(|source| HistoryError::Io {
        path: path.clone(),
        source: io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot encode the sessions report: {source}"),
        ),
    })?;
    let folder = history.root().join(history.host());
    atomic::ensure_private_dir(&folder).map_err(|source| HistoryError::Io {
        path: folder.clone(),
        source,
    })?;
    atomic::write_atomic(&path, &bytes, Access::Private)
        .map_err(|source| HistoryError::Io { path, source })?;
    Ok(report)
}

/// The reports of every other host folder (spec §3.8). A folder without
/// `sessions-now.json` is left out: that host has no report. A file that cannot
/// be read or parsed is a warning naming the file and the reason, and its host
/// is left out. Only an error listing the host folders themselves is an error.
///
/// A report whose `at` is more than [`REPORT_STALE_MIN`] before `now` is stale:
/// every session's status becomes `Other("no report")`. A report from the
/// future (another machine's clock is ahead) is fresh.
pub fn other_hosts(history: &HistoryDir, now: DateTime<Utc>) -> Result<OtherHosts, HistoryError> {
    let mut hosts = Vec::new();
    let mut warnings = Vec::new();
    for host in history.hosts()? {
        if host == history.host() {
            continue;
        }
        let path = history.sessions_now_file(&host);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                warnings.push(format!("{}: {source}", path.display()));
                continue;
            }
        };
        let report: HostReport = match serde_json::from_str(&text) {
            Ok(report) => report,
            Err(source) => {
                warnings.push(format!("{}: {source}", path.display()));
                continue;
            }
        };
        let stale = now.signed_duration_since(report.at) > Duration::minutes(REPORT_STALE_MIN);
        let mut sessions = report.sessions;
        if stale {
            for session in &mut sessions {
                session.status = LiveStatus::Other("no report".to_string());
            }
        }
        hosts.push(OtherHost {
            host,
            at: report.at,
            stale,
            sessions,
        });
    }
    hosts.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(OtherHosts { hosts, warnings })
}

/// The model with the most output tokens; on a tie, the first in name order.
fn most_output(out_by_model: &BTreeMap<String, u64>) -> Option<String> {
    let mut best: Option<(&String, u64)> = None;
    for (model, out) in out_by_model {
        if best.is_none_or(|(_, best_out)| *out > best_out) {
            best = Some((model, *out));
        }
    }
    best.map(|(model, _)| model.clone())
}

/// One token record for the live view: a model's or an agent's sum dated at its
/// last answer. Only the fields the valuer reads are filled.
fn token_record(
    session: &str,
    cwd: &str,
    project: &ProjectKey,
    model: &str,
    at: DateTime<Utc>,
    tokens: Tokens,
) -> TokenRecord {
    TokenRecord {
        t: at,
        session: session.to_string(),
        cwd: cwd.to_string(),
        project: project.clone(),
        model: model.to_string(),
        input: tokens.input,
        out: tokens.out,
        cw: tokens.cw,
        cr: tokens.cr,
        answers: 0,
        agent_answers: 0,
    }
}

/// The `SessionStatus` of a session file as the page shows it.
fn live_status(status: &SessionStatus) -> LiveStatus {
    match status {
        SessionStatus::Busy => LiveStatus::Busy,
        SessionStatus::Idle => LiveStatus::Idle,
        SessionStatus::Waiting => LiveStatus::Waiting,
        SessionStatus::Other(text) => LiveStatus::Other(text.clone()),
    }
}

/// Milliseconds since the epoch as a time; `None` when it is not a valid time.
fn time_ms(ms: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms).single()
}

/// The first two characters of a session id, the part a label adds to a
/// project's name.
fn short_id(session: &str) -> String {
    session.chars().take(2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::api::LimitKind;
    use crate::history::{AccountRef, Event, EventKind, WindowRecord};
    use crate::money::{Fx, ModelPrice, Prices};
    use crate::store::{Account, OwnRate};
    use crate::transcripts::SessionSeen;

    const SESSION: &str = "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34";
    const SESSION_11: &str = "11a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b";
    const AGENT: &str = "a8751b20660a1de6b";
    const ATLAS_CWD: &str = "/home/sam/projects/atlas";
    const AC_ACC: &str = "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04";
    const AC_ORG: &str = "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20";

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("a test time")
    }

    fn project_key() -> ProjectKey {
        ProjectKey::Repo("github.com/acme/atlas".to_string())
    }

    fn ac() -> AccountRef {
        AccountRef {
            acc: AC_ACC.to_string(),
            org: AC_ORG.to_string(),
        }
    }

    fn account(price: Option<f64>) -> Account {
        serde_json::from_value(json!({
            "id": "0b4f6a2c8d1e4f7a9c3b5d7e9f1a2b4c",
            "accountUuid": AC_ACC,
            "organizationUuid": AC_ORG,
            "label": "AC",
            "abbr": "AC",
            "color": "#3b82f6",
            "email": "sam@example.com",
            "orgName": "Acme",
            "plan": "Team 5x",
            "price": price,
            "priceCurrency": "EUR",
            "oauthAccount": {
                "accountUuid": AC_ACC,
                "organizationUuid": AC_ORG,
                "emailAddress": "sam@example.com"
            },
            "addedAt": "2026-09-01T00:00:00Z",
            "loginState": "ok",
            "autoBurn": true
        }))
        .expect("a test account")
    }

    fn settings_eur() -> Settings {
        Settings {
            currency: Some("EUR".to_string()),
            usd_rate: Some(OwnRate {
                currency: "EUR".to_string(),
                value: 1.0,
            }),
            ..Settings::default()
        }
    }

    fn settings_chf() -> Settings {
        Settings {
            currency: Some("CHF".to_string()),
            ..Settings::default()
        }
    }

    fn prices() -> Prices {
        let mut prices = Prices::default();
        for model in ["claude-opus-5-5", "claude-fable-5-2"] {
            prices.overrides.insert(
                format!("anthropic/{model}"),
                ModelPrice {
                    input: 4.0,
                    output: 20.0,
                    cache_write: 5.0,
                    cache_read: 0.2,
                    tiered: false,
                },
            );
        }
        prices
    }

    fn token(session: &str, at: DateTime<Utc>, model: &str, input: u64, out: u64) -> TokenRecord {
        TokenRecord {
            t: at,
            session: session.to_string(),
            cwd: ATLAS_CWD.to_string(),
            project: project_key(),
            model: model.to_string(),
            input,
            out,
            cw: 0,
            cr: 0,
            answers: 1,
            agent_answers: 0,
        }
    }

    fn session_file() -> SessionFile {
        SessionFile {
            pid: 41822,
            session_id: SESSION.to_string(),
            cwd: ATLAS_CWD.to_string(),
            started_at: 1794612960232,
            proc_start: "19664719".to_string(),
            version: Some("2.1.322".to_string()),
            kind: Some("interactive".to_string()),
            entrypoint: Some("cli".to_string()),
            name: Some("atlas-a6".to_string()),
            messaging_socket_path: Some("/run/user/1000/cc-socks/41822.sock".to_string()),
            status: SessionStatus::Busy,
            status_updated_at: 1794681295320,
        }
    }

    fn seen(model: &str, out: u64) -> SessionSeen {
        let tokens = Tokens {
            input: 2_000_000,
            out,
            cw: 26_173,
            cr: 28_126,
        };
        let mut by_model = BTreeMap::new();
        by_model.insert(model.to_string(), tokens);
        SessionSeen {
            folder: "home-sam-projects-atlas".to_string(),
            cwd: ATLAS_CWD.to_string(),
            first: utc("2026-11-13T23:40:00Z"),
            last: utc("2026-11-14T18:30:00Z"),
            last_model: Some(model.to_string()),
            last_user_turn: None,
            thinking_since_user: 0,
            totals: tokens,
            by_model,
            agents: BTreeMap::new(),
            spawns: BTreeMap::new(),
            recent: Vec::new(),
            recorded: None,
        }
    }

    fn state_with(seen: SessionSeen) -> ScanState {
        let mut state = ScanState::default();
        state.sessions.insert(SESSION.to_string(), seen);
        state
    }

    fn session_record(
        session: &str,
        t: DateTime<Utc>,
        first: DateTime<Utc>,
        last: DateTime<Utc>,
        closed: bool,
        agents_finished: u32,
    ) -> SessionRecord {
        SessionRecord {
            t,
            session: session.to_string(),
            cwd: ATLAS_CWD.to_string(),
            project: project_key(),
            entrypoint: Some("cli".to_string()),
            first,
            last,
            agents_started: agents_finished,
            agents_finished,
            closed,
        }
    }

    fn projects() -> Projects {
        let hosted = Hosted {
            host: "atlas".to_string(),
            record: session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ),
        };
        Projects::resolve(&[], &[hosted], &[])
    }

    fn known_pricing<'a>(valuer: &'a Valuer, settings: &'a Settings) -> Pricing<'a> {
        Pricing::Known { valuer, settings }
    }

    fn weekly_window() -> WindowRecord {
        let end = utc("2026-11-14T00:00:00Z");
        WindowRecord {
            account: ac(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(end - Duration::days(7)),
            end,
            final_percent: 80.0,
            lost: None,
            last_sample: end,
            plan: None,
            uncertain: false,
        }
    }

    fn valuer(history: &HistoryDir, settings: &Settings, accounts: &[Account]) -> Valuer {
        let range = Range::between(
            Some(utc("2026-11-10T00:00:00Z")),
            Some(utc("2026-11-15T00:00:00Z")),
        )
        .expect("a range");
        Valuer::build(
            history,
            &range,
            &prices(),
            &Fx::default(),
            settings,
            accounts,
        )
        .expect("a valuer")
    }

    fn row(status: LiveStatus, model: Option<&str>, since: DateTime<Utc>) -> LiveSession {
        LiveSession {
            session: SESSION.to_string(),
            host: "atlas".to_string(),
            label: "atlas-7c".to_string(),
            project_id: ProjectId::of(&project_key(), "atlas"),
            project: "atlas".to_string(),
            org: Some("acme".to_string()),
            cwd: ATLAS_CWD.to_string(),
            provider: PROVIDER.to_string(),
            model: model.map(str::to_string),
            tied: model.map(thinking_tied).unwrap_or(false),
            status,
            since,
            started: since,
            agents_running: 0,
            agents_done: 0,
            agents: Vec::new(),
            tokens: Tokens::default(),
            last_hour: Tokens::default(),
            values: Values::missing(NO_TOKENS, &Settings::default()),
        }
    }

    #[test]
    fn a_running_session_becomes_a_row() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let state = state_with(seen("claude-opus-5-5", 12_400_000));
        let rows = running(
            &[session_file()],
            &state,
            &|_: &str| Vec::new(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        assert!(rows.warnings.is_empty(), "{:?}", rows.warnings);
        assert_eq!(rows.sessions.len(), 1);
        let row = &rows.sessions[0];
        assert_eq!(row.label, "atlas-a6");
        assert_eq!(row.provider, "Claude Code");
        assert!(!row.tied);
        assert_eq!(row.status, LiveStatus::Busy);
        assert_eq!(row.tokens.out, 12_400_000);
        assert_eq!(row.project, "atlas");
        assert_eq!(row.org.as_deref(), Some("acme"));
        assert_eq!(row.started, utc("2026-11-13T23:36:00.232Z"));
        assert_eq!(row.since, utc("2026-11-14T18:34:55.320Z"));
    }

    #[test]
    fn agents_are_counted_running_and_done() {
        let metas: Vec<AgentMeta> = (0..41)
            .map(|i| AgentMeta {
                session: SESSION.to_string(),
                agent: format!("a8751b20660a1de{i:02x}"),
                agent_type: Some("general-purpose".to_string()),
                description: Some("fix the failing tests".to_string()),
                finished: i >= 3,
                spawn_depth: 1,
                tool_use_id: Some(format!("toolu_{i:02}")),
                parent: None,
            })
            .collect();
        let metas_of = |_: &str| metas.clone();
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let state = state_with(seen("claude-opus-5-5", 1_000_000));
        let rows = running(
            &[session_file()],
            &state,
            &metas_of,
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        let row = &rows.sessions[0];
        assert_eq!(row.agents_running, 3);
        assert_eq!(row.agents_done, 38);
        assert_eq!(row.agents.len(), 41);
    }

    #[test]
    fn a_fable_session_is_tied() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let state = state_with(seen("claude-fable-5-2", 1_000_000));
        let rows = running(
            &[session_file()],
            &state,
            &|_: &str| Vec::new(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        assert!(rows.sessions[0].tied);
    }

    #[test]
    fn switch_kinds() {
        let now = utc("2026-11-14T19:00:00Z");
        let idle = row(
            LiveStatus::Idle,
            Some("claude-opus-5-5"),
            now - Duration::minutes(61),
        );
        assert_eq!(switch_kind(&idle, 0, now), SwitchKind::Free);
        let idle = row(
            LiveStatus::Idle,
            Some("claude-opus-5-5"),
            now - Duration::minutes(10),
        );
        assert_eq!(switch_kind(&idle, 0, now), SwitchKind::Resend);
        let busy = row(LiveStatus::Busy, Some("claude-fable-5-2"), now);
        assert_eq!(switch_kind(&busy, 3, now), SwitchKind::LosesThinking);
        assert_eq!(switch_kind(&busy, 0, now), SwitchKind::Resend);
        let closed = row(LiveStatus::Closed, Some("claude-fable-5-2"), now);
        assert_eq!(switch_kind(&closed, 3, now), SwitchKind::Free);
    }

    #[test]
    fn values_come_from_the_valuer_and_say_why_when_missing() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&Event {
                t: utc("2026-11-06T00:00:00Z"),
                kind: EventKind::Live {
                    account: Some(ac()),
                },
            })
            .expect("a live event");
        history.append(&weekly_window()).expect("a window");
        let accounts = [account(Some(140.0))];
        let opus = token(
            SESSION,
            utc("2026-11-10T10:00:00Z"),
            "claude-opus-5-5",
            1_000_000,
            0,
        );
        let unknown = token(
            SESSION,
            utc("2026-11-10T10:30:00Z"),
            "claude-x",
            1_000_000,
            0,
        );

        let no_currency_settings = Settings::default();
        let no_currency = valuer(&history, &no_currency_settings, &accounts);
        let no_currency = Values::of(&[no_currency.value("atlas", &opus)], &no_currency_settings);
        assert_eq!(no_currency.api, None);
        assert_eq!(
            no_currency.api_missing.as_deref(),
            Some("set a currency in Settings → General")
        );
        assert_eq!(no_currency.subscription, None);
        assert_eq!(
            no_currency.subscription_missing.as_deref(),
            Some("set a currency in Settings → General")
        );
        assert_eq!(no_currency.currency, None);

        let eur = settings_eur();
        let priced = valuer(&history, &eur, &accounts);
        let no_price = Values::of(&[priced.value("atlas", &unknown)], &eur);
        assert_eq!(no_price.api, None);
        assert_eq!(
            no_price.api_missing.as_deref(),
            Some("no price for claude-x")
        );
        assert_eq!(
            no_price.subscription_missing.as_deref(),
            Some("no price for claude-x")
        );

        let chf = settings_chf();
        let in_usd = valuer(&history, &chf, &accounts);
        let in_usd = Values::of(&[in_usd.value("atlas", &opus)], &chf);
        assert_eq!(in_usd.api, Some(4.0));
        assert_eq!(in_usd.api_currency.as_deref(), Some("USD"));
        assert_eq!(in_usd.api_missing, None);
        assert_eq!(in_usd.currency.as_deref(), Some("CHF"));
    }

    #[test]
    fn closed_sessions_come_from_the_history() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ))
            .expect("a session record");
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T09:05:00Z"),
                "claude-opus-5-5",
                0,
                4_000_000,
            ))
            .expect("a token record");
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T09:10:00Z"),
                "claude-opus-5-5",
                0,
                3_900_000,
            ))
            .expect("a token record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert!(rows.warnings.is_empty(), "{:?}", rows.warnings);
        assert_eq!(rows.sessions.len(), 1);
        let row = &rows.sessions[0];
        assert_eq!(row.status, LiveStatus::Closed);
        assert_eq!(row.since, utc("2026-11-14T09:12:00Z"));
        assert_eq!(row.started, utc("2026-11-14T08:00:00Z"));
        assert_eq!(row.agents_done, 17);
        assert!(row.agents.is_empty());
        assert_eq!(row.label, "atlas-11");
        assert_eq!(row.project, "atlas");
        assert_eq!(row.tokens.out, 7_900_000);
    }

    #[test]
    fn a_session_closed_before_since_is_left_out() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:30:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T08:30:00Z"),
                true,
                3,
            ))
            .expect("a session record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert!(rows.sessions.is_empty());
    }

    #[test]
    fn an_agent_without_an_answer_has_no_time() {
        let meta = AgentMeta {
            session: SESSION.to_string(),
            agent: AGENT.to_string(),
            agent_type: Some("general-purpose".to_string()),
            description: Some("fix the failing tests".to_string()),
            finished: false,
            spawn_depth: 1,
            tool_use_id: Some("toolu_01Nkn4".to_string()),
            parent: None,
        };
        let metas_of = |_: &str| vec![meta.clone()];
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let state = state_with(seen("claude-opus-5-5", 1_000_000));
        let rows = running(
            &[session_file()],
            &state,
            &metas_of,
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        assert_eq!(rows.sessions[0].agents.len(), 1);
        let agent = &rows.sessions[0].agents[0];
        assert_eq!(agent.first, None);
        assert_eq!(agent.last, None);
        assert!(agent.running);
        assert_eq!(agent.kind.as_deref(), Some("general-purpose"));
        assert_eq!(agent.task.as_deref(), Some("fix the failing tests"));
        assert_eq!(agent.model, None);
        assert_eq!(
            agent.values.api_missing.as_deref(),
            Some("the agent's model is not known yet")
        );
        assert_eq!(
            agent.values.subscription_missing.as_deref(),
            Some("the agent's model is not known yet")
        );
    }

    #[test]
    fn values_of_two_currencies_are_not_added() {
        let eur = Valued {
            api: Some(1.5),
            subscription: Some(0.5),
            account: None,
            api_currency: Some("EUR".to_string()),
            api_missing: None,
            subscription_missing: None,
        };
        let usd = Valued {
            api: Some(2.0),
            subscription: Some(0.5),
            account: None,
            api_currency: Some("USD".to_string()),
            api_missing: None,
            subscription_missing: None,
        };
        let values = Values::of(&[eur, usd], &settings_eur());

        assert_eq!(values.api, None);
        assert_eq!(values.api_currency, None);
        assert_eq!(values.api_missing.as_deref(), Some(TWO_CURRENCIES));
        assert_eq!(values.subscription, Some(1.0));
        assert_eq!(values.currency.as_deref(), Some("EUR"));

        let empty = Values::of(&[], &settings_eur());
        assert_eq!(empty.api, None);
        assert_eq!(empty.api_missing.as_deref(), Some("no tokens yet"));
        assert_eq!(empty.subscription, None);
        assert_eq!(empty.subscription_missing.as_deref(), Some("no tokens yet"));
    }

    #[test]
    fn a_closed_session_sums_every_token_line_of_an_hour() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                0,
            ))
            .expect("a session record");
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T09:10:00Z"),
                "claude-opus-5-5",
                0,
                1_000_000,
            ))
            .expect("a token record");
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                "claude-opus-5-5",
                0,
                2_000_000,
            ))
            .expect("a token record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        let row = &rows.sessions[0];
        assert_eq!(row.tokens.out, 3_000_000);
        assert_eq!(row.values.api, Some(60.0));
    }

    #[test]
    fn a_row_with_a_bad_start_time_is_a_warning() {
        let mut file = session_file();
        file.started_at = i64::MAX;
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = running(
            &[file],
            &ScanState::default(),
            &|_: &str| Vec::new(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        assert!(rows.sessions.is_empty());
        assert_eq!(rows.warnings.len(), 1);
        assert_eq!(
            rows.warnings[0],
            format!(
                "session {SESSION}: startedAt {} is not a valid time",
                i64::MAX
            )
        );
    }

    // ---- the edges the review looked at ----

    /// The marker that makes a folder a host folder for the history reader.
    fn host_marker(root: &std::path::Path, host: &str) {
        let dir = root.join(host);
        std::fs::create_dir_all(&dir).expect("the host folder");
        std::fs::write(dir.join("host.json"), "{\"machineId\":\"made-up\"}").expect("the marker");
    }

    #[test]
    /// Two hosts with the same session id are two rows: the key is host and session.
    fn two_hosts_with_the_same_session_id_are_two_rows() {
        let dir = tempdir().expect("tempdir");
        let atlas = HistoryDir::at(dir.path(), "atlas");
        let vega = HistoryDir::at(dir.path(), "vega");
        host_marker(dir.path(), "vega");
        atlas
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ))
            .expect("a session record");
        vega.append(&session_record(
            SESSION_11,
            utc("2026-11-14T09:20:00Z"),
            utc("2026-11-14T08:30:00Z"),
            utc("2026-11-14T09:20:00Z"),
            true,
            3,
        ))
        .expect("a session record");
        let settings = settings_eur();
        let valuer = valuer(&atlas, &settings, &[]);
        let rows = closed(
            &atlas,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert_eq!(rows.sessions.len(), 2, "{:?}", rows.sessions);
        let hosts: Vec<&str> = rows.sessions.iter().map(|row| row.host.as_str()).collect();
        assert_eq!(hosts, vec!["atlas", "vega"]);
    }

    #[test]
    /// A session that was closed and continued has a later line with `closed: false`: it is not a row.
    fn a_session_that_was_resumed_is_not_a_closed_row() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ))
            .expect("a session record");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:40:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:40:00Z"),
                false,
                17,
            ))
            .expect("a session record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert!(rows.sessions.is_empty(), "{:?}", rows.sessions);
    }

    #[test]
    /// The last `SessionRecord` of a session wins: its counts and its time.
    fn the_last_line_of_a_session_wins() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T08:30:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T08:30:00Z"),
                false,
                2,
            ))
            .expect("a session record");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ))
            .expect("a session record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert_eq!(rows.sessions.len(), 1);
        assert_eq!(rows.sessions[0].agents_done, 17);
        assert_eq!(rows.sessions[0].since, utc("2026-11-14T09:12:00Z"));
    }

    #[test]
    /// A session that has waited for the user for hours has no stored copy left.
    fn a_session_waiting_for_hours_is_free() {
        let now = utc("2026-11-14T19:00:00Z");
        let waiting = row(
            LiveStatus::Waiting,
            Some("claude-fable-5-2"),
            now - Duration::minutes(61),
        );
        assert_eq!(switch_kind(&waiting, 3, now), SwitchKind::Free);
    }

    #[test]
    /// Exactly `CACHE_LIFE_MIN` of idleness is not yet 'longer than' it.
    fn idle_exactly_sixty_minutes_is_not_free() {
        let now = utc("2026-11-14T19:00:00Z");
        let idle = row(
            LiveStatus::Idle,
            Some("claude-opus-5-5"),
            now - Duration::minutes(60),
        );
        assert_eq!(switch_kind(&idle, 0, now), SwitchKind::Resend);
    }

    #[test]
    /// A session without a model has no answers: a switch costs it nothing.
    fn a_session_without_a_model_is_free() {
        let now = utc("2026-11-14T19:00:00Z");
        let busy = row(LiveStatus::Busy, None, now);
        assert_eq!(switch_kind(&busy, 3, now), SwitchKind::Free);
    }

    #[test]
    /// An agent with answers but no meta file is listed as done.
    fn an_agent_with_answers_but_no_meta_is_listed() {
        let mut seen = seen("claude-opus-5-5", 1_000_000);
        seen.agents.insert(
            AGENT.to_string(),
            AgentSeen {
                first: utc("2026-11-14T18:00:00Z"),
                last: utc("2026-11-14T18:30:00Z"),
                model: Some("claude-opus-5-5".to_string()),
                tokens: Tokens {
                    input: 1_000,
                    out: 2_000,
                    cw: 0,
                    cr: 0,
                },
                answers: 1,
            },
        );
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = running(
            &[session_file()],
            &state_with(seen),
            &|_: &str| Vec::new(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        let row = &rows.sessions[0];
        assert_eq!(row.agents.len(), 1);
        let agent = &row.agents[0];
        assert!(!agent.running);
        assert_eq!(agent.kind, None);
        assert_eq!(agent.task, None);
        assert_eq!(agent.parent, None);
        assert_eq!(row.agents_done, 1);
        assert_eq!(row.agents_running, 0);
    }

    #[test]
    /// An agent started by an agent keeps its parent.
    fn an_agent_started_by_an_agent_keeps_its_parent() {
        let meta = AgentMeta {
            session: SESSION.to_string(),
            agent: AGENT.to_string(),
            agent_type: Some("general-purpose".to_string()),
            description: Some("fix the failing tests".to_string()),
            finished: true,
            spawn_depth: 2,
            tool_use_id: Some("toolu_01Nkn4".to_string()),
            parent: Some("a8751b20660a1de6b".to_string()),
        };
        let metas_of = |_: &str| vec![meta.clone()];
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = running(
            &[session_file()],
            &state_with(seen("claude-opus-5-5", 1_000_000)),
            &metas_of,
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        assert_eq!(
            rows.sessions[0].agents[0].parent.as_deref(),
            Some("a8751b20660a1de6b")
        );
    }

    #[test]
    /// A session the scan has not seen yet is a row with no tokens.
    fn a_session_without_a_scan_state_is_a_row_with_no_tokens() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = running(
            &[session_file()],
            &ScanState::default(),
            &|_: &str| Vec::new(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        let row = &rows.sessions[0];
        assert_eq!(row.tokens, Tokens::default());
        assert_eq!(row.values.api, None);
        assert_eq!(row.values.api_missing.as_deref(), Some("no tokens yet"));
        assert_eq!(row.model, None);
        assert!(!row.tied);
    }

    #[test]
    /// The name and the label of a joined project are the joined ones.
    fn the_label_and_project_name_after_a_join() {
        let old = ProjectId::of(&ProjectKey::Folder("atlas-old".to_string()), "atlas");
        let atlas = ProjectId::of(&project_key(), "atlas");
        let log = vec![Hosted {
            host: "atlas".to_string(),
            record: crate::projects::ProjectsLogRecord {
                t: utc("2026-11-14T12:00:00Z"),
                edit: crate::projects::ProjectEdit::Join {
                    key: old.clone(),
                    into: atlas.clone(),
                },
            },
        }];
        let sessions = vec![Hosted {
            host: "atlas".to_string(),
            record: session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ),
        }];
        let projects = Projects::resolve(&log, &sessions, &[]);
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let mut file = session_file();
        file.name = None;
        let rows = running(
            &[file],
            &state_with(seen("claude-opus-5-5", 1_000_000)),
            &|_: &str| Vec::new(),
            Some(&projects),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| ProjectKey::Folder("atlas-old".to_string()),
        );

        let row = &rows.sessions[0];
        assert_eq!(row.project, "atlas", "the name after the join");
        assert_eq!(row.label, "atlas-7c", "the label after the join");
    }

    #[test]
    /// A folder project is one project per host.
    fn a_folder_project_is_one_project_per_host() {
        let dir = tempdir().expect("tempdir");
        let atlas = HistoryDir::at(dir.path(), "atlas");
        let vega = HistoryDir::at(dir.path(), "vega");
        host_marker(dir.path(), "vega");
        let mut record = session_record(
            SESSION_11,
            utc("2026-11-14T09:12:00Z"),
            utc("2026-11-14T08:00:00Z"),
            utc("2026-11-14T09:12:00Z"),
            true,
            17,
        );
        record.project = ProjectKey::Folder("atlas".to_string());
        atlas.append(&record).expect("a session record");
        vega.append(&record).expect("a session record");
        let settings = settings_eur();
        let valuer = valuer(&atlas, &settings, &[]);
        let rows = closed(
            &atlas,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert_eq!(rows.sessions.len(), 2, "{:?}", rows.sessions);
        let labels: Vec<&str> = rows.sessions.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, vec!["atlas-11", "atlas-11"]);
    }

    #[test]
    /// A closed session without token records has no tokens and says so.
    fn a_closed_session_without_token_records() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ))
            .expect("a session record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:00:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        let row = &rows.sessions[0];
        assert_eq!(row.tokens, Tokens::default());
        assert_eq!(row.values.api, None);
        assert_eq!(row.values.api_missing.as_deref(), Some("no tokens yet"));
        assert_eq!(row.model, None);
        assert!(!row.tied);
    }

    #[test]
    /// A closed session that closed less than an hour ago shows its tokens of the last hour.
    fn a_closed_session_shows_its_tokens_of_the_last_hour() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:12:00Z"),
                utc("2026-11-14T08:00:00Z"),
                utc("2026-11-14T09:12:00Z"),
                true,
                17,
            ))
            .expect("a session record");
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T09:10:00Z"),
                "claude-opus-5-5",
                0,
                1_000_000,
            ))
            .expect("a token record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T09:20:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        let row = &rows.sessions[0];
        assert_eq!(
            row.last_hour.out, 1_000_000,
            "the row was closed ten minutes ago: its tokens are in the last hour"
        );
    }

    #[test]
    /// A session with 400 agents is built in well under a second.
    fn the_cost_of_a_row_with_four_hundred_agents() {
        let metas: Vec<AgentMeta> = (0..400)
            .map(|i| AgentMeta {
                session: SESSION.to_string(),
                agent: format!("a8751b20660a1de{i:04x}"),
                agent_type: Some("general-purpose".to_string()),
                description: Some("fix the failing tests".to_string()),
                finished: i >= 3,
                spawn_depth: 1,
                tool_use_id: Some(format!("toolu_{i:04}")),
                parent: None,
            })
            .collect();
        let metas_of = |_: &str| metas.clone();
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let state = state_with(seen("claude-opus-5-5", 1_000_000));
        let start = std::time::Instant::now();
        let rows = running(
            &[session_file()],
            &state,
            &metas_of,
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );
        let elapsed = start.elapsed();
        let text = serde_json::to_string(&rows.sessions[0]).expect("the row as json");
        println!(
            "400 agents: {:?} for the row, {} bytes of json",
            elapsed,
            text.len()
        );
        assert_eq!(rows.sessions[0].agents.len(), 400);
    }

    #[test]
    /// One part without a price makes the whole sum missing, with that part's reason.
    fn values_of_a_list_with_a_missing_part() {
        let missing = Valued {
            api: None,
            subscription: None,
            account: None,
            api_currency: None,
            api_missing: Some(crate::money::Missing::NoPrice {
                model: "claude-x".to_string(),
            }),
            subscription_missing: Some(crate::money::Missing::NoPrice {
                model: "claude-x".to_string(),
            }),
        };
        let priced = Valued {
            api: Some(2.0),
            subscription: Some(0.5),
            account: None,
            api_currency: Some("EUR".to_string()),
            api_missing: None,
            subscription_missing: None,
        };
        let values = Values::of(&[missing, priced], &settings_eur());

        assert_eq!(values.api, None);
        assert_eq!(values.api_missing.as_deref(), Some("no price for claude-x"));
        assert_eq!(values.subscription, None);
        assert_eq!(
            values.subscription_missing.as_deref(),
            Some("no price for claude-x")
        );
    }

    #[test]
    /// A sum is all or nothing per half, never adds two API currencies, and
    /// keeps the first missing part's reason. An empty sum says "no tokens yet".
    fn a_sum_of_values_is_all_or_nothing() {
        let eur = Values {
            api: Some(1.0),
            api_currency: Some("EUR".to_string()),
            api_missing: None,
            subscription: Some(0.5),
            currency: Some("EUR".to_string()),
            subscription_missing: None,
        };
        let usd = Values {
            api: Some(2.0),
            api_currency: Some("USD".to_string()),
            api_missing: None,
            subscription: Some(0.75),
            currency: Some("EUR".to_string()),
            subscription_missing: None,
        };
        let sum = Values::sum(&[&eur, &usd]);
        assert_eq!(sum.api, None);
        assert_eq!(sum.api_currency, None);
        assert_eq!(sum.api_missing.as_deref(), Some(TWO_CURRENCIES));
        assert_eq!(sum.subscription, Some(1.25));
        assert_eq!(sum.subscription_missing, None);
        assert_eq!(sum.currency.as_deref(), Some("EUR"));

        let unpriced = Values {
            api: None,
            api_currency: None,
            api_missing: Some("no price for claude-x".to_string()),
            subscription: None,
            currency: Some("EUR".to_string()),
            subscription_missing: Some("no price for claude-x".to_string()),
        };
        let sum = Values::sum(&[&eur, &unpriced]);
        assert_eq!(sum.api, None);
        assert_eq!(sum.api_missing.as_deref(), Some("no price for claude-x"));
        assert_eq!(sum.subscription, None);
        assert_eq!(
            sum.subscription_missing.as_deref(),
            Some("no price for claude-x")
        );
        assert_eq!(sum.currency.as_deref(), Some("EUR"));

        let empty = Values::sum(&[]);
        assert_eq!(empty.api, None);
        assert_eq!(empty.subscription, None);
        assert_eq!(empty.api_missing.as_deref(), Some(NO_TOKENS));
        assert_eq!(empty.subscription_missing.as_deref(), Some(NO_TOKENS));
        assert_eq!(empty.currency, None);
    }

    #[test]
    /// Two hosts can set different currencies; their subscriptions are then not
    /// added, and neither is their API value.
    fn a_sum_of_two_hosts_in_two_currencies_is_not_added() {
        let atlas = Values {
            api: Some(1.0),
            api_currency: Some("EUR".to_string()),
            api_missing: None,
            subscription: Some(2.0),
            currency: Some("EUR".to_string()),
            subscription_missing: None,
        };
        let vega = Values {
            api: Some(3.0),
            api_currency: Some("USD".to_string()),
            api_missing: None,
            subscription: Some(4.0),
            currency: Some("USD".to_string()),
            subscription_missing: None,
        };
        let sum = Values::sum(&[&atlas, &vega]);
        assert_eq!(sum.subscription, None);
        assert_eq!(sum.currency, None);
        assert_eq!(
            sum.subscription_missing.as_deref(),
            Some(TWO_SUBSCRIPTION_CURRENCIES)
        );
        assert_eq!(sum.api, None);
        assert_eq!(sum.api_currency, None);
        assert_eq!(sum.api_missing.as_deref(), Some(TWO_CURRENCIES));
    }

    #[test]
    /// `Values::of` is exactly the sum of the one-record values it builds.
    fn values_of_is_the_sum_of_its_records() {
        let records = [
            Valued {
                api: Some(2.0),
                subscription: Some(0.5),
                account: None,
                api_currency: Some("EUR".to_string()),
                api_missing: None,
                subscription_missing: None,
            },
            Valued {
                api: Some(3.0),
                subscription: Some(0.25),
                account: None,
                api_currency: Some("EUR".to_string()),
                api_missing: None,
                subscription_missing: None,
            },
            Valued {
                api: None,
                subscription: None,
                account: None,
                api_currency: None,
                api_missing: Some(crate::money::Missing::NoPrice {
                    model: "claude-x".to_string(),
                }),
                subscription_missing: Some(crate::money::Missing::NoPrice {
                    model: "claude-x".to_string(),
                }),
            },
        ];
        let settings = settings_eur();
        let of = Values::of(&records, &settings);
        let parts: Vec<Values> = records
            .iter()
            .map(|record| Values::of(std::slice::from_ref(record), &settings))
            .collect();
        let parts: Vec<&Values> = parts.iter().collect();
        assert_eq!(of, Values::sum(&parts));
        assert_eq!(of.api, None);
        assert_eq!(of.api_missing.as_deref(), Some("no price for claude-x"));
        assert_eq!(of.subscription, None);
    }

    #[test]
    /// The agent metas of a session are read once per session file.
    fn metas_is_called_once_per_session() {
        let calls = std::cell::RefCell::new(0);
        let metas_of = |_: &str| {
            *calls.borrow_mut() += 1;
            Vec::new()
        };
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let mut second = session_file();
        second.session_id = SESSION_11.to_string();
        second.pid = 41823;
        let rows = running(
            &[session_file(), second],
            &state_with(seen("claude-opus-5-5", 1_000_000)),
            &metas_of,
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        assert_eq!(rows.sessions.len(), 2);
        assert_eq!(*calls.borrow(), 2, "one call per session file");
    }

    // ---- the report other hosts read (spec §3.8) ----

    fn report_session(host: &str, status: LiveStatus, at: DateTime<Utc>) -> LiveSession {
        let mut session = row(status, Some("claude-opus-5-5"), at);
        session.host = host.to_string();
        session
    }

    #[test]
    fn the_report_has_no_task_text() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let at = utc("2026-11-14T19:00:00Z");
        let mut session = report_session("atlas", LiveStatus::Busy, at);
        session.agents.push(LiveAgent {
            agent: "a8751b20660a1de6b".to_string(),
            parent: None,
            kind: Some("general-purpose".to_string()),
            task: Some("fix the failing tests".to_string()),
            model: Some("claude-opus-5-5".to_string()),
            running: false,
            first: Some(at),
            last: Some(at),
            tokens: Tokens::default(),
            values: Values::missing(NO_TOKENS, &Settings::default()),
        });

        let report = write_report(&history, &[session], at).expect("the report");
        assert!(
            report.sessions[0].agents[0].task.is_none(),
            "the returned report has no task either"
        );
        let text =
            std::fs::read_to_string(history.sessions_now_file("atlas")).expect("the report file");
        assert!(
            !text.contains("fix the failing tests"),
            "an agent's task is never written: {text}"
        );
        assert!(text.contains("general-purpose"));
        assert!(text.contains("claude-opus-5-5"));
    }

    #[test]
    fn other_hosts_reads_every_other_report() {
        let dir = tempdir().expect("tempdir");
        let now = utc("2026-11-14T19:00:00Z");
        let atlas = HistoryDir::at(dir.path(), "atlas");
        let vega = HistoryDir::at(dir.path(), "vega");
        host_marker(dir.path(), "atlas");
        host_marker(dir.path(), "vega");
        let at = now - Duration::minutes(2);
        write_report(&atlas, &[report_session("atlas", LiveStatus::Busy, at)], at)
            .expect("this host's report");
        write_report(
            &vega,
            &[report_session("vega", LiveStatus::Waiting, at)],
            at,
        )
        .expect("vega's report");

        let other = other_hosts(&atlas, now).expect("the other hosts");
        assert!(other.warnings.is_empty(), "{:?}", other.warnings);
        assert_eq!(other.hosts.len(), 1);
        assert_eq!(other.hosts[0].host, "vega");
        assert_eq!(other.hosts[0].at, at);
        assert!(!other.hosts[0].stale, "two minutes old is fresh");
        assert_eq!(other.hosts[0].sessions[0].status, LiveStatus::Waiting);
    }

    #[test]
    fn an_old_report_is_stale() {
        let dir = tempdir().expect("tempdir");
        let now = utc("2026-11-14T19:00:00Z");
        let atlas = HistoryDir::at(dir.path(), "atlas");
        let vega = HistoryDir::at(dir.path(), "vega");
        host_marker(dir.path(), "vega");
        let at = now - Duration::minutes(REPORT_STALE_MIN + 1);
        write_report(
            &vega,
            &[report_session("vega", LiveStatus::Waiting, at)],
            at,
        )
        .expect("vega's report");

        let other = other_hosts(&atlas, now).expect("the other hosts");
        assert_eq!(other.hosts.len(), 1);
        assert!(other.hosts[0].stale);
        assert_eq!(
            other.hosts[0].sessions[0].status,
            LiveStatus::Other("no report".to_string())
        );
    }

    #[test]
    fn a_broken_report_is_a_warning_and_hides_nothing_else() {
        let dir = tempdir().expect("tempdir");
        let now = utc("2026-11-14T19:00:00Z");
        let atlas = HistoryDir::at(dir.path(), "atlas");
        let vega = HistoryDir::at(dir.path(), "vega");
        let ledger = HistoryDir::at(dir.path(), "ledger");
        host_marker(dir.path(), "vega");
        host_marker(dir.path(), "ledger");
        host_marker(dir.path(), "pr");
        write_report(&vega, &[report_session("vega", LiveStatus::Busy, now)], now)
            .expect("vega's report");
        std::fs::write(ledger.sessions_now_file("ledger"), "{not json").expect("a broken report");

        let other = other_hosts(&atlas, now).expect("the folder can be listed");
        assert_eq!(other.hosts.len(), 1, "vega is still read");
        assert_eq!(other.hosts[0].host, "vega");
        assert_eq!(other.warnings.len(), 1, "{:?}", other.warnings);
        assert!(
            other.warnings[0].contains("ledger/sessions-now.json"),
            "the warning names the file: {}",
            other.warnings[0]
        );
        assert!(
            other.warnings[0].contains("key must be a string"),
            "the warning names the reason: {}",
            other.warnings[0]
        );
    }
}
