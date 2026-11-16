//! The rows of the Sessions area (spec §3.8): the running sessions of this host
//! and the closed ones from the history, with their agents, tokens and the two
//! values of those tokens.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::atomic::{self, Access};
use crate::burn::tied::thinking_tied;
use crate::codex_sessions::CodexSession;
use crate::harness::{self, HarnessSession};
use crate::history::{HistoryDir, HistoryError, Hosted, Hosts, Range, SessionRecord, TokenRecord};
use crate::money::{Valued, Valuer};
use crate::project::ProjectKey;
use crate::projects::{ProjectId, Projects};
use crate::sessions::{SessionFile, SessionStatus};
use crate::store::Settings;
use crate::transcripts::{self, AgentMeta, AgentSeen, HarnessRecorded, ScanState, Tokens};

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
    /// The last hour's tokens valued per model, like `values` (spec §3.8).
    /// Closed rows value the hourly records that overlap the last hour.
    #[serde(default = "no_values")]
    pub last_hour_values: Values,
    /// [`Values::sum`] of the values of the agents counted done; the sum of an
    /// empty list when no agent is done (a closed row lists no agents).
    #[serde(default = "no_values")]
    pub agents_done_values: Values,
    pub values: Values,
}

/// The values of a row whose report does not carry them (another host still on
/// an older version): named as missing, never a made-up number.
fn no_values() -> Values {
    Values::unknown("the report has no such value")
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
/// meta says `finished == false` and does not say its transcript ended with the
/// agent's final `end_turn` answer (`AgentMeta::transcript_ended`). An agent
/// seen only in the transcripts has no meta and keeps today's rule: done.
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
/// of an empty list. The last hour is valued per model: each model's tokens of
/// the 60 minutes are one record dated at the newest minute of that model in
/// the hour, through the same [`Valuer::value`] as `values`. `agents_done_values`
/// is [`Values::sum`] over the values of the agents counted done.
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
        let mut done_values: Vec<Values> = Vec::new();
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
            let is_running = meta.as_ref().map(|meta| !meta.is_done()).unwrap_or(false);
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
            if !is_running {
                done_values.push(values.clone());
            }
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
        let done_values: Vec<&Values> = done_values.iter().collect();
        let agents_done_values = Values::sum(&done_values);

        let model = seen.and_then(|seen| seen.last_model.clone());
        let tokens = seen.map(|seen| seen.totals).unwrap_or_default();
        let last_hour = seen
            .map(|seen| transcripts::last_hour(seen, now))
            .unwrap_or_default();
        // Each model's tokens of the last 60 minutes, dated at the newest minute
        // of that model in the hour, so the hour is valued per model; the one
        // window rule is `transcripts::last_hour_by_model`.
        let last_hour_by_model = seen
            .map(|seen| transcripts::last_hour_by_model(seen, now))
            .unwrap_or_default();
        let last_hour_values = match pricing {
            Pricing::Known { valuer, .. } => {
                let valued: Vec<Valued> = last_hour_by_model
                    .iter()
                    .map(|(model, (at, tokens))| {
                        let record =
                            token_record(&file.session_id, &file.cwd, &key, model, *at, *tokens);
                        valuer.value(host, &record)
                    })
                    .collect();
                pricing.of(&valued)
            }
            Pricing::Unknown { reason } => Values::unknown(reason),
        };
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
            .unwrap_or_else(|| session_label(&project_name, &file.session_id));
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
            last_hour_values,
            agents_done_values,
            values,
        });
    }
    Rows { sessions, warnings }
}

/// The rows and agents one Harness read adds, and the token lines its tokens
/// produce.
#[derive(Debug, Clone)]
pub struct HarnessRows {
    /// The Harness sessions that are their own row: no linked top-level job.
    pub rows: Rows,
    /// The flash agents of each Claude Code session, keyed by that session's id.
    pub agents: BTreeMap<String, Vec<LiveAgent>>,
    /// The token lines this pass must write, and the totals after writing them.
    pub tokens: Vec<TokenRecord>,
    /// `ScanState::harness_recorded` after this pass.
    pub recorded: BTreeMap<String, HarnessRecorded>,
}

/// The provider of a Harness session that is its own row (spec §3.8).
const HARNESS_PROVIDER: &str = "DeepSeek Harness";

/// Put the Harness sessions into the Sessions area: a job named in a Claude
/// Code transcript becomes a `"flash"` agent of that session, with its workers
/// below it; every other top-level session is its own row. The tokens of every
/// session become token lines for what grew since `recorded`; the returned
/// `recorded` is the total after this pass, and a line is written only when a
/// total grew (a total that went down is a warning, never a negative line).
#[allow(clippy::too_many_arguments)]
pub fn harness_rows(
    sessions: &[HarnessSession],
    owners: &BTreeMap<String, String>,
    claude_cwd: &BTreeMap<String, String>,
    recorded: &BTreeMap<String, HarnessRecorded>,
    locks: &BTreeSet<String>,
    projects: Option<&Projects>,
    pricing: &Pricing,
    host: &str,
    now: DateTime<Utc>,
    project_of: &dyn Fn(&str) -> ProjectKey,
) -> HarnessRows {
    let by_id: BTreeMap<&str, &HarnessSession> = sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect();
    let mut components: BTreeMap<&str, Vec<&HarnessSession>> = BTreeMap::new();
    for session in sessions {
        components
            .entry(top_known_ancestor(session, &by_id))
            .or_default()
            .push(session);
    }

    let mut out = HarnessRows {
        rows: Rows {
            sessions: Vec::new(),
            warnings: Vec::new(),
        },
        agents: BTreeMap::new(),
        tokens: Vec::new(),
        recorded: recorded.clone(),
    };
    for (root_id, mut component) in components {
        component.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
        let root = by_id[root_id];
        // The owner of the tree: its own id first, then the ids of its sessions
        // and their parent chains as far as they are known.
        let owner = owner_of(root, &component, &by_id, owners);
        match owner {
            Some(owner) => {
                let claude_cwd = claude_cwd.get(&owner).cloned();
                if claude_cwd.is_none() {
                    out.rows.warnings.push(format!(
                        "the Claude Code session {owner} of Harness job {root_id} is not known yet; its folder is not known"
                    ));
                }
                for session in component {
                    let Some((cwd, project)) = harness_location(
                        session,
                        claude_cwd.as_deref(),
                        project_of,
                        &mut out.rows.warnings,
                    ) else {
                        continue;
                    };
                    let is_root = session.id.as_str() == root_id;
                    // A worker whose job is out of the slice still hangs under
                    // the job's owner; only a depth-0 job is the flash agent.
                    let (parent, kind) = if is_root && session.depth == 0 {
                        (None, Some("flash"))
                    } else {
                        (session.parent.clone(), None)
                    };
                    let running = harness::is_running(session, locks.contains(&session.id), now);
                    let agent = harness_agent(
                        session, parent, kind, running, &cwd, &project, pricing, host,
                    );
                    out.agents.entry(owner.clone()).or_default().push(agent);
                    harness_token_line(
                        session,
                        &owner,
                        &cwd,
                        &project,
                        &mut out.recorded,
                        &mut out.tokens,
                        &mut out.rows.warnings,
                    );
                }
            }
            None => {
                let cwd = match root.cwd.to_str() {
                    Some(cwd) => cwd.to_string(),
                    None => {
                        out.rows.warnings.push(format!(
                            "Harness session {} has a folder that is not text; it is not shown",
                            root.id
                        ));
                        continue;
                    }
                };
                let project = project_of(&cwd);
                let id = ProjectId::of(&project, host);
                let (project_id, project_name, org) = named_project(projects, &id);
                let mut agents = Vec::new();
                let mut running = 0u32;
                let mut done = 0u32;
                for session in component.iter().filter(|session| session.id != root_id) {
                    let is_running = harness::is_running(session, locks.contains(&session.id), now);
                    let agent = harness_agent(
                        session,
                        session.parent.clone(),
                        None,
                        is_running,
                        &cwd,
                        &project,
                        pricing,
                        host,
                    );
                    if agent.running {
                        running += 1;
                    } else {
                        done += 1;
                    }
                    agents.push(agent);
                }
                let agents_done_values = {
                    let done_values: Vec<&Values> = agents
                        .iter()
                        .filter(|agent| !agent.running)
                        .map(|agent| &agent.values)
                        .collect();
                    Values::sum(&done_values)
                };
                let values = harness_values(root, &cwd, &project, pricing, host);
                let root_running = harness::is_running(root, locks.contains(&root.id), now);
                out.rows.sessions.push(LiveSession {
                    session: root.id.clone(),
                    host: host.to_string(),
                    label: session_label(&project_name, &root.id),
                    project_id,
                    project: project_name,
                    org,
                    cwd: cwd.clone(),
                    provider: HARNESS_PROVIDER.to_string(),
                    model: root.model.clone(),
                    tied: root.model.as_deref().map(thinking_tied).unwrap_or(false),
                    status: if root_running {
                        LiveStatus::Busy
                    } else {
                        LiveStatus::Idle
                    },
                    since: root.last,
                    started: root.created,
                    agents_running: running,
                    agents_done: done,
                    agents,
                    tokens: root.tokens,
                    last_hour: Tokens::default(),
                    last_hour_values: pricing.of(&[]),
                    agents_done_values,
                    values,
                });
                for session in component {
                    harness_token_line(
                        session,
                        &session.id,
                        &cwd,
                        &project,
                        &mut out.recorded,
                        &mut out.tokens,
                        &mut out.rows.warnings,
                    );
                }
            }
        }
    }
    out.tokens
        .sort_by(|a, b| a.session.cmp(&b.session).then_with(|| a.t.cmp(&b.t)));
    out
}

/// The top known ancestor of a session: a parent that is not in the slice stops
/// the walk but its id is still looked at for an owner.
fn top_known_ancestor<'a>(
    session: &'a HarnessSession,
    by_id: &BTreeMap<&'a str, &'a HarnessSession>,
) -> &'a str {
    let mut root = session.id.as_str();
    let mut seen = BTreeSet::new();
    let mut parent = session.parent.as_deref();
    while let Some(id) = parent {
        if !seen.insert(id) {
            break;
        }
        let Some(next) = by_id.get(id) else {
            break;
        };
        root = next.id.as_str();
        parent = next.parent.as_deref();
    }
    root
}

/// The owner of a tree: the root's own id, then every session's own id and
/// parent chain as far as it is known. A parent outside the slice is still
/// looked at for an owner.
fn owner_of(
    root: &HarnessSession,
    component: &[&HarnessSession],
    by_id: &BTreeMap<&str, &HarnessSession>,
    owners: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(owner) = owners.get(&root.id) {
        return Some(owner.clone());
    }
    for session in component {
        let mut seen = BTreeSet::new();
        let mut current = Some(session.id.as_str());
        while let Some(id) = current {
            if !seen.insert(id) {
                break;
            }
            if let Some(owner) = owners.get(id) {
                return Some(owner.clone());
            }
            current = match by_id.get(id) {
                Some(session) => session.parent.as_deref(),
                None => break,
            };
        }
    }
    None
}

/// Add agents to a running row and recount its done totals and their values.
pub fn add_agents(session: &mut LiveSession, agents: Vec<LiveAgent>) {
    for agent in agents {
        if agent.running {
            session.agents_running += 1;
        } else {
            session.agents_done += 1;
        }
        session.agents.push(agent);
    }
    let done: Vec<&Values> = session
        .agents
        .iter()
        .filter(|agent| !agent.running)
        .map(|agent| &agent.values)
        .collect();
    session.agents_done_values = Values::sum(&done);
}

/// The folder and project of a Harness session: the Claude Code session's for a
/// linked job, else its own. A folder that is not text leaves the session out
/// with a warning.
fn harness_location(
    session: &HarnessSession,
    claude_cwd: Option<&str>,
    project_of: &dyn Fn(&str) -> ProjectKey,
    warnings: &mut Vec<String>,
) -> Option<(String, ProjectKey)> {
    let cwd = match claude_cwd {
        Some(cwd) => cwd,
        None => match session.cwd.to_str() {
            Some(cwd) => cwd,
            None => {
                warnings.push(format!(
                    "Harness session {} has a folder that is not text; it is not shown",
                    session.id
                ));
                return None;
            }
        },
    };
    Some((cwd.to_string(), project_of(cwd)))
}

/// One Harness session as a live agent; `kind` is `"flash"` for a linked job.
#[allow(clippy::too_many_arguments)]
fn harness_agent(
    session: &HarnessSession,
    parent: Option<String>,
    kind: Option<&str>,
    running: bool,
    cwd: &str,
    project: &ProjectKey,
    pricing: &Pricing,
    host: &str,
) -> LiveAgent {
    LiveAgent {
        agent: session.id.clone(),
        parent,
        kind: kind.map(str::to_string),
        task: session.title.clone(),
        model: session.model.clone(),
        running,
        first: Some(session.created),
        last: Some(session.last),
        tokens: session.tokens,
        values: harness_values(session, cwd, project, pricing, host),
    }
}

/// The values of a Harness session's tokens at its reported model.
fn harness_values(
    session: &HarnessSession,
    cwd: &str,
    project: &ProjectKey,
    pricing: &Pricing,
    host: &str,
) -> Values {
    let Some(model) = session.model.as_deref() else {
        return pricing.missing(NO_AGENT_MODEL);
    };
    let record = token_record(
        &session.id,
        cwd,
        project,
        model,
        session.last,
        session.tokens,
    );
    match pricing {
        Pricing::Known { valuer, .. } => pricing.of(&[valuer.value(host, &record)]),
        Pricing::Unknown { .. } => pricing.of(&[]),
    }
}

/// One token line for what a Harness session gained since `recorded`, or a
/// warning when its totals went down or its model or folder is not known. The
/// growth is split by the hour and model of each new assistant message, exactly
/// as S-1's `token_records` groups its answers.
#[allow(clippy::too_many_arguments)]
fn harness_token_line(
    session: &HarnessSession,
    history_session: &str,
    cwd: &str,
    project: &ProjectKey,
    recorded: &mut BTreeMap<String, HarnessRecorded>,
    lines: &mut Vec<TokenRecord>,
    warnings: &mut Vec<String>,
) {
    let previous = recorded.get(&session.id).cloned().unwrap_or_default();
    let current = HarnessRecorded {
        tokens: session.tokens,
        messages: session.messages,
        file: Some(session.file.clone()),
    };
    if current == previous {
        return;
    }
    let grew = current.tokens.input >= previous.tokens.input
        && current.tokens.out >= previous.tokens.out
        && current.tokens.cw >= previous.tokens.cw
        && current.tokens.cr >= previous.tokens.cr
        && current.messages >= previous.messages;
    if !grew {
        warnings.push(format!(
            "Harness session {} has fewer tokens or messages than already recorded; the old totals are kept",
            session.id
        ));
        return;
    }
    // The answers since the recorded message count: each has its own time,
    // model and usage. A message without usage adds an answer with no tokens.
    let new_count = (current.messages - previous.messages) as usize;
    let Some(messages) = session
        .messages_detail
        .get(previous.messages as usize..)
        .map(|messages| &messages[..messages.len().min(new_count)])
    else {
        warnings.push(format!(
            "Harness session {} counts {} answers but its file holds fewer; the old totals are kept",
            session.id, current.messages
        ));
        return;
    };
    let mut groups: BTreeMap<(DateTime<Utc>, String), (Tokens, u32)> = BTreeMap::new();
    for message in messages {
        let Some(model) = message.model.clone().or_else(|| session.model.clone()) else {
            warnings.push(format!(
                "Harness session {} has an answer without a model; its tokens are not recorded yet",
                session.id
            ));
            return;
        };
        let group = groups
            .entry((transcripts::hour_start(message.at), model))
            .or_default();
        if let Some(usage) = message.usage {
            group.0 += usage;
        }
        group.1 = group.1.saturating_add(1);
    }
    if groups.is_empty() && current.tokens != previous.tokens {
        warnings.push(format!(
            "Harness session {} has more tokens than recorded but no new answer; the old totals are kept",
            session.id
        ));
        return;
    }
    for ((hour, model), (tokens, answers)) in groups {
        let mut record = token_record(history_session, cwd, project, &model, hour, tokens);
        record.agent_answers = answers;
        lines.push(record);
    }
    recorded.insert(session.id.clone(), current);
}

/// The provider of a Codex row (spec §3.8).
const CODEX_PROVIDER: &str = "Codex";

/// Why a Codex row's last hour has no value: its newest token total is below the
/// total the hour is measured from, which the read warns about and never turns
/// into a number. The row carries that reason in `last_hour_values`; showing it
/// in the row and in the sums is task S-3's.
const CODEX_LAST_HOUR_UNKNOWN: &str =
    "the Codex session's token total went down, so its last hour is not known";

/// Put the running Codex sessions into the Sessions area. A session is running
/// when a process holds its rollout file open (`open_files`, from the one
/// process walk the Harness links use) or its last event is less than
/// [`harness::HARNESS_QUIET_MINUTES`] old; its `busy` says whether a task is
/// open. The two rules are Harness's running rule without its finished test: a
/// Codex session between two tasks is still open, only idle.
///
/// Only running sessions are rows. This task writes no Codex session or token
/// record to the history, so a closed Codex session has no closed row to read;
/// the providers' readings already count Codex use.
///
/// The values use the same [`Pricing`] and [`Valuer`] as the other rows: a
/// session's tokens at its model, dated at its last event, and its `last_hour`
/// at the same model and time with the same valuer. A session without a model
/// has both values missing with the same reason as an agent without one. A
/// last hour that is not known has both values missing with
/// [`CODEX_LAST_HOUR_UNKNOWN`] rather than a value of 0; the row carries that
/// reason in `last_hour_values`, and showing it in the row and in the sums is
/// task S-3's.
pub fn codex_rows(
    sessions: &[CodexSession],
    open_files: &BTreeSet<PathBuf>,
    projects: Option<&Projects>,
    pricing: &Pricing,
    host: &str,
    now: DateTime<Utc>,
    project_of: &dyn Fn(&str) -> ProjectKey,
) -> Rows {
    let mut rows = Rows {
        sessions: Vec::new(),
        warnings: Vec::new(),
    };
    for session in sessions {
        if !codex_running(session, open_files, now) {
            continue;
        }
        let Some(cwd) = session.cwd.to_str() else {
            rows.warnings.push(format!(
                "Codex session {} has a folder that is not text; it is not shown",
                session.id
            ));
            continue;
        };
        let project = project_of(cwd);
        let id = ProjectId::of(&project, host);
        let (project_id, project_name, org) = named_project(projects, &id);
        let last_hour_values = match session.last_hour {
            Some(last_hour) => {
                codex_tokens_values(session, last_hour, cwd, &project, pricing, host)
            }
            None => pricing.missing(CODEX_LAST_HOUR_UNKNOWN),
        };
        rows.sessions.push(LiveSession {
            session: session.id.clone(),
            host: host.to_string(),
            label: session_label(&project_name, &session.id),
            project_id,
            project: project_name,
            org,
            cwd: cwd.to_string(),
            provider: CODEX_PROVIDER.to_string(),
            model: session.model.clone(),
            // Codex does not run on the Claude account, so no thinking of its is
            // tied to that account.
            tied: false,
            status: if session.busy {
                LiveStatus::Busy
            } else {
                LiveStatus::Idle
            },
            since: session.last,
            started: session.started,
            agents_running: 0,
            agents_done: 0,
            agents: Vec::new(),
            tokens: session.tokens,
            // A row's tokens cannot say "not known"; its values carry the
            // reason (`CODEX_LAST_HOUR_UNKNOWN`), which S-3 shows in the row
            // and the sums.
            last_hour: session.last_hour.unwrap_or_default(),
            last_hour_values,
            agents_done_values: Values::sum(&[]),
            values: codex_values(session, cwd, &project, pricing, host),
        });
    }
    rows
}

/// Whether a Codex session is running: a process holds its rollout file open,
/// or its last event is less than [`harness::HARNESS_QUIET_MINUTES`] old.
fn codex_running(
    session: &CodexSession,
    open_files: &BTreeSet<PathBuf>,
    now: DateTime<Utc>,
) -> bool {
    codex_recent(session, now) || holds_open(open_files, session)
}

/// Whether a Codex session's last event is inside the quiet window, so the
/// running rule holds without the process walk.
fn codex_recent(session: &CodexSession, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(session.last) < Duration::minutes(harness::HARNESS_QUIET_MINUTES)
}

/// Whether any Codex session is old enough that only an open file could still
/// make it running: the one condition under which the process walk's open files
/// are needed. The session thread asks this before walking, so a pass with only
/// recent Codex sessions does not walk at all.
pub fn codex_needs_process_walk(sessions: &[CodexSession], now: DateTime<Utc>) -> bool {
    sessions.iter().any(|session| !codex_recent(session, now))
}

/// Whether the process walk found `session`'s file open. The kernel reports an
/// open file by its resolved path, so a store reached through a symlink is
/// matched after resolving the file too.
fn holds_open(open_files: &BTreeSet<PathBuf>, session: &CodexSession) -> bool {
    if open_files.contains(&session.file) {
        return true;
    }
    match fs::canonicalize(&session.file) {
        Ok(resolved) => open_files.contains(&resolved),
        Err(_) => false,
    }
}

/// The values of a Codex session's tokens at its reported model.
fn codex_values(
    session: &CodexSession,
    cwd: &str,
    project: &ProjectKey,
    pricing: &Pricing,
    host: &str,
) -> Values {
    codex_tokens_values(session, session.tokens, cwd, project, pricing, host)
}

/// The values of one token total of a Codex session, at its model and the
/// session's last event: the one valuer both `tokens` and `last_hour` use.
fn codex_tokens_values(
    session: &CodexSession,
    tokens: Tokens,
    cwd: &str,
    project: &ProjectKey,
    pricing: &Pricing,
    host: &str,
) -> Values {
    let Some(model) = session.model.as_deref() else {
        return pricing.missing(NO_AGENT_MODEL);
    };
    let record = token_record(&session.id, cwd, project, model, session.last, tokens);
    match pricing {
        Pricing::Known { valuer, .. } => pricing.of(&[valuer.value(host, &record)]),
        Pricing::Unknown { .. } => pricing.of(&[]),
    }
}

/// The sessions of every host that closed in `[since, now)` (spec §3.8). A row
/// is a session whose last `SessionRecord` has `closed: true` and whose `last`
/// is in the range; its tokens are the sum of every `TokenRecord` of the
/// session from the hour start of the earliest `first` of the rows (a record's
/// `t` is the start of its hour), because one hour and model can have several
/// lines. Its last hour sums the records whose hour `[t, t + 1 h)` overlaps
/// `[now - 60 min, now)`: the history's records are hourly, so a record from
/// the hour before the window still counts; those overlapping records are also
/// its `last_hour_values`, each valued like `values`. Its agents list is empty,
/// so its `agents_done_values` is the sum of an empty list; the counts come from
/// the record. `label` is `<project>-<first 2 characters of
/// the session id>`.
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
    let token_range = Range::between(Some(transcripts::hour_start(earliest)), Some(now))?;
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
        let mut last_hour_valued: Vec<Valued> = Vec::new();
        for token in of_session {
            let of_record = Tokens::from(*token);
            tokens += of_record;
            // The history is hourly: a record whose hour overlaps the window
            // counts, even when its start is before the window.
            if token.t < now && transcripts::hour_start(token.t) + Duration::hours(1) > hour_ago {
                last_hour += of_record;
                if let Pricing::Known { valuer, .. } = pricing {
                    last_hour_valued.push(valuer.value(&hosted.host, token));
                }
            }
            *out_by_model.entry(token.model.clone()).or_insert(0) += token.out;
            if let Pricing::Known { valuer, .. } = pricing {
                valued.push(valuer.value(&hosted.host, token));
            }
        }
        let model = most_output(&out_by_model);
        let id = ProjectId::of(&record.project, &hosted.host);
        let (project_id, project_name, org) = named_project(projects, &id);
        let label = session_label(&project_name, &record.session);
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
            last_hour_values: pricing.of(&last_hour_valued),
            agents_done_values: Values::sum(&[]),
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

/// The key with the most output tokens; on a tie, the first in key order.
pub(crate) fn most_output<K: Ord + Clone>(by: &BTreeMap<K, u64>) -> Option<K> {
    let mut best: Option<(&K, u64)> = None;
    for (key, out) in by {
        if best.is_none_or(|(_, best_out)| *out > best_out) {
            best = Some((key, *out));
        }
    }
    best.map(|(key, _)| key.clone())
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

/// A session's label: the project's name, a dash, and the first two characters
/// of the session id.
pub(crate) fn session_label(project: &str, session: &str) -> String {
    format!("{}-{}", project, short_id(session))
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
    use crate::store::{Account, Settings};
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
        let list = ["claude-opus-5-5", "claude-fable-5-2"]
            .into_iter()
            .map(|model| {
                (
                    format!("anthropic/{model}"),
                    ModelPrice {
                        input: 4.0,
                        output: 20.0,
                        cache_write: 5.0,
                        cache_read: 0.2,
                        tiered: false,
                    },
                )
            })
            .collect();
        prices.take(list, utc("2026-01-01T00:00:00Z"));
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
            thinking: Vec::new(),
            totals: tokens,
            by_model,
            agents: BTreeMap::new(),
            spawns: BTreeMap::new(),
            recent: Vec::new(),
            recorded: None,
            flash_jobs: BTreeSet::new(),
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
            reset: false,
            from: None,
        }
    }

    fn valuer(history: &HistoryDir, settings: &Settings, accounts: &[Account]) -> Valuer {
        valuer_with(history, settings, accounts, &prices())
    }

    /// A looked-up EUR rate of 1 so the fixture's EUR values stay EUR.
    fn fx_eur() -> Fx {
        let mut fx = Fx {
            currency: "EUR".to_string(),
            ..Fx::default()
        };
        fx.take(utc("2026-01-01T00:00:00Z").date_naive(), 1.0);
        fx
    }

    fn valuer_with(
        history: &HistoryDir,
        settings: &Settings,
        accounts: &[Account],
        prices: &Prices,
    ) -> Valuer {
        let range = Range::between(
            Some(utc("2026-11-10T00:00:00Z")),
            Some(utc("2026-11-15T00:00:00Z")),
        )
        .expect("a range");
        Valuer::build(
            history,
            &range,
            prices,
            &fx_eur(),
            settings,
            accounts,
            &std::cell::RefCell::new(Vec::new()),
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
            last_hour_values: Values::missing(NO_TOKENS, &Settings::default()),
            agents_done_values: Values::sum(&[]),
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
                spawn_depth: Some(1),
                tool_use_id: Some(format!("toolu_{i:02}")),
                parent: None,
                transcript_ended: false,
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
    fn last_hour_is_valued_per_model() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        // Two prices, so a sum of both models cannot be mistaken for one model's.
        let mut prices = prices();
        prices
            .models
            .get_mut("anthropic/claude-fable-5-2")
            .and_then(|points| points.last_mut())
            .expect("the fable price")
            .price
            .output = 40.0;
        let valuer = valuer_with(&history, &settings, &[], &prices);
        let minute = utc("2026-11-14T18:30:00Z");
        let mut seen = seen("claude-opus-5-5", 1_000_000);
        let mut of_minute = BTreeMap::new();
        of_minute.insert(
            "claude-opus-5-5".to_string(),
            Tokens {
                out: 1_000_000,
                ..Tokens::default()
            },
        );
        of_minute.insert(
            "claude-fable-5-2".to_string(),
            Tokens {
                out: 500_000,
                ..Tokens::default()
            },
        );
        seen.recent = vec![
            // A minute starting exactly at `now - 60 min` is not in the hour.
            (utc("2026-11-14T18:00:00Z"), {
                let mut outside = BTreeMap::new();
                outside.insert(
                    "claude-opus-5-5".to_string(),
                    Tokens {
                        out: 9_000_000,
                        ..Tokens::default()
                    },
                );
                outside.insert(
                    "claude-fable-5-2".to_string(),
                    Tokens {
                        out: 9_000_000,
                        ..Tokens::default()
                    },
                );
                outside
            }),
            (minute, of_minute),
        ];
        let now = utc("2026-11-14T19:00:00Z");

        // Each model's own part: its tokens of the hour and its newest minute.
        let by_model = transcripts::last_hour_by_model(&seen, now);
        assert_eq!(by_model.len(), 2, "one part per model");
        assert_eq!(
            by_model["claude-opus-5-5"],
            (
                minute,
                Tokens {
                    out: 1_000_000,
                    ..Tokens::default()
                }
            ),
            "opus's own part"
        );
        assert_eq!(
            by_model["claude-fable-5-2"],
            (
                minute,
                Tokens {
                    out: 500_000,
                    ..Tokens::default()
                }
            ),
            "fable's own part"
        );

        let rows = running(
            &[session_file()],
            &state_with(seen),
            &|_: &str| Vec::new(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            now,
            &|_: &str| project_key(),
        );

        let row = &rows.sessions[0];
        assert_eq!(
            row.last_hour.out, 1_500_000,
            "both models' minutes; the boundary minute is not counted"
        );
        assert_eq!(
            row.last_hour_values.api,
            Some(40.0),
            "20 for opus and 20 for fable, in two parts"
        );
        assert_eq!(row.last_hour_values.api_currency.as_deref(), Some("EUR"));
    }

    #[test]
    fn completed_agents_have_a_summed_value() {
        let mut seen = seen("claude-opus-5-5", 1_000_000);
        let priced = AgentSeen {
            first: utc("2026-11-14T18:10:00Z"),
            last: utc("2026-11-14T18:21:00Z"),
            model: Some("claude-opus-5-5".to_string()),
            tokens: Tokens {
                out: 1_000_000,
                ..Tokens::default()
            },
            answers: 1,
        };
        let unpriced = AgentSeen {
            first: utc("2026-11-14T18:00:00Z"),
            last: utc("2026-11-14T18:20:00Z"),
            model: Some("claude-x".to_string()),
            tokens: Tokens {
                out: 1_000_000,
                ..Tokens::default()
            },
            answers: 1,
        };
        // Sorted by id, the unpriced agent's reason comes first in the sum.
        seen.agents.insert("a1111b20660a1de6".to_string(), unpriced);
        seen.agents.insert("a2221b20660a1de6".to_string(), priced);
        let metas = vec![
            AgentMeta {
                session: SESSION.to_string(),
                agent: "a1111b20660a1de6".to_string(),
                agent_type: Some("general-purpose".to_string()),
                description: Some("fix the failing tests".to_string()),
                finished: true,
                spawn_depth: Some(1),
                tool_use_id: None,
                parent: None,
                transcript_ended: true,
            },
            AgentMeta {
                session: SESSION.to_string(),
                agent: "a2221b20660a1de6".to_string(),
                agent_type: Some("general-purpose".to_string()),
                description: Some("fix the failing tests".to_string()),
                finished: true,
                spawn_depth: Some(1),
                tool_use_id: None,
                parent: None,
                transcript_ended: true,
            },
        ];
        let metas_of = |_: &str| metas.clone();
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = running(
            &[session_file()],
            &state_with(seen),
            &metas_of,
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        let row = &rows.sessions[0];
        assert_eq!(row.agents_done, 2);
        assert_eq!(row.agents_done_values.api, None);
        assert_eq!(
            row.agents_done_values.api_missing.as_deref(),
            Some("no price for claude-x"),
            "the unpriced agent's reason stays in the sum"
        );
        assert_eq!(
            row.agents_done_values.subscription_missing.as_deref(),
            Some("no price for claude-x")
        );
    }

    #[test]
    fn an_agent_without_finished_whose_last_entry_is_end_turn_is_done() {
        let meta = AgentMeta {
            session: SESSION.to_string(),
            agent: AGENT.to_string(),
            agent_type: Some("general-purpose".to_string()),
            description: Some("fix the failing tests".to_string()),
            finished: false,
            spawn_depth: Some(1),
            tool_use_id: None,
            parent: None,
            transcript_ended: true,
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

        let row = &rows.sessions[0];
        assert_eq!(row.agents_running, 0, "the transcript ended with end_turn");
        assert_eq!(row.agents_done, 1);
        assert!(!row.agents[0].running);
    }

    #[test]
    fn a_resumed_agent_runs_again() {
        let dir = tempdir().expect("tempdir");
        let claude = crate::paths::ClaudePaths::for_config_dir(dir.path(), dir.path());
        let subagents = claude
            .projects_dir()
            .join("-home-sam-projects-atlas")
            .join(SESSION)
            .join("subagents");
        std::fs::create_dir_all(&subagents).expect("the subagents folder");
        std::fs::write(
            subagents.join(format!("agent-{AGENT}.meta.json")),
            json!({
                "agentType": "general-purpose",
                "description": "fix the failing tests",
                "spawnDepth": 1,
                "toolUseId": null
            })
            .to_string(),
        )
        .expect("the meta file");
        let transcript = subagents.join(format!("agent-{AGENT}.jsonl"));
        let end_turn = json!({
            "type": "assistant",
            "timestamp": "2026-11-14T18:30:00Z",
            "cwd": ATLAS_CWD,
            "message": {
                "id": "msg_agent_a",
                "model": "claude-opus-5-5",
                "content": [],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 2,
                    "output_tokens": 166,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        })
        .to_string();
        std::fs::write(&transcript, format!("{end_turn}\n")).expect("the transcript");

        let first = crate::transcripts::agents_of(
            &claude,
            SESSION,
            &BTreeMap::new(),
            &mut crate::transcripts::AgentEnds::default(),
        );
        assert_eq!(first.metas.len(), 1);
        assert!(
            first.metas[0].transcript_ended,
            "the last entry is end_turn"
        );
        let metas = first.metas.clone();
        let settings = settings_eur();
        let history = HistoryDir::at(dir.path(), "atlas");
        let valuer = valuer(&history, &settings, &[]);
        let first_rows = running(
            &[session_file()],
            &state_with(seen("claude-opus-5-5", 1_000_000)),
            &|_: &str| metas.clone(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );
        assert_eq!(first_rows.sessions[0].agents_done, 1);
        assert_eq!(first_rows.sessions[0].agents_running, 0);

        // The agent is resumed: a new user turn is the transcript's last entry.
        let mut text = std::fs::read_to_string(&transcript).expect("the transcript");
        text.push_str(
            &json!({
                "type": "user",
                "timestamp": "2026-11-14T18:40:00Z",
                "sessionId": SESSION,
                "message": {"role": "user", "content": "fix the failing tests"}
            })
            .to_string(),
        );
        text.push('\n');
        std::fs::write(&transcript, text).expect("the resumed transcript");

        let again = crate::transcripts::agents_of(
            &claude,
            SESSION,
            &BTreeMap::new(),
            &mut crate::transcripts::AgentEnds::default(),
        );
        assert_eq!(again.metas.len(), 1);
        assert!(
            !again.metas[0].transcript_ended,
            "a new user turn after end_turn means the agent runs again"
        );
        let metas = again.metas.clone();
        let second_rows = running(
            &[session_file()],
            &state_with(seen("claude-opus-5-5", 1_000_000)),
            &|_: &str| metas.clone(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );
        assert_eq!(second_rows.sessions[0].agents_running, 1);
        assert_eq!(second_rows.sessions[0].agents_done, 0);
        assert!(second_rows.sessions[0].agents[0].running);
    }

    #[test]
    fn an_unreadable_meta_still_counts_as_running() {
        let dir = tempdir().expect("tempdir");
        let claude = crate::paths::ClaudePaths::for_config_dir(dir.path(), dir.path());
        let subagents = claude
            .projects_dir()
            .join("-home-sam-projects-atlas")
            .join(SESSION)
            .join("subagents");
        std::fs::create_dir_all(&subagents).expect("the subagents folder");
        std::fs::write(
            subagents.join(format!("agent-{AGENT}.meta.json")),
            "{\"agentType\": ",
        )
        .expect("the unreadable meta");
        std::fs::write(
            subagents.join(format!("agent-{AGENT}.jsonl")),
            format!(
                "{}\n",
                json!({
                    "type": "user",
                    "timestamp": "2026-11-14T18:40:00Z",
                    "sessionId": SESSION,
                    "message": {"role": "user", "content": "fix the failing tests"}
                })
            ),
        )
        .expect("the transcript");

        let agents = crate::transcripts::agents_of(
            &claude,
            SESSION,
            &BTreeMap::new(),
            &mut crate::transcripts::AgentEnds::default(),
        );
        assert_eq!(agents.metas.len(), 1);
        assert_eq!(agents.warnings.len(), 1, "the unreadable meta is a warning");
        assert!(!agents.metas[0].finished);
        assert!(
            !agents.metas[0].transcript_ended,
            "the last entry is not an end_turn answer"
        );
        let metas = agents.metas.clone();
        let settings = settings_eur();
        let history = HistoryDir::at(dir.path(), "atlas");
        let valuer = valuer(&history, &settings, &[]);
        let rows = running(
            &[session_file()],
            &state_with(seen("claude-opus-5-5", 1_000_000)),
            &|_: &str| metas.clone(),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_: &str| project_key(),
        );

        assert_eq!(rows.sessions[0].agents_running, 1);
        assert_eq!(rows.sessions[0].agents_done, 0);
        assert!(rows.sessions[0].agents[0].running);
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
    fn a_closed_row_keeps_the_hour_of_its_first_answer() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:40:00Z"),
                utc("2026-11-14T08:12:00Z"),
                utc("2026-11-14T09:40:00Z"),
                true,
                0,
            ))
            .expect("a session record");
        // `token_records` writes the start of the hour, so the first answer at
        // 08:12 has its record at 08:00.
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T08:00:00Z"),
                "claude-opus-5-5",
                0,
                4_000_000,
            ))
            .expect("a token record");
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T09:00:00Z"),
                "claude-opus-5-5",
                0,
                3_000_000,
            ))
            .expect("a token record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T07:00:00Z"),
            utc("2026-11-14T10:10:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert_eq!(rows.sessions.len(), 1);
        assert_eq!(rows.sessions[0].tokens.out, 7_000_000);
    }

    #[test]
    fn a_closed_rows_last_hour_counts_the_hour_that_overlaps() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&session_record(
                SESSION_11,
                utc("2026-11-14T09:58:00Z"),
                utc("2026-11-14T09:50:00Z"),
                utc("2026-11-14T09:58:00Z"),
                true,
                0,
            ))
            .expect("a session record");
        history
            .append(&token(
                SESSION_11,
                utc("2026-11-14T09:00:00Z"),
                "claude-opus-5-5",
                0,
                3_000_000,
            ))
            .expect("a token record");
        let settings = settings_eur();
        let valuer = valuer(&history, &settings, &[]);
        let rows = closed(
            &history,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T10:20:00Z"),
            Some(&projects()),
            &known_pricing(&valuer, &settings),
        )
        .expect("rows");

        assert_eq!(rows.sessions.len(), 1);
        assert_eq!(
            rows.sessions[0].last_hour.out, 3_000_000,
            "the 09:00 hour overlaps [09:20, 10:20)"
        );
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
            spawn_depth: Some(1),
            tool_use_id: Some("toolu_01Nkn4".to_string()),
            parent: None,
            transcript_ended: false,
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
            spawn_depth: Some(2),
            tool_use_id: Some("toolu_01Nkn4".to_string()),
            parent: Some("a8751b20660a1de6b".to_string()),
            transcript_ended: false,
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
                spawn_depth: Some(1),
                tool_use_id: Some(format!("toolu_{i:04}")),
                parent: None,
                transcript_ended: false,
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
            other.warnings[0].contains(&ledger.sessions_now_file("ledger").display().to_string()),
            "the warning names the file: {}",
            other.warnings[0]
        );
        assert!(
            other.warnings[0].contains("key must be a string"),
            "the warning names the reason: {}",
            other.warnings[0]
        );
    }

    const JOB_A: &str = "flash-task-5c974a53-178a-4254-93b2-b804a111414a";
    const JOB_B: &str = "flash-task-8d2f0e7c-6b1a-4c3d-9e0f-1a2b3c4d5e6f";
    const WORKER_A: &str = "a25abffd-ed93-4296-868f-dbd77aca32ac";
    const HARNESS_KEY: &str = "--home-sam-projects-atlas--";
    const FLASH_MODEL: &str = "deepseek-v4.1-flash:cloud";

    fn harness_store() -> (tempfile::TempDir, crate::harness::HarnessPaths) {
        let tmp = tempdir().expect("tempdir");
        let paths = crate::harness::HarnessPaths::at(
            tmp.path().join("sessions"),
            tmp.path().join("flash-agents"),
        );
        (tmp, paths)
    }

    fn harness_event(kind: &str, time: i64, data: serde_json::Value) -> String {
        serde_json::json!({"type": kind, "seq": 1, "time": time, "data": data}).to_string()
    }

    /// Write one Harness session file; the times are 2026-11-14 09:40 UTC and
    /// later.
    #[allow(clippy::too_many_arguments)]
    fn write_harness_session(
        paths: &crate::harness::HarnessPaths,
        id: &str,
        parent: Option<&str>,
        depth: u32,
        finished: bool,
        input: u64,
        out: u64,
        title: Option<&str>,
    ) {
        let dir = paths.sessions.join(HARNESS_KEY).join(id);
        std::fs::create_dir_all(&dir).expect("a session folder");
        let mut header = serde_json::json!({
            "type": "session",
            "version": 4,
            "id": id,
            "createdAt": 1794696000000_i64,
            "cwd": ATLAS_CWD,
            "isSeeded": false,
            "delegationDepth": depth,
        });
        if let Some(parent) = parent {
            header["parentSession"] = serde_json::json!(parent);
            header["origin"] = serde_json::json!("subagent");
        }
        let mut lines = vec![
            header.to_string(),
            harness_event("turn/start", 1794696001000, serde_json::json!({"turn": 1})),
            harness_event(
                "assistant/message",
                1794696002000,
                serde_json::json!({
                    "turn": 1,
                    "step": 1,
                    "message": {
                        "role": "assistant",
                        "source": {"kind": "model", "provider": "ollama", "model": FLASH_MODEL},
                        "id": "m1"
                    },
                    "usage": {
                        "inputTokens": input,
                        "outputTokens": out,
                        "totalTokens": input + out
                    }
                }),
            ),
        ];
        if let Some(title) = title {
            lines.push(harness_event(
                "session/title",
                1794696003000,
                serde_json::json!({"title": title}),
            ));
        }
        if finished {
            lines.push(harness_event(
                "turn/end",
                1794696004000,
                serde_json::json!({"turn": 1, "reason": {"kind": "completed"}}),
            ));
        }
        let mut text = String::new();
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        std::fs::write(
            dir.join("session.v4.jsonl.zstd"),
            zstd::encode_all(text.as_bytes(), 3).expect("zstd"),
        )
        .expect("a session file");
        std::fs::write(dir.join("session.lock"), []).expect("a lock");
    }

    /// Write a Harness session with `messages` assistant events and a final
    /// `turn/end` when `finished`; every message is 100 input and 50 output.
    fn write_harness_messages(
        paths: &crate::harness::HarnessPaths,
        id: &str,
        parent: Option<&str>,
        depth: u32,
        messages: u32,
        finished: bool,
        last: i64,
    ) {
        let dir = paths.sessions.join(HARNESS_KEY).join(id);
        std::fs::create_dir_all(&dir).expect("a session folder");
        let mut header = serde_json::json!({
            "type": "session",
            "version": 4,
            "id": id,
            "createdAt": last - 3_600_000,
            "cwd": ATLAS_CWD,
            "isSeeded": false,
            "delegationDepth": depth,
        });
        if let Some(parent) = parent {
            header["parentSession"] = serde_json::json!(parent);
            header["origin"] = serde_json::json!("subagent");
        }
        let mut lines = vec![
            header.to_string(),
            harness_event("turn/start", last - 60_000, serde_json::json!({"turn": 1})),
        ];
        for number in 0..messages {
            lines.push(harness_event(
                "assistant/message",
                last - 3_600_000 + i64::from(number) * 1_000,
                serde_json::json!({
                    "turn": 1,
                    "step": 1,
                    "message": {
                        "role": "assistant",
                        "source": {"kind": "model", "provider": "ollama", "model": FLASH_MODEL},
                        "id": format!("m{number}")
                    },
                    "usage": {
                        "inputTokens": 100,
                        "outputTokens": 50,
                        "totalTokens": 150
                    }
                }),
            ));
        }
        if finished {
            lines.push(harness_event(
                "turn/end",
                last,
                serde_json::json!({"turn": 1, "reason": {"kind": "completed"}}),
            ));
        }
        let mut text = String::new();
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        std::fs::write(
            dir.join("session.v4.jsonl.zstd"),
            zstd::encode_all(text.as_bytes(), 3).expect("zstd"),
        )
        .expect("a session file");
        std::fs::write(dir.join("session.lock"), []).expect("a lock");
    }

    fn unknown_pricing() -> Pricing<'static> {
        Pricing::Unknown {
            reason: "no prices in the test".to_string(),
        }
    }

    fn harness_links(
        paths: &crate::harness::HarnessPaths,
        state: &ScanState,
    ) -> BTreeMap<String, String> {
        crate::harness::links(paths, &state.sessions, &state.closed_flash_jobs).owners
    }

    #[test]
    fn a_job_named_in_a_transcript_hangs_under_its_session() {
        let (_dir, paths) = harness_store();
        write_harness_session(&paths, JOB_A, None, 0, false, 10516, 3585, None);
        write_harness_session(&paths, WORKER_A, Some(JOB_A), 1, false, 2200, 1100, None);
        let report = serde_json::json!({"sessionId": JOB_A});
        std::fs::create_dir_all(paths.reports.join("acme").join("patches")).expect("reports");
        std::fs::write(
            paths
                .reports
                .join("acme")
                .join("patches")
                .join("job-3e32c2fc.report.json"),
            report.to_string(),
        )
        .expect("a report");
        let mut state = state_with(seen(FLASH_MODEL, 3585));
        state
            .sessions
            .get_mut(SESSION)
            .expect("the session")
            .flash_jobs = BTreeSet::from(["job-3e32c2fc".to_string()]);
        let now = utc("2026-11-14T19:00:00Z");
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));
        let claude_cwd = BTreeMap::from([(SESSION.to_string(), ATLAS_CWD.to_string())]);

        let mut rows = running(
            &[session_file()],
            &state,
            &|_| Vec::new(),
            Some(&projects()),
            &unknown_pricing(),
            "atlas",
            now,
            &|_| project_key(),
        );
        let out = harness_rows(
            &harness.sessions,
            &harness_links(&paths, &state),
            &claude_cwd,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );
        let agents = out.agents.get(SESSION).expect("the session's flash agents");
        assert_eq!(agents.len(), 2);
        let job = agents
            .iter()
            .find(|agent| agent.agent == JOB_A)
            .expect("the job");
        assert_eq!(job.kind.as_deref(), Some("flash"));
        assert_eq!(job.parent, None);
        assert!(job.running);
        assert_eq!(job.model.as_deref(), Some(FLASH_MODEL));
        let worker = agents
            .iter()
            .find(|agent| agent.agent == WORKER_A)
            .expect("the worker");
        assert_eq!(worker.parent.as_deref(), Some(JOB_A));
        add_agents(
            rows.sessions
                .iter_mut()
                .find(|row| row.session == SESSION)
                .expect("the Claude row"),
            agents.clone(),
        );
        let row = rows
            .sessions
            .iter()
            .find(|row| row.session == SESSION)
            .expect("the Claude row");
        assert_eq!(row.agents_running, 2);
        assert_eq!(row.agents_done, 0);
    }

    #[test]
    fn two_sessions_in_one_repository_keep_their_own_jobs() {
        let (_dir, paths) = harness_store();
        write_harness_session(&paths, JOB_A, None, 0, true, 100, 50, None);
        write_harness_session(&paths, JOB_B, None, 0, true, 200, 70, None);
        let mut state = state_with(seen(FLASH_MODEL, 3585));
        state
            .sessions
            .get_mut(SESSION)
            .expect("the session")
            .flash_jobs = BTreeSet::from([JOB_A.to_string()]);
        let mut other = seen(FLASH_MODEL, 3585);
        other.flash_jobs = BTreeSet::from([JOB_B.to_string()]);
        state.sessions.insert(SESSION_11.to_string(), other);
        let mut other_file = session_file();
        other_file.pid = 41823;
        other_file.session_id = SESSION_11.to_string();
        let now = utc("2026-11-14T19:00:00Z");
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));
        let claude_cwd = BTreeMap::from([
            (SESSION.to_string(), ATLAS_CWD.to_string()),
            (SESSION_11.to_string(), ATLAS_CWD.to_string()),
        ]);
        let mut rows = running(
            &[session_file(), other_file],
            &state,
            &|_| Vec::new(),
            Some(&projects()),
            &unknown_pricing(),
            "atlas",
            now,
            &|_| project_key(),
        );

        let out = harness_rows(
            &harness.sessions,
            &harness_links(&paths, &state),
            &claude_cwd,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );
        for (session, agents) in &out.agents {
            add_agents(
                rows.sessions
                    .iter_mut()
                    .find(|row| &row.session == session)
                    .expect("the session's row"),
                agents.clone(),
            );
        }

        assert_eq!(rows.sessions.len(), 2);
        let first = rows
            .sessions
            .iter()
            .find(|row| row.session == SESSION)
            .expect("the first row");
        let second = rows
            .sessions
            .iter()
            .find(|row| row.session == SESSION_11)
            .expect("the second row");
        assert_eq!(
            first
                .agents
                .iter()
                .map(|agent| agent.agent.as_str())
                .collect::<Vec<_>>(),
            vec![JOB_A]
        );
        assert_eq!(
            second
                .agents
                .iter()
                .map(|agent| agent.agent.as_str())
                .collect::<Vec<_>>(),
            vec![JOB_B]
        );
    }

    #[test]
    fn a_session_without_a_parent_is_its_own_row() {
        let (_dir, paths) = harness_store();
        write_harness_session(
            &paths,
            JOB_A,
            None,
            0,
            false,
            10516,
            3585,
            Some("atlas link test"),
        );
        write_harness_session(&paths, WORKER_A, Some(JOB_A), 1, true, 2200, 1100, None);
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));

        let out = harness_rows(
            &harness.sessions,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        assert!(out.agents.is_empty());
        assert_eq!(out.rows.sessions.len(), 1);
        let row = &out.rows.sessions[0];
        assert_eq!(row.session, JOB_A);
        assert_eq!(row.provider, "DeepSeek Harness");
        assert_eq!(row.model.as_deref(), Some(FLASH_MODEL));
        assert_eq!(row.tokens.input, 10516);
        assert_eq!(row.tokens.out, 3585);
        assert_eq!(row.agents.len(), 1);
        assert_eq!(row.agents[0].agent, WORKER_A);
        assert_eq!(row.agents[0].parent.as_deref(), Some(JOB_A));
        assert_eq!(row.agents_running, 0);
        assert_eq!(row.agents_done, 1);
    }

    #[test]
    fn a_codex_session_is_a_row_while_held_or_recent() {
        let now = utc("2026-11-14T19:00:00Z");
        let old = crate::codex_sessions::CodexSession {
            id: "3f7b2c1e-9a4d-4e8f-8b6a-2c5d9e1f7a30".to_string(),
            cwd: PathBuf::from(ATLAS_CWD),
            model: Some("gpt-6.1-sol".to_string()),
            started: utc("2026-11-14T16:00:00Z"),
            last: utc("2026-11-14T16:30:00Z"),
            tokens: Tokens {
                input: 12000,
                out: 900,
                cw: 300,
                cr: 8000,
            },
            last_hour: Some(Tokens {
                input: 1200,
                out: 90,
                cw: 0,
                cr: 800,
            }),
            busy: true,
            file: PathBuf::from("/home/sam/.codex/sessions/2026/11/14/rollout-old.jsonl"),
        };

        let quiet = codex_rows(
            std::slice::from_ref(&old),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            now,
            &|_| project_key(),
        );
        assert!(
            quiet.sessions.is_empty(),
            "a quiet file that no process holds is not a row"
        );

        let open = BTreeSet::from([old.file.clone()]);
        let rows = codex_rows(
            std::slice::from_ref(&old),
            &open,
            None,
            &unknown_pricing(),
            "atlas",
            now,
            &|_| project_key(),
        );
        assert_eq!(rows.sessions.len(), 1);
        let row = &rows.sessions[0];
        assert_eq!(row.session, old.id);
        assert_eq!(row.provider, "Codex");
        assert_eq!(row.status, LiveStatus::Busy);
        assert_eq!(row.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(row.tokens, old.tokens);
        assert_eq!(row.started, old.started);
        assert_eq!(row.since, old.last);
        assert!(row.agents.is_empty());
        assert_eq!(row.agents_running, 0);

        let mut recent = old.clone();
        recent.last = now - Duration::minutes(5);
        recent.busy = false;
        let rows = codex_rows(
            &[recent],
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            now,
            &|_| project_key(),
        );
        assert_eq!(rows.sessions.len(), 1);
        assert_eq!(rows.sessions[0].status, LiveStatus::Idle);
    }

    #[test]
    fn a_codex_row_values_its_last_hour_and_says_when_it_is_not_known() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let settings = settings_eur();
        let mut prices = Prices::default();
        prices.take(
            BTreeMap::from([(
                crate::money::price_key("anthropic", "gpt-6.1-sol"),
                ModelPrice {
                    input: 1.0,
                    output: 2.0,
                    cache_write: 1.0,
                    cache_read: 0.1,
                    tiered: false,
                },
            )]),
            utc("2026-01-01T00:00:00Z"),
        );
        let valuer = valuer_with(&history, &settings, &[], &prices);
        let now = utc("2026-11-14T19:00:00Z");
        let session = crate::codex_sessions::CodexSession {
            id: "3f7b2c1e-9a4d-4e8f-8b6a-2c5d9e1f7a30".to_string(),
            cwd: PathBuf::from(ATLAS_CWD),
            model: Some("gpt-6.1-sol".to_string()),
            started: now - Duration::minutes(30),
            last: now - Duration::minutes(5),
            tokens: Tokens {
                input: 1000,
                out: 100,
                cw: 0,
                cr: 500,
            },
            last_hour: Some(Tokens {
                input: 400,
                out: 40,
                cw: 0,
                cr: 200,
            }),
            busy: false,
            file: PathBuf::from("/home/sam/.codex/sessions/2026/11/14/rollout-now.jsonl"),
        };

        let rows = codex_rows(
            std::slice::from_ref(&session),
            &BTreeSet::new(),
            None,
            &known_pricing(&valuer, &settings),
            "atlas",
            now,
            &|_| project_key(),
        );
        assert_eq!(rows.sessions.len(), 1);
        let row = &rows.sessions[0];
        assert_eq!(Some(row.last_hour), session.last_hour);
        assert!(
            row.last_hour_values.api.is_some(),
            "the last hour is valued like the tokens: {:?}",
            row.last_hour_values
        );

        let mut unknown = session.clone();
        unknown.last_hour = None;
        let rows = codex_rows(
            &[unknown],
            &BTreeSet::new(),
            None,
            &known_pricing(&valuer, &settings),
            "atlas",
            now,
            &|_| project_key(),
        );
        let row = &rows.sessions[0];
        assert_eq!(row.last_hour, Tokens::default());
        assert_eq!(
            row.last_hour_values.api_missing.as_deref(),
            Some(CODEX_LAST_HOUR_UNKNOWN),
            "an unknown last hour says so, never 0: {:?}",
            row.last_hour_values
        );
        assert_eq!(
            row.last_hour_values.subscription_missing.as_deref(),
            Some(CODEX_LAST_HOUR_UNKNOWN)
        );
    }

    #[test]
    fn no_title_reaches_the_history() {
        let (dir, paths) = harness_store();
        let title = "atlas link test";
        write_harness_session(&paths, JOB_A, None, 0, true, 10516, 3585, None);
        write_harness_session(
            &paths,
            WORKER_A,
            Some(JOB_A),
            1,
            false,
            2200,
            1100,
            Some(title),
        );
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));

        let out = harness_rows(
            &harness.sessions,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        assert_eq!(
            out.rows.sessions[0].agents[0].task.as_deref(),
            Some(title),
            "the title is shown live"
        );
        let history = HistoryDir::at(&dir.path().join("history"), "atlas");
        write_report(&history, &out.rows.sessions, utc("2026-11-14T19:00:00Z"))
            .expect("the sessions report");
        let recorder =
            crate::history::Recorder::new(HistoryDir::at(&dir.path().join("history"), "atlas"));
        recorder.tokens(&out.tokens).expect("the token lines");

        let mut found = String::new();
        let mut files = 0;
        collect_text(&dir.path().join("history"), &mut found, &mut files);
        assert!(files > 0, "a token line was written");
        assert!(!found.contains(title), "no history file holds the title");
    }

    fn collect_text(dir: &std::path::Path, text: &mut String, files: &mut usize) {
        for entry in std::fs::read_dir(dir).expect("a folder") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                collect_text(&path, text, files);
            } else {
                *files += 1;
                text.push_str(&std::fs::read_to_string(&path).expect("a history file"));
            }
        }
    }

    #[test]
    fn proof_a_worker_whose_job_is_out_of_range_still_hangs_under_the_owner() {
        let (_dir, paths) = harness_store();
        // Only the worker is in the range (its job's file is older than `since`).
        write_harness_session(&paths, WORKER_A, Some(JOB_A), 1, false, 2200, 1100, None);
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));
        let owners = BTreeMap::from([(JOB_A.to_string(), SESSION.to_string())]);
        let claude_cwd = BTreeMap::from([(SESSION.to_string(), ATLAS_CWD.to_string())]);

        let out = harness_rows(
            &harness.sessions,
            &owners,
            &claude_cwd,
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        assert!(
            out.rows.sessions.is_empty(),
            "the worker became its own DeepSeek Harness row"
        );
        assert_eq!(out.agents.get(SESSION).map(Vec::len), Some(1));
    }

    #[test]
    fn a_worker_without_its_job_and_owner_is_its_own_row() {
        let (_dir, paths) = harness_store();
        write_harness_session(&paths, WORKER_A, Some(JOB_A), 1, true, 2200, 1100, None);
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));

        let out = harness_rows(
            &harness.sessions,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        assert!(out.agents.is_empty());
        assert_eq!(out.rows.sessions.len(), 1);
        let row = &out.rows.sessions[0];
        assert_eq!(row.session, WORKER_A);
        assert_eq!(row.provider, "DeepSeek Harness");
    }

    /// Write a Harness session whose assistant messages are `(time, usage)`;
    /// `None` is a message without `usage`, an answer with no tokens.
    fn write_harness_answers(
        paths: &crate::harness::HarnessPaths,
        id: &str,
        answers: &[(i64, Option<(u64, u64)>)],
    ) {
        let dir = paths.sessions.join(HARNESS_KEY).join(id);
        std::fs::create_dir_all(&dir).expect("a session folder");
        let first = answers.first().map(|(at, _)| *at).unwrap_or(0);
        let header = serde_json::json!({
            "type": "session",
            "version": 4,
            "id": id,
            "createdAt": first - 3_600_000,
            "cwd": ATLAS_CWD,
            "isSeeded": false,
            "delegationDepth": 0,
        });
        let mut lines = vec![
            header.to_string(),
            harness_event("turn/start", first - 60_000, serde_json::json!({"turn": 1})),
        ];
        for (number, (at, usage)) in answers.iter().enumerate() {
            let mut data = serde_json::json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "role": "assistant",
                    "source": {"kind": "model", "provider": "ollama", "model": FLASH_MODEL},
                    "id": format!("m{number}")
                },
            });
            if let Some((input, out)) = usage {
                data["usage"] = serde_json::json!({
                    "inputTokens": input,
                    "outputTokens": out,
                    "totalTokens": input + out
                });
            }
            lines.push(harness_event("assistant/message", *at, data));
        }
        let last = answers.last().map(|(at, _)| *at).unwrap_or(first);
        lines.push(harness_event(
            "turn/end",
            last + 1_000,
            serde_json::json!({"turn": 1, "reason": {"kind": "completed"}}),
        ));
        let mut text = String::new();
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        std::fs::write(
            dir.join("session.v4.jsonl.zstd"),
            zstd::encode_all(text.as_bytes(), 3).expect("zstd"),
        )
        .expect("a session file");
        std::fs::write(dir.join("session.lock"), []).expect("a lock");
    }

    #[test]
    fn harness_token_lines_are_per_hour_of_the_answers() {
        let (_dir, paths) = harness_store();
        let first = 1_794_696_000_000_i64;
        let second = first + 3_600_000;
        write_harness_answers(
            &paths,
            JOB_A,
            &[(first, Some((100, 50))), (second, Some((200, 70)))],
        );
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));
        let session = &harness.sessions[0];

        let out = harness_rows(
            &harness.sessions,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        let hour_of = |ms: i64| {
            transcripts::hour_start(Utc.timestamp_millis_opt(ms).single().expect("a time"))
        };
        assert_eq!(
            out.tokens.len(),
            2,
            "one line per hour of the new answers: {:?}",
            out.tokens
        );
        let mut lines: Vec<(DateTime<Utc>, u64, u64, u32)> = out
            .tokens
            .iter()
            .map(|record| (record.t, record.input, record.out, record.agent_answers))
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            vec![(hour_of(first), 100, 50, 1), (hour_of(second), 200, 70, 1),]
        );
        assert_eq!(
            out.tokens.iter().map(|record| record.input).sum::<u64>(),
            session.tokens.input,
            "the lines' sums are the delta since the recorded total"
        );
        assert_eq!(
            out.tokens.iter().map(|record| record.out).sum::<u64>(),
            session.tokens.out
        );
    }

    #[test]
    fn an_answer_without_usage_adds_a_line_with_no_tokens() {
        let (_dir, paths) = harness_store();
        let first = 1_794_696_000_000_i64;
        write_harness_answers(
            &paths,
            JOB_A,
            &[(first, Some((100, 50))), (first + 1_000, None)],
        );
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));

        let out = harness_rows(
            &harness.sessions,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        assert_eq!(out.tokens.len(), 1, "the two answers share their hour");
        assert_eq!(out.tokens[0].input, 100);
        assert_eq!(out.tokens[0].out, 50);
        assert_eq!(
            out.tokens[0].agent_answers, 2,
            "a message without usage is still an answer"
        );
    }

    #[test]
    fn harness_agent_answers_count_the_new_messages() {
        let (_dir, paths) = harness_store();
        // Two assistant messages, then a third one in a later pass.
        write_harness_messages(&paths, JOB_A, None, 0, 2, true, 1_794_696_004_000);
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));
        let session = &harness.sessions[0];

        let first = harness_rows(
            &harness.sessions,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        assert_eq!(first.tokens.len(), 1);
        assert_eq!(first.tokens[0].agent_answers, 2);
        assert_eq!(first.tokens[0].input, 200);
        assert_eq!(
            first.tokens[0].t,
            transcripts::hour_start(session.last_message),
            "the line is dated at the hour of the last answer S-1 uses"
        );
        assert_ne!(
            first.tokens[0].t,
            transcripts::hour_start(session.last),
            "the last event (turn/end) is not the answer's hour"
        );

        write_harness_messages(&paths, JOB_A, None, 0, 3, true, 1_794_696_005_000);
        let harness = crate::harness::sessions(&paths, utc("2026-11-01T00:00:00Z"));
        let second = harness_rows(
            &harness.sessions,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &first.recorded,
            &BTreeSet::new(),
            None,
            &unknown_pricing(),
            "atlas",
            utc("2026-11-14T19:00:00Z"),
            &|_| project_key(),
        );

        assert_eq!(second.tokens.len(), 1);
        assert_eq!(second.tokens[0].agent_answers, 1);
        assert_eq!(second.tokens[0].input, 100);
    }
}
