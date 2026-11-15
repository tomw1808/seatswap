//! The large window: its label, its areas and the one command that opens it
//! (spec §3; docs/implementation.md "app: GUI", "The large window").
//!
//! The window itself is a second Tauri webview window with its own page
//! (`window.html`). The pop-up keeps `panel.rs`: both windows exist at the same
//! time, and a `snapshot` event reaches each of them.

#[cfg(feature = "gui")]
use std::collections::{BTreeMap, BTreeSet};

#[cfg(feature = "gui")]
use chrono::{DateTime, Duration, Local, TimeZone, Utc};
#[cfg(feature = "gui")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "gui")]
use seatswap_core::api::Usage;
#[cfg(all(feature = "gui", test))]
use seatswap_core::burn::decide::Regime;
#[cfg(all(feature = "gui", test))]
use seatswap_core::burn::plan::Inputs;
#[cfg(feature = "gui")]
use seatswap_core::feed::Feed;
#[cfg(feature = "gui")]
use seatswap_core::history::HistoryDir;
#[cfg(feature = "gui")]
use seatswap_core::live_sessions::{
    switch_kind, LiveSession, LiveStatus, OtherHost, SwitchKind, Values,
};
#[cfg(feature = "gui")]
use seatswap_core::money::{Fx, Prices};
#[cfg(feature = "gui")]
use seatswap_core::paths::AppPaths;
#[cfg(feature = "gui")]
use seatswap_core::project::ProjectKey;
#[cfg(feature = "gui")]
use seatswap_core::projects::{self, ProjectId};
#[cfg(feature = "gui")]
use seatswap_core::stats::{
    self, activity, forecast, overview, projects as stats_projects, subscriptions, weeks, Ctx,
    Empty, Query,
};
#[cfg(feature = "gui")]
use seatswap_core::store::{self, Account, Settings, StatuslineConfig};
#[cfg(feature = "gui")]
use seatswap_core::switcher::LiveState;
#[cfg(feature = "gui")]
use seatswap_core::transcripts::Tokens;

#[cfg(feature = "gui")]
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

#[cfg(feature = "gui")]
use crate::statusline::render::AccountFacts;
#[cfg(feature = "gui")]
use crate::statusline::{self, FactsSource, PreviewLine, RecordedKeys, Scenario};

#[cfg(feature = "gui")]
use super::burn_rt::KeptForecast;

/// Window label used by `tauri.conf.json`'s capability and by every command.
#[cfg(feature = "gui")]
pub(crate) const WINDOW_LABEL: &str = "main";

/// The four areas the window can show, in the mock-up's order.
pub(crate) const AREAS: [&str; 4] = ["sessions", "statistics", "statusline", "settings"];

/// Start size of the large window (spec §3).
#[cfg(feature = "gui")]
const WINDOW_WIDTH: f64 = 1280.0;
#[cfg(feature = "gui")]
const WINDOW_HEIGHT: f64 = 800.0;

/// `"settings/accounts"` → `("settings", Some("accounts"))`;
/// `"settings/accounts?<id>"` → `("settings", Some("accounts?<id>"))`;
/// `"sessions?<session id>"` → `("sessions", Some("<session id>"))`;
/// `"settings"` → `("settings", None)`. An item without a `/` is only for the
/// sessions area: `"settings?accounts"` is an error. Anything else is an error
/// with the reason: an unknown area, an empty sub-area, one that is not a plain
/// name (`a-z`, `0-9`, `-`) optionally followed by `?<item>` (the item a plain
/// name too), or more than one `/`.
pub(crate) fn parse_target(target: &str) -> Result<(&str, Option<&str>), String> {
    if !target.contains('/') {
        let (area, item) = match target.split_once('?') {
            Some((area, item)) => (area, Some(item)),
            None => (target, None),
        };
        if !AREAS.contains(&area) {
            return Err(format!(
                "unknown area {area:?}: use one of {}",
                AREAS.join(", ")
            ));
        }
        let Some(item) = item else {
            return Ok((area, None));
        };
        if area != "sessions" {
            return Err(format!(
                "{target:?}: only the sessions area takes an item after \"?\""
            ));
        }
        if !is_slug(item) {
            return Err(format!(
                "the item {item:?} may hold only a-z, 0-9 and \"-\""
            ));
        }
        return Ok((area, Some(item)));
    }
    let mut parts = target.split('/');
    let area = parts.next().unwrap_or_default();
    if !AREAS.contains(&area) {
        return Err(format!(
            "unknown area {area:?}: use one of {}",
            AREAS.join(", ")
        ));
    }
    match (parts.next(), parts.next()) {
        (None, None) => Ok((area, None)),
        (Some(""), None) => Err(format!(
            "the sub-area of {target:?} is empty: use <area>/<sub>"
        )),
        (Some(sub), None) if is_sub_area(sub) => Ok((area, Some(sub))),
        (Some(sub), None) => Err(format!(
            "the sub-area {sub:?} may hold only a-z, 0-9 and \"-\", optionally followed by ?<item>"
        )),
        _ => Err(format!(
            "{target:?} names more than one sub-area: use <area> or <area>/<sub>"
        )),
    }
}

/// A sub-area is a name like `accounts` or `forecast`, optionally followed by
/// `?<item>` (`settings/accounts?<account id>`): lower-case letters, digits and
/// `-`. It travels over the socket and in the page's address unencoded, so
/// nothing else may be in it.
fn is_sub_area(sub: &str) -> bool {
    match sub.split_once('?') {
        Some((name, item)) => is_slug(name) && is_slug(item),
        None => is_slug(sub),
    }
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Create the window (1280 × 800, resizable, decorated, in the taskbar, not
/// always on top) or focus it, and send it `target`.
///
/// A newly created window loads `window.html#<target>`; an existing one is
/// shown, un-minimized, focused and sent `open-area`. The two windows each have
/// their own event name, because `emit` reaches every window: the pop-up keeps
/// `navigate` (decision 3 of the plan's lead).
#[cfg(feature = "gui")]
pub(crate) fn show(app: &AppHandle, target: &str) -> Result<(), String> {
    parse_target(target)?;
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        window.show().map_err(|error| error.to_string())?;
        window.unminimize().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        window
            .emit("open-area", target)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    create(app, target)
}

/// Build the large window on its first opening, with the target in its address.
#[cfg(feature = "gui")]
fn create(app: &AppHandle, target: &str) -> Result<(), String> {
    let initial = target.to_string();
    let window =
        WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("window.html".into()))
            .title("seatswap")
            .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
            .resizable(true)
            .decorations(true)
            .always_on_top(false)
            .skip_taskbar(false)
            // Before the page's scripts run, so the window opens on its target
            // instead of drawing Sessions first and jumping.
            .initialization_script(format!("window.location.hash = {initial:?};"))
            .build()
            .map_err(|error| format!("cannot open the window: {error}"))?;
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let Some(state) = handle.try_state::<super::GuiState>() else {
                return;
            };
            let Ok(client) = state.sessions_commands() else {
                return;
            };
            // The sessions thread may be in a pass: never hold the main thread.
            std::thread::spawn(move || {
                let _ = client.unsubscribe();
            });
        }
    });
    window
        .show()
        .map_err(|error| format!("cannot show the window: {error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("cannot focus the window: {error}"))
}

/// `open_window("settings")` from a page: the same as `show`.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) fn open_window(app: AppHandle, target: String) -> Result<(), String> {
    show(&app, &target)
}

/// The sessions view. The sessions thread answers from what it holds; the
/// command runs on a blocking thread, so a pass never holds up the window.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn sessions_view(
    state: State<'_, super::GuiState>,
    options: SessionsOptions,
) -> Result<SessionsView, String> {
    let client = state.sessions_commands()?;
    let view = tauri::async_runtime::spawn_blocking(move || client.view(options))
        .await
        .map_err(|error| error.to_string())??;
    Ok(view)
}

/// Ask the sessions thread to push a view after every pass, for `options`.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn sessions_subscribe(
    state: State<'_, super::GuiState>,
    options: SessionsOptions,
) -> Result<(), String> {
    let client = state.sessions_commands()?;
    tauri::async_runtime::spawn_blocking(move || client.subscribe(options))
        .await
        .map_err(|error| error.to_string())?
}

/// Stop the pushed views; the thread drops the transcript folder watches.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn sessions_unsubscribe(state: State<'_, super::GuiState>) -> Result<(), String> {
    let client = state.sessions_commands()?;
    tauri::async_runtime::spawn_blocking(move || client.unsubscribe())
        .await
        .map_err(|error| error.to_string())?
}

/// How far back the closed sessions are listed (spec §3.8).
#[cfg(feature = "gui")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ClosedSince {
    /// The default: closed sessions are not listed at all.
    #[default]
    None,
    Day,
    Week,
    Month,
}

#[cfg(feature = "gui")]
impl ClosedSince {
    /// The first moment a closed session may have closed: 24 h, 7 d or 30 d
    /// before `now`; `None` for [`ClosedSince::None`].
    pub(crate) fn since(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let days = match self {
            ClosedSince::None => return None,
            ClosedSince::Day => 1,
            ClosedSince::Week => 7,
            ClosedSince::Month => 30,
        };
        Some(now - Duration::days(days))
    }
}

/// What the Sessions area asks for. `host` is `None` for all hosts.
#[cfg(feature = "gui")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionsOptions {
    pub closed: ClosedSince,
    pub host: Option<String>,
}

/// One host the view knows about.
#[cfg(feature = "gui")]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostState {
    pub host: String,
    /// When its rows were last known (`this` host: the pass time).
    pub at: DateTime<Utc>,
    /// Another host's report is older than `REPORT_STALE_MIN`; never this host.
    pub stale: bool,
    pub this: bool,
}

/// What a switch now costs this host's running sessions (spec §2.11).
#[cfg(feature = "gui")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwitchEffect {
    pub loses_thinking: u32,
    pub loses_thinking_busy: u32,
    /// The distinct model ids of the sessions counted in `loses_thinking`, in
    /// the order their first session is listed.
    pub loses_thinking_models: Vec<String>,
    pub resend: u32,
    pub free: u32,
}

/// Which group a [`Sum`] belongs to (spec §3.8). The page looks a row's or a
/// card's numbers up by this key; it never adds values itself. The page shows
/// the provider group too, so a provider key is here beside the brief's four
/// (`All`, `Project`, `ProjectModel`, `Model`).
#[cfg(feature = "gui")]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SumOf {
    All,
    Provider(String),
    Project(ProjectId),
    ProjectModel(ProjectId, Option<String>),
    Model(Option<String>),
}

#[cfg(feature = "gui")]
impl Serialize for SumOf {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            SumOf::All => map.serialize_entry("kind", "all")?,
            SumOf::Provider(provider) => {
                map.serialize_entry("kind", "provider")?;
                map.serialize_entry("provider", provider)?;
            }
            SumOf::Project(project) => {
                map.serialize_entry("kind", "project")?;
                map.serialize_entry("project", project)?;
            }
            SumOf::ProjectModel(project, model) => {
                map.serialize_entry("kind", "projectModel")?;
                map.serialize_entry("project", project)?;
                map.serialize_entry("model", model)?;
            }
            SumOf::Model(model) => {
                map.serialize_entry("kind", "model")?;
                map.serialize_entry("model", model)?;
            }
        }
        map.end()
    }
}

/// One group's numbers, built over the listed sessions only (spec §3.8): every
/// total counts exactly what the page lists, and the page adds nothing.
#[cfg(feature = "gui")]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Sum {
    pub of: SumOf,
    pub sessions: u32,
    pub tokens: Tokens,
    pub last_hour: Tokens,
    pub values: Values,
}

/// What the Sessions area shows (spec §3.8). The groups' numbers come from
/// `sums`, so they always agree with the list.
#[cfg(feature = "gui")]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionsView {
    /// The pass time this view was built from.
    pub at: DateTime<Utc>,
    /// The history's host name; `None` when the thread runs without a history.
    pub this_host: Option<String>,
    /// This host first, then the other hosts by name.
    pub hosts: Vec<HostState>,
    /// This host's running rows in their order, then the other hosts' by host,
    /// then the closed ones newest first.
    pub sessions: Vec<LiveSession>,
    /// Every group the page shows, over the listed sessions only.
    pub sums: Vec<Sum>,
    /// This host's running sessions only, whatever the host filter says.
    pub switch_effect: SwitchEffect,
    pub currency: Option<String>,
    pub unavailable: Option<String>,
}

/// The one way the held facts become what the page shows. `at` is the pass
/// time; `other_hosts` is the last other-host read; `thinking` is each running
/// session's answers with thinking since the user's last turn.
#[cfg(feature = "gui")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn view_of(
    at: DateTime<Utc>,
    this_host: Option<String>,
    running: &[LiveSession],
    closed: &[LiveSession],
    other_hosts: &[OtherHost],
    thinking: &BTreeMap<String, u32>,
    unavailable: Option<&str>,
    currency: Option<&str>,
    options: &SessionsOptions,
) -> SessionsView {
    let mut hosts = Vec::new();
    if let Some(host) = &this_host {
        hosts.push(HostState {
            host: host.clone(),
            at,
            stale: false,
            this: true,
        });
    }
    let mut others: Vec<&OtherHost> = other_hosts.iter().collect();
    others.sort_by(|a, b| a.host.cmp(&b.host));
    for other in &others {
        hosts.push(HostState {
            host: other.host.clone(),
            at: other.at,
            stale: other.stale,
            this: false,
        });
    }

    let filter = options.host.as_deref();
    let known = filter.is_none_or(|wanted| hosts.iter().any(|host| host.host == wanted));
    let mut sessions = Vec::new();
    if known {
        let this_host_listed =
            filter.is_none() || (this_host.is_some() && filter == this_host.as_deref());
        if this_host_listed {
            sessions.extend(running.iter().cloned());
        }
        for other in &others {
            if filter.is_none() || filter == Some(other.host.as_str()) {
                sessions.extend(other.sessions.iter().cloned());
            }
        }
        if let Some(since) = options.closed.since(at) {
            let mut closed_rows: Vec<&LiveSession> = closed
                .iter()
                .filter(|row| row.since >= since && row.since < at)
                .filter(|row| filter.is_none() || filter == Some(row.host.as_str()))
                .collect();
            closed_rows.sort_by_key(|row| std::cmp::Reverse(row.since));
            // A session listed as running already is the newer fact: its closed
            // row from the history is not listed a second time.
            let listed: BTreeSet<(String, String)> = sessions
                .iter()
                .map(|row| (row.host.clone(), row.session.clone()))
                .collect();
            sessions.extend(
                closed_rows
                    .into_iter()
                    .filter(|row| !listed.contains(&(row.host.clone(), row.session.clone())))
                    .cloned(),
            );
        }
    }

    let mut switch_effect = SwitchEffect::default();
    for row in running {
        match switch_kind(row, thinking.get(&row.session).copied().unwrap_or(0), at) {
            SwitchKind::LosesThinking => {
                switch_effect.loses_thinking += 1;
                if let Some(model) = &row.model {
                    if !switch_effect.loses_thinking_models.contains(model) {
                        switch_effect.loses_thinking_models.push(model.clone());
                    }
                }
                if row.status == LiveStatus::Busy {
                    switch_effect.loses_thinking_busy += 1;
                }
            }
            SwitchKind::Resend => switch_effect.resend += 1,
            SwitchKind::Free => switch_effect.free += 1,
        }
    }

    let sums = sums_of(&sessions);
    SessionsView {
        at,
        this_host,
        hosts,
        sessions,
        sums,
        switch_effect,
        currency: currency.map(str::to_string),
        unavailable: unavailable.map(str::to_string),
    }
}

/// Every group's numbers over exactly `sessions`: all listed, each provider,
/// each project, each project's model, each model. A key exists when it has a
/// session; `All` exists even for an empty list, so the page always has the
/// total to look up.
#[cfg(feature = "gui")]
fn sums_of(sessions: &[LiveSession]) -> Vec<Sum> {
    type Group<'a> = (u32, Tokens, Tokens, Vec<&'a Values>);
    let mut groups: BTreeMap<SumOf, Group<'_>> = BTreeMap::new();
    groups.insert(
        SumOf::All,
        (0, Tokens::default(), Tokens::default(), Vec::new()),
    );
    for row in sessions {
        let keys = [
            SumOf::All,
            SumOf::Provider(row.provider.clone()),
            SumOf::Project(row.project_id.clone()),
            SumOf::ProjectModel(row.project_id.clone(), row.model.clone()),
            SumOf::Model(row.model.clone()),
        ];
        for key in keys {
            let group = groups
                .entry(key)
                .or_insert_with(|| (0, Tokens::default(), Tokens::default(), Vec::new()));
            group.0 += 1;
            group.1 += row.tokens;
            group.2 += row.last_hour;
            group.3.push(&row.values);
        }
    }
    groups
        .into_iter()
        .map(|(of, (sessions, tokens, last_hour, values))| Sum {
            of,
            sessions,
            tokens,
            last_hour,
            values: Values::sum(&values),
        })
        .collect()
}

// ---- the Statistics area's commands (step-c3.md, Task 8) ----

/// A statistics page: the data, or the "nothing recorded" marker.
#[cfg(feature = "gui")]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Page<T> {
    Empty(Empty),
    Data(T),
}

/// Everything a page reads besides the history, loaded once per command.
#[cfg(feature = "gui")]
#[derive(Debug)]
pub(crate) struct StatsParts {
    accounts: Vec<Account>,
    settings: Settings,
    prices: Prices,
    fx: Fx,
    projects: projects::Projects,
}

#[cfg(feature = "gui")]
impl StatsParts {
    pub(crate) fn ctx<'a, Tz: TimeZone + Copy>(
        &'a self,
        history: &'a HistoryDir,
        now: DateTime<Utc>,
        zone: Tz,
    ) -> Ctx<'a, Tz> {
        Ctx {
            history,
            accounts: &self.accounts,
            settings: &self.settings,
            prices: &self.prices,
            fx: &self.fx,
            projects: &self.projects,
            now,
            zone,
        }
    }
}

/// Read the prices and the dollar rate the app shows money with. Any failure
/// is the text the page shows.
#[cfg(feature = "gui")]
pub(crate) fn load_prices_and_fx(app: &AppPaths) -> Result<(Prices, Fx), String> {
    let prices =
        Prices::load(app).map_err(|error| format!("the prices cannot be read: {error}"))?;
    let fx = Fx::load(app).map_err(|error| format!("the dollar rate cannot be read: {error}"))?;
    Ok((prices, fx))
}

/// Load the accounts file, the prices, the dollar rate and the projects for
/// one statistics command. Any failure is the text the page shows.
#[cfg(feature = "gui")]
pub(crate) fn stats_ctx_parts(app: &AppPaths, history: &HistoryDir) -> Result<StatsParts, String> {
    let file = store::load_accounts(app)
        .map_err(|error| format!("the accounts cannot be read: {error}"))?;
    let (prices, fx) = load_prices_and_fx(app)?;
    let (projects, warnings) = projects::load(history, &file.accounts)
        .map_err(|error| format!("the projects cannot be read: {error}"))?;
    for warning in warnings {
        eprintln!("seatswap: warning: {warning}");
    }
    Ok(StatsParts {
        accounts: file.accounts,
        settings: file.settings,
        prices,
        fx,
        projects,
    })
}

/// The one way a page becomes [`Page`]: "nothing recorded" first, else the
/// page's own numbers.
#[cfg(feature = "gui")]
fn stats_page<T, Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
    compute: impl FnOnce(&Ctx<'_, Tz>) -> Result<T, stats::StatsError>,
) -> Result<Page<T>, String> {
    let ctx = parts.ctx(history, now, zone);
    let range = stats::range_of(query.range, &ctx).map_err(|error| error.to_string())?;
    match stats::empty(&ctx, &range).map_err(|error| error.to_string())? {
        Some(empty) => Ok(Page::Empty(empty)),
        None => compute(&ctx)
            .map(Page::Data)
            .map_err(|error| error.to_string()),
    }
}

#[cfg(feature = "gui")]
pub(crate) fn stats_overview_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
) -> Result<Page<overview::Overview>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        overview::overview(ctx, query)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_weeks_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
) -> Result<Page<weeks::WeeksPage>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        weeks::weeks(ctx, query)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_week_summary_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
    week: DateTime<Utc>,
) -> Result<Page<weeks::WeekSummary>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        weeks::week_summary(ctx, query, week)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_window_detail_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
    account_id: &str,
    end: DateTime<Utc>,
) -> Result<Page<weeks::WindowDetail>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        weeks::window_detail(ctx, account_id, end)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_projects_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
) -> Result<Page<stats_projects::ProjectsPage>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        stats_projects::projects_page(ctx, query)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_project_detail_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
    key: &ProjectKey,
) -> Result<Page<stats_projects::ProjectDetail>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        stats_projects::project_detail(ctx, query, key)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_subscriptions_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
) -> Result<Page<subscriptions::SubscriptionsPage>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        subscriptions::subscriptions(ctx, query)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_what_if_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
    without: &str,
) -> Result<Page<subscriptions::WhatIf>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        subscriptions::what_if(ctx, query, without)
    })
}

/// The forecast has no empty marker: it either has a plan or the error that no
/// reading of every account exists yet. A reading older than the burn runtime's
/// freshness is the same as no reading; the age is the oldest reading the
/// situation kept, not the tick time its plan was built for.
#[cfg(feature = "gui")]
pub(crate) fn stats_forecast_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    kept: Option<KeptForecast>,
) -> Result<Page<forecast::Forecast>, String> {
    let Some((inputs, regime, oldest_reading)) = kept else {
        return Err("the forecast needs a reading of every account first".to_string());
    };
    if now.signed_duration_since(oldest_reading) > Duration::minutes(seatswap_core::burn::FRESH_MIN)
    {
        return Err("the forecast needs a reading of every account first".to_string());
    }
    let ctx = parts.ctx(history, now, zone);
    forecast::forecast(&ctx, &inputs, &regime)
        .map(Page::Data)
        .map_err(|error| error.to_string())
}

#[cfg(feature = "gui")]
pub(crate) fn stats_rhythm_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
) -> Result<Page<activity::RhythmPage>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        activity::rhythm(ctx, query)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_activity_at<Tz: TimeZone + Copy>(
    parts: &StatsParts,
    history: &HistoryDir,
    now: DateTime<Utc>,
    zone: Tz,
    query: &Query,
) -> Result<Page<activity::ActivityPage>, String> {
    stats_page(parts, history, now, zone, query, |ctx| {
        activity::activity(ctx, query)
    })
}

#[cfg(feature = "gui")]
pub(crate) fn stats_hosts_at(history: &HistoryDir) -> Result<Vec<String>, String> {
    history.hosts().map_err(|error| error.to_string())
}

/// The one body of a statistics command: on the blocking thread it reads the
/// files the page needs, then runs the page's own function with the moment and
/// the local zone it uses there.
#[cfg(feature = "gui")]
async fn stats_command<T, F>(state: State<'_, super::GuiState>, run: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&StatsParts, &HistoryDir, DateTime<Utc>, Local) -> Result<T, String> + Send + 'static,
{
    let app = state.app.clone();
    let history = state
        .history
        .as_ref()
        .map_err(|error| error.clone())?
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        let parts = stats_ctx_parts(&app, &history)?;
        run(&parts, &history, Utc::now(), Local)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_overview(
    state: State<'_, super::GuiState>,
    query: Query,
) -> Result<Page<overview::Overview>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_overview_at(parts, history, now, offset, &query)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_weeks(
    state: State<'_, super::GuiState>,
    query: Query,
) -> Result<Page<weeks::WeeksPage>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_weeks_at(parts, history, now, offset, &query)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_week_summary(
    state: State<'_, super::GuiState>,
    query: Query,
    week: DateTime<Utc>,
) -> Result<Page<weeks::WeekSummary>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_week_summary_at(parts, history, now, offset, &query, week)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_window_detail(
    state: State<'_, super::GuiState>,
    query: Query,
    account_id: String,
    end: DateTime<Utc>,
) -> Result<Page<weeks::WindowDetail>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_window_detail_at(parts, history, now, offset, &query, &account_id, end)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_projects(
    state: State<'_, super::GuiState>,
    query: Query,
) -> Result<Page<stats_projects::ProjectsPage>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_projects_at(parts, history, now, offset, &query)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_project_detail(
    state: State<'_, super::GuiState>,
    query: Query,
    key: ProjectKey,
) -> Result<Page<stats_projects::ProjectDetail>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_project_detail_at(parts, history, now, offset, &query, &key)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_subscriptions(
    state: State<'_, super::GuiState>,
    query: Query,
) -> Result<Page<subscriptions::SubscriptionsPage>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_subscriptions_at(parts, history, now, offset, &query)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_what_if(
    state: State<'_, super::GuiState>,
    query: Query,
    without: String,
) -> Result<Page<subscriptions::WhatIf>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_what_if_at(parts, history, now, offset, &query, &without)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_forecast(
    state: State<'_, super::GuiState>,
) -> Result<Page<forecast::Forecast>, String> {
    let inputs = super::lock(&state.forecast).clone();
    stats_command(state, move |parts, history, now, offset| {
        stats_forecast_at(parts, history, now, offset, inputs)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_rhythm(
    state: State<'_, super::GuiState>,
    query: Query,
) -> Result<Page<activity::RhythmPage>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_rhythm_at(parts, history, now, offset, &query)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_activity(
    state: State<'_, super::GuiState>,
    query: Query,
) -> Result<Page<activity::ActivityPage>, String> {
    stats_command(state, move |parts, history, now, offset| {
        stats_activity_at(parts, history, now, offset, &query)
    })
    .await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn stats_hosts(state: State<'_, super::GuiState>) -> Result<Vec<String>, String> {
    // The host list needs only the history, not the parts a page reads: a
    // broken prices file must not hide the hosts.
    let history = state
        .history
        .as_ref()
        .map_err(|error| error.clone())?
        .clone();
    tauri::async_runtime::spawn_blocking(move || stats_hosts_at(&history))
        .await
        .map_err(|error| error.to_string())?
}

/// The preview's source: the data the page already has, never a file read of
/// its own.
#[cfg(feature = "gui")]
struct SnapshotFacts<'a> {
    snapshot: &'a super::worker::Snapshot,
    app: &'a AppPaths,
}

#[cfg(feature = "gui")]
impl FactsSource for SnapshotFacts<'_> {
    fn host_name(&self) -> Option<&str> {
        self.snapshot.settings.as_ref()?.host_name.as_deref()
    }

    fn account(&self, account_id: &str) -> Option<AccountFacts> {
        self.snapshot
            .views
            .iter()
            .find(|view| view.id == account_id)
            .map(|view| AccountFacts {
                abbr: view.abbr.clone(),
                color: view.color.clone(),
                plan: view.plan.clone(),
            })
    }

    fn usage(&self, account_id: &str, _feed: Option<&Feed>) -> Option<Usage> {
        // The snapshot's numbers are the engine's merged view already.
        self.snapshot
            .views
            .iter()
            .find(|view| view.id == account_id)?
            .usage
            .clone()
    }

    fn model_limits(&self, account_id: &str) -> Vec<(String, f64)> {
        self.snapshot
            .views
            .iter()
            .find(|view| view.id == account_id)
            .and_then(|view| view.usage.as_ref())
            .map(|usage| {
                usage
                    .weekly_scoped()
                    .filter_map(|limit| {
                        limit
                            .model
                            .as_ref()
                            .map(|model| (model.clone(), limit.percent))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn burn_next(&self, now: DateTime<Utc>) -> Result<Option<(String, DateTime<Utc>)>, String> {
        // The same file the command reads: the snapshot's burn plan says what
        // auto-burn would do, whether or not it is on, while the line shows
        // only what the tray app wrote.
        statusline::burn_next_account(self.app, now, |account_id| {
            self.snapshot
                .views
                .iter()
                .find(|view| view.id == account_id)
                .map(|view| view.abbr.clone())
        })
    }
}

/// The snapshot's live login in the shape [`statusline::facts_from_source`]
/// takes.
#[cfg(feature = "gui")]
fn live_state_of(live: &super::worker::LiveView) -> LiveState {
    match live {
        super::worker::LiveView::NoLogin => LiveState::NoLogin,
        super::worker::LiveView::Ended { account_id } => LiveState::Ended {
            account_id: account_id.clone(),
        },
        super::worker::LiveView::Known { account_id } => LiveState::Known {
            account_id: account_id.clone(),
        },
        super::worker::LiveView::Unknown { email, org_name } => LiveState::Unknown {
            email: email.clone(),
            org_name: org_name.clone(),
        },
    }
}

/// Render the builder's preview once per width: the given config, never the
/// stored one, and README §5's payload for every scenario.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) fn statusline_preview(
    state: State<'_, super::GuiState>,
    config: StatuslineConfig,
    widths: Vec<u16>,
    scenario: Scenario,
) -> Result<Vec<PreviewLine>, String> {
    if widths.is_empty() {
        return Err("the preview needs at least one width".to_string());
    }
    if widths.contains(&0) {
        return Err("a preview width must be at least 1 column; 0 cannot show a line".to_string());
    }
    let snapshot = state.snapshot()?;
    if snapshot.settings.is_none() {
        return Err("accounts.json cannot be read; the preview has no data".to_string());
    }
    let payload = statusline::parse(statusline::README_PAYLOAD.as_bytes());
    let now = Utc::now();
    let facts = statusline::facts_from_source(
        &SnapshotFacts {
            snapshot: &snapshot,
            app: &state.app,
        },
        &payload,
        &live_state_of(&snapshot.live),
        state.claude.home(),
        crate::cli::user_name()?,
        now,
    )?;
    statusline::preview_lines(&config, &payload, &facts, &widths, scenario, &Local)
}

/// The builder's three presets, in the page's order.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) fn statusline_presets() -> Vec<(String, StatuslineConfig)> {
    StatuslineConfig::presets()
}

/// Start a recording for `minutes` by changing the one setting through the
/// settings page's save command, the one writer of `accounts.json`.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) async fn statusline_record(
    state: State<'_, super::GuiState>,
    minutes: u32,
) -> Result<(), String> {
    let until = statusline::record_until(minutes, Utc::now())?;
    let value =
        serde_json::to_value(until).map_err(|error| format!("statuslineRecordUntil: {error}"))?;
    super::panel::save_setting(state, "statuslineRecordUntil".to_string(), value)
        .await
        .map(|_saved| ())
}

/// The field names recorded so far, or `None` when no recording has written
/// the keys file.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) fn statusline_keys(
    state: State<'_, super::GuiState>,
) -> Result<Option<RecordedKeys>, String> {
    statusline::read_recorded_keys(&state.app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_an_area_with_an_optional_sub() {
        assert_eq!(
            parse_target("settings/accounts"),
            Ok(("settings", Some("accounts")))
        );
        assert_eq!(
            parse_target("settings/accounts?8564d960fb9a498daede9211fc09c9f8"),
            Ok((
                "settings",
                Some("accounts?8564d960fb9a498daede9211fc09c9f8")
            ))
        );
        assert!(parse_target("settings/accounts?").is_err());
        assert!(parse_target("settings/?x").is_err());
        assert!(parse_target("settings/accounts?a b").is_err());
        assert!(parse_target("nonsense").is_err());
        assert_eq!(parse_target("settings"), Ok(("settings", None)));
        assert_eq!(
            parse_target("sessions?7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34"),
            Ok(("sessions", Some("7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34")))
        );
        assert!(parse_target("sessions?").is_err());
        assert!(parse_target("sessions?7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34/x").is_err());
        assert!(parse_target("settings/").is_err());
        assert!(parse_target("settings/a/b").is_err());
        assert!(parse_target("settings/a b").is_err());
        assert!(parse_target("settings/a\nb").is_err());
        assert!(parse_target("settings/Übersicht").is_err());
    }

    #[test]
    fn review_an_item_after_another_area_is_not_a_sub_area() {
        assert!(
            parse_target("settings?accounts").is_err(),
            "\"settings?accounts\" is read as the sub-area accounts"
        );
        assert!(parse_target("statistics?x").is_err());
    }

    // ---- the Sessions view (spec §3.8) ----

    #[cfg(feature = "gui")]
    use seatswap_core::history::HistoryDir;
    #[cfg(feature = "gui")]
    use seatswap_core::live_sessions::{other_hosts, write_report, Values};
    #[cfg(feature = "gui")]
    use seatswap_core::store::Settings;
    #[cfg(feature = "gui")]
    use seatswap_core::transcripts::Tokens;

    #[cfg(feature = "gui")]
    const ATLAS: &str = "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34";
    #[cfg(feature = "gui")]
    const VEGA: &str = "dd5e0c1a-3b7f-4c2d-9e8a-6f1b2c3d4e5f";
    #[cfg(feature = "gui")]
    const CLOSED_RECENT: &str = "11a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b";
    #[cfg(feature = "gui")]
    const CLOSED_OLD: &str = "2b7d9c1e-5a4f-4c3b-9e8d-7f6a5b4c3d2e";

    #[cfg(feature = "gui")]
    fn time(text: &str) -> DateTime<Utc> {
        text.parse().expect("a test time")
    }

    #[cfg(feature = "gui")]
    fn row(
        session: &str,
        host: &str,
        status: LiveStatus,
        model: Option<&str>,
        since: DateTime<Utc>,
    ) -> LiveSession {
        LiveSession {
            session: session.to_string(),
            host: host.to_string(),
            label: format!("acme-kb-{}", session.chars().take(2).collect::<String>()),
            project_id: ProjectId::of(
                &seatswap_core::project::ProjectKey::Repo("github.com/acme/acme-kb".to_string()),
                host,
            ),
            project: "acme-kb".to_string(),
            org: None,
            cwd: "/home/sam/projects/acme-kb".to_string(),
            provider: "Claude Code".to_string(),
            model: model.map(str::to_string),
            tied: model
                .map(seatswap_core::burn::tied::thinking_tied)
                .unwrap_or(false),
            status,
            since,
            started: since,
            agents_running: 0,
            agents_done: 0,
            agents: Vec::new(),
            tokens: Tokens::default(),
            last_hour: Tokens::default(),
            values: Values::of(&[], &Settings::default()),
        }
    }

    #[cfg(feature = "gui")]
    fn id(index: u32) -> String {
        format!("{index:08x}-1111-4111-8111-111111111111")
    }

    #[cfg(feature = "gui")]
    fn options(closed: ClosedSince, host: Option<&str>) -> SessionsOptions {
        SessionsOptions {
            closed,
            host: host.map(str::to_string),
        }
    }

    /// A vega report as `write_report` writes it, read back by this host.
    #[cfg(feature = "gui")]
    fn vega_report(
        root: &std::path::Path,
        now: DateTime<Utc>,
        sessions: Vec<LiveSession>,
    ) -> Vec<OtherHost> {
        std::fs::create_dir_all(root.join("vega")).expect("the vega folder");
        std::fs::write(
            root.join("vega").join("host.json"),
            "{\"machineId\":\"made-up\"}",
        )
        .expect("the host marker");
        let vega = HistoryDir::at(root, "vega");
        write_report(&vega, &sessions, now).expect("a vega report");
        let atlas = HistoryDir::at(root, "atlas");
        other_hosts(&atlas, now).expect("the other hosts").hosts
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_view_lists_this_host_and_other_hosts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let now = time("2026-11-14T10:00:00Z");
        let other = vega_report(
            tmp.path(),
            now,
            vec![row(
                VEGA,
                "vega",
                LiveStatus::Busy,
                Some("claude-opus-5-5"),
                now - Duration::minutes(2),
            )],
        );
        let view = view_of(
            now,
            Some("atlas".to_string()),
            &[row(
                ATLAS,
                "atlas",
                LiveStatus::Busy,
                Some("claude-opus-5-5"),
                now - Duration::minutes(3),
            )],
            &[],
            &other,
            &BTreeMap::new(),
            None,
            Some("EUR"),
            &SessionsOptions::default(),
        );

        assert_eq!(view.sessions.len(), 2, "{:?}", view.sessions);
        assert_eq!(view.sessions[0].session, ATLAS);
        assert_eq!(view.sessions[1].session, VEGA);
        assert_eq!(view.hosts.len(), 2);
        assert_eq!(view.hosts[0].host, "atlas");
        assert!(view.hosts[0].this);
        assert_eq!(view.hosts[1].host, "vega");
        assert!(!view.hosts[1].this);
        assert_eq!(view.this_host.as_deref(), Some("atlas"));
    }

    #[cfg(feature = "gui")]
    #[test]
    fn closed_none_lists_only_running() {
        let now = time("2026-11-14T10:00:00Z");
        let view = view_of(
            now,
            Some("atlas".to_string()),
            &[row(
                ATLAS,
                "atlas",
                LiveStatus::Busy,
                Some("claude-opus-5-5"),
                now - Duration::minutes(3),
            )],
            &[row(
                CLOSED_RECENT,
                "atlas",
                LiveStatus::Closed,
                Some("claude-opus-5-5"),
                now - Duration::hours(3),
            )],
            &[],
            &BTreeMap::new(),
            None,
            None,
            &options(ClosedSince::None, None),
        );

        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.sessions[0].status, LiveStatus::Busy);
    }

    #[cfg(feature = "gui")]
    #[test]
    fn closed_day_adds_the_closed_ones() {
        let now = time("2026-11-14T10:00:00Z");
        let recent = row(
            CLOSED_RECENT,
            "atlas",
            LiveStatus::Closed,
            Some("claude-opus-5-5"),
            now - Duration::hours(3),
        );
        let old = row(
            CLOSED_OLD,
            "atlas",
            LiveStatus::Closed,
            Some("claude-opus-5-5"),
            now - Duration::hours(30),
        );
        let day = view_of(
            now,
            Some("atlas".to_string()),
            &[],
            &[recent.clone(), old.clone()],
            &[],
            &BTreeMap::new(),
            None,
            None,
            &options(ClosedSince::Day, None),
        );
        assert!(day.sessions.iter().any(|row| row.session == CLOSED_RECENT));
        assert!(
            !day.sessions.iter().any(|row| row.session == CLOSED_OLD),
            "a session closed 30 h ago is not in the day"
        );
        assert_eq!(day.sessions[0].status, LiveStatus::Closed);

        let week = view_of(
            now,
            Some("atlas".to_string()),
            &[],
            &[recent, old],
            &[],
            &BTreeMap::new(),
            None,
            None,
            &options(ClosedSince::Week, None),
        );
        assert_eq!(week.sessions.len(), 2);
        assert_eq!(week.sessions[0].session, CLOSED_RECENT, "newest first");
        assert_eq!(week.sessions[1].session, CLOSED_OLD);
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_session_running_on_another_host_is_not_listed_closed_too() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let now = time("2026-11-14T10:00:00Z");
        let mut running = row(
            VEGA,
            "vega",
            LiveStatus::Busy,
            Some("claude-opus-5-5"),
            now - Duration::minutes(2),
        );
        running.tokens.out = 1_000_000;
        let other = vega_report(tmp.path(), now, vec![running]);
        let mut closed = row(
            VEGA,
            "vega",
            LiveStatus::Closed,
            Some("claude-opus-5-5"),
            now - Duration::hours(3),
        );
        closed.tokens.out = 9_000_000;

        let view = view_of(
            now,
            Some("atlas".to_string()),
            &[],
            &[closed],
            &other,
            &BTreeMap::new(),
            None,
            None,
            &options(ClosedSince::Day, None),
        );

        assert_eq!(view.sessions.len(), 1, "{:?}", view.sessions);
        assert_eq!(view.sessions[0].session, VEGA);
        assert_eq!(view.sessions[0].status, LiveStatus::Busy);
        let all = view
            .sums
            .iter()
            .find(|sum| sum.of == SumOf::All)
            .expect("the total");
        assert_eq!(all.sessions, 1);
        assert_eq!(
            all.tokens.out, 1_000_000,
            "the closed row's tokens are not counted a second time"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_host_filter_keeps_only_that_host() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let now = time("2026-11-14T10:00:00Z");
        let other = vega_report(
            tmp.path(),
            now,
            vec![row(
                VEGA,
                "vega",
                LiveStatus::Busy,
                Some("claude-opus-5-5"),
                now - Duration::minutes(2),
            )],
        );
        let running = [row(
            ATLAS,
            "atlas",
            LiveStatus::Busy,
            Some("claude-opus-5-5"),
            now - Duration::minutes(3),
        )];
        let vega = view_of(
            now,
            Some("atlas".to_string()),
            &running,
            &[],
            &other,
            &BTreeMap::new(),
            None,
            None,
            &options(ClosedSince::None, Some("vega")),
        );
        assert_eq!(vega.sessions.len(), 1);
        assert_eq!(vega.sessions[0].host, "vega");
        assert_eq!(vega.hosts.len(), 2, "the list of hosts stays complete");

        let unknown = view_of(
            now,
            Some("atlas".to_string()),
            &running,
            &[],
            &other,
            &BTreeMap::new(),
            None,
            None,
            &options(ClosedSince::None, Some("nope")),
        );
        assert!(unknown.sessions.is_empty());
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_switch_effect_counts_each_kind() {
        let now = time("2026-11-14T10:00:00Z");
        let mut running = Vec::new();
        let mut thinking = BTreeMap::new();
        // 3 losing: one busy, two idle but only recently.
        running.push(row(
            &id(0),
            "atlas",
            LiveStatus::Busy,
            Some("claude-fable-5-2"),
            now,
        ));
        for index in 1..3 {
            running.push(row(
                &id(index),
                "atlas",
                LiveStatus::Idle,
                Some("claude-fable-5-2"),
                now - Duration::minutes(10),
            ));
        }
        for index in 0..3 {
            thinking.insert(id(index), 1);
        }
        // 2 resend: idle on a model whose thinking survives, no thinking held.
        for index in 3..5 {
            running.push(row(
                &id(index),
                "atlas",
                LiveStatus::Idle,
                Some("claude-opus-5-5"),
                now - Duration::minutes(10),
            ));
        }
        // 4 free: idle for longer than the stored copy lives.
        for index in 5..9 {
            running.push(row(
                &id(index),
                "atlas",
                LiveStatus::Idle,
                Some("claude-opus-5-5"),
                now - Duration::minutes(61),
            ));
        }
        let view = view_of(
            now,
            Some("atlas".to_string()),
            &running,
            &[],
            &[],
            &thinking,
            None,
            None,
            &SessionsOptions::default(),
        );
        assert_eq!(
            view.switch_effect,
            SwitchEffect {
                loses_thinking: 3,
                loses_thinking_busy: 1,
                loses_thinking_models: vec!["claude-fable-5-2".to_string()],
                resend: 2,
                free: 4,
            }
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_switch_effect_names_the_models_that_lose_thinking() {
        let now = time("2026-11-14T10:00:00Z");
        let running = vec![
            row(
                &id(0),
                "atlas",
                LiveStatus::Busy,
                Some("claude-fable-5-2"),
                now,
            ),
            row(
                &id(1),
                "atlas",
                LiveStatus::Idle,
                Some("claude-sonnet-5-5"),
                now - Duration::minutes(10),
            ),
            row(
                &id(2),
                "atlas",
                LiveStatus::Idle,
                Some("claude-fable-5-2"),
                now - Duration::minutes(10),
            ),
        ];
        let thinking: BTreeMap<String, u32> =
            running.iter().map(|row| (row.session.clone(), 1)).collect();
        let view = view_of(
            now,
            Some("atlas".to_string()),
            &running,
            &[],
            &[],
            &thinking,
            None,
            None,
            &SessionsOptions::default(),
        );

        assert_eq!(view.switch_effect.loses_thinking, 3);
        assert_eq!(
            view.switch_effect.loses_thinking_models,
            vec![
                "claude-fable-5-2".to_string(),
                "claude-sonnet-5-5".to_string()
            ],
            "each model once, in the order its first session is listed"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_switch_effect_counts_only_this_host() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let now = time("2026-11-14T10:00:00Z");
        let other = vega_report(
            tmp.path(),
            now,
            vec![row(
                VEGA,
                "vega",
                LiveStatus::Busy,
                Some("claude-fable-5-2"),
                now,
            )],
        );
        let mut thinking = BTreeMap::new();
        thinking.insert(ATLAS.to_string(), 1);
        thinking.insert(VEGA.to_string(), 1);
        let view = view_of(
            now,
            Some("atlas".to_string()),
            &[row(
                ATLAS,
                "atlas",
                LiveStatus::Busy,
                Some("claude-fable-5-2"),
                now,
            )],
            &[row(
                CLOSED_RECENT,
                "atlas",
                LiveStatus::Closed,
                Some("claude-fable-5-2"),
                now - Duration::hours(1),
            )],
            &other,
            &thinking,
            None,
            None,
            &options(ClosedSince::Day, None),
        );
        assert_eq!(
            view.switch_effect,
            SwitchEffect {
                loses_thinking: 1,
                loses_thinking_busy: 1,
                loses_thinking_models: vec!["claude-fable-5-2".to_string()],
                resend: 0,
                free: 0,
            },
            "another host's row and a closed row do not change this host's switch cost"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_view_without_a_history_lists_this_hosts_sessions() {
        let now = time("2026-11-14T10:00:00Z");
        let view = view_of(
            now,
            None,
            &[row(
                ATLAS,
                "",
                LiveStatus::Busy,
                Some("claude-opus-5-5"),
                now,
            )],
            &[],
            &[],
            &BTreeMap::new(),
            Some("the history cannot be read"),
            None,
            &SessionsOptions::default(),
        );
        assert_eq!(view.this_host, None);
        assert!(view.hosts.is_empty());
        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.sessions[0].host, "");
        assert_eq!(
            view.unavailable.as_deref(),
            Some("the history cannot be read")
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_sums_cover_every_group() {
        fn find<'a>(view: &'a SessionsView, of: &SumOf) -> &'a Sum {
            view.sums
                .iter()
                .find(|sum| &sum.of == of)
                .unwrap_or_else(|| panic!("no sum for {of:?}"))
        }
        use seatswap_core::project::ProjectKey;

        let now = time("2026-11-14T10:00:00Z");
        let first = ProjectId::of(
            &ProjectKey::Repo("github.com/acme/acme-kb".to_string()),
            "atlas",
        );
        let second = ProjectId::of(
            &ProjectKey::Repo("github.com/sam/acme-kb".to_string()),
            "atlas",
        );
        let mut a = row(
            ATLAS,
            "atlas",
            LiveStatus::Busy,
            Some("claude-opus-5-5"),
            now,
        );
        a.project_id = first.clone();
        a.tokens.out = 1_000_000;
        let mut b = row(
            "aaaa1111-1111-4111-8111-111111111111",
            "atlas",
            LiveStatus::Idle,
            Some("claude-opus-5-5"),
            now,
        );
        b.project_id = second.clone();
        b.tokens.out = 2_000_000;
        let mut c = row(
            "bbbb1111-1111-4111-8111-111111111111",
            "atlas",
            LiveStatus::Idle,
            Some("claude-fable-5-2"),
            now,
        );
        c.project_id = first.clone();
        c.tokens.out = 4_000_000;
        let mut d = row(
            "cccc1111-1111-4111-8111-111111111111",
            "atlas",
            LiveStatus::Idle,
            None,
            now,
        );
        d.project_id = first.clone();
        d.provider = "Codex".to_string();
        d.tokens.out = 8_000_000;
        let view = view_of(
            now,
            Some("atlas".to_string()),
            &[a, b, c, d],
            &[],
            &[],
            &BTreeMap::new(),
            None,
            Some("EUR"),
            &SessionsOptions::default(),
        );

        assert_eq!(view.sums.len(), 12, "{:?}", view.sums);
        assert_eq!(find(&view, &SumOf::All).sessions, 4);
        assert_eq!(find(&view, &SumOf::All).tokens.out, 15_000_000);
        assert_eq!(
            find(&view, &SumOf::Provider("Claude Code".to_string())).sessions,
            3
        );
        assert_eq!(
            find(&view, &SumOf::Provider("Claude Code".to_string()))
                .tokens
                .out,
            7_000_000
        );
        assert_eq!(
            find(&view, &SumOf::Provider("Codex".to_string())).sessions,
            1
        );
        assert_eq!(find(&view, &SumOf::Project(first.clone())).sessions, 3);
        assert_eq!(
            find(&view, &SumOf::Project(first.clone())).tokens.out,
            13_000_000
        );
        assert_eq!(
            find(&view, &SumOf::Project(second.clone())).sessions,
            1,
            "two projects with the same name are two groups"
        );
        assert_eq!(
            find(
                &view,
                &SumOf::ProjectModel(first.clone(), Some("claude-opus-5-5".to_string()))
            )
            .tokens
            .out,
            1_000_000
        );
        assert_eq!(
            find(
                &view,
                &SumOf::ProjectModel(first.clone(), Some("claude-fable-5-2".to_string()))
            )
            .tokens
            .out,
            4_000_000
        );
        assert_eq!(
            find(&view, &SumOf::ProjectModel(first.clone(), None))
                .tokens
                .out,
            8_000_000
        );
        assert_eq!(
            find(
                &view,
                &SumOf::ProjectModel(second.clone(), Some("claude-opus-5-5".to_string()))
            )
            .tokens
            .out,
            2_000_000
        );
        assert_eq!(
            find(&view, &SumOf::Model(Some("claude-opus-5-5".to_string())))
                .tokens
                .out,
            3_000_000
        );
        assert_eq!(
            find(&view, &SumOf::Model(Some("claude-fable-5-2".to_string())))
                .tokens
                .out,
            4_000_000
        );
        assert_eq!(find(&view, &SumOf::Model(None)).tokens.out, 8_000_000);
    }

    #[cfg(feature = "gui")]
    #[test]
    /// A group's sum carries the values and the last hour of exactly the rows
    /// it covers: the closed row is in the sum of the day, and another host's
    /// row in another currency is not added.
    fn the_sums_carry_the_values_of_their_rows() {
        use seatswap_core::project::ProjectKey;

        fn find<'a>(view: &'a SessionsView, of: &SumOf) -> &'a Sum {
            view.sums
                .iter()
                .find(|sum| &sum.of == of)
                .unwrap_or_else(|| panic!("no sum for {of:?}"))
        }

        fn valued(subscription: f64, api: f64, currency: &str) -> Values {
            Values {
                api: Some(api),
                api_currency: Some(currency.to_string()),
                api_missing: None,
                subscription: Some(subscription),
                currency: Some(currency.to_string()),
                subscription_missing: None,
            }
        }

        let now = time("2026-11-14T10:00:00Z");
        let acme = ProjectId::of(
            &ProjectKey::Repo("github.com/acme/acme-kb".to_string()),
            "atlas",
        );
        let ledger = ProjectId::of(
            &ProjectKey::Repo("github.com/sam/ledger".to_string()),
            "atlas",
        );
        let mut a = row(
            ATLAS,
            "atlas",
            LiveStatus::Busy,
            Some("claude-opus-5-5"),
            now,
        );
        a.project_id = acme.clone();
        a.tokens.out = 1_000_000;
        a.last_hour.out = 100_000;
        a.values = valued(2.0, 20.0, "EUR");
        let mut b = row(
            "aaaa1111-1111-4111-8111-111111111111",
            "vega",
            LiveStatus::Busy,
            Some("claude-opus-5-5"),
            now,
        );
        b.project_id = acme.clone();
        b.tokens.out = 2_000_000;
        b.last_hour.out = 200_000;
        b.values = valued(4.0, 40.0, "USD");
        let mut c = row(
            "bbbb1111-1111-4111-8111-111111111111",
            "atlas",
            LiveStatus::Idle,
            Some("claude-fable-5-2"),
            now,
        );
        c.project_id = ledger.clone();
        c.tokens.out = 3_000_000;
        c.last_hour.out = 300_000;
        c.values = valued(6.0, 60.0, "EUR");
        let mut done = row(
            CLOSED_RECENT,
            "atlas",
            LiveStatus::Closed,
            Some("claude-fable-5-2"),
            now - Duration::hours(3),
        );
        done.project_id = ledger.clone();
        done.tokens.out = 500_000;
        done.last_hour.out = 50_000;
        done.values = valued(1.0, 10.0, "EUR");

        let view = view_of(
            now,
            Some("atlas".to_string()),
            &[a, b, c],
            &[done],
            &[],
            &BTreeMap::new(),
            None,
            Some("EUR"),
            &options(ClosedSince::Day, None),
        );

        // The ledger group holds this host's row and the closed one.
        let ledger_sum = find(&view, &SumOf::Project(ledger.clone()));
        assert_eq!(ledger_sum.sessions, 2, "the closed row is in the day");
        assert_eq!(ledger_sum.tokens.out, 3_500_000);
        assert_eq!(ledger_sum.last_hour.out, 350_000);
        assert_eq!(ledger_sum.values.subscription, Some(7.0));
        assert_eq!(ledger_sum.values.api, Some(70.0));
        assert_eq!(ledger_sum.values.currency.as_deref(), Some("EUR"));

        // The group of the other host's row holds two currencies: nothing is added.
        let acme_sum = find(&view, &SumOf::Project(acme.clone()));
        assert_eq!(acme_sum.sessions, 2);
        assert_eq!(acme_sum.tokens.out, 3_000_000);
        assert_eq!(acme_sum.last_hour.out, 300_000);
        assert_eq!(acme_sum.values.subscription, None);
        assert_eq!(acme_sum.values.currency, None);
        assert_eq!(
            acme_sum.values.subscription_missing.as_deref(),
            Some("these sessions are valued in two currencies")
        );
        assert_eq!(acme_sum.values.api, None);
        assert_eq!(
            acme_sum.values.api_missing.as_deref(),
            Some(
                "the dollar rate is missing for some of these days, so the values are in two currencies"
            )
        );

        // The total of the day counts every listed row.
        let all = find(&view, &SumOf::All);
        assert_eq!(all.sessions, 4);
        assert_eq!(all.tokens.out, 6_500_000);
        assert_eq!(all.last_hour.out, 650_000);
    }

    #[cfg(feature = "gui")]
    use seatswap_core::burn::decide::RegimeWhy;
    #[cfg(feature = "gui")]
    use seatswap_core::burn::plan::{AccountState, SessionState};
    #[cfg(feature = "gui")]
    use seatswap_core::burn::{Rates, Rhythm};
    #[cfg(feature = "gui")]
    use seatswap_core::stats::fixture;
    #[cfg(feature = "gui")]
    use seatswap_core::stats::RangeKey;

    #[cfg(feature = "gui")]
    #[test]
    fn an_empty_range_serialises_as_empty() {
        let page = Page::<()>::Empty(Empty {
            recording_since: None,
        });
        assert_eq!(
            serde_json::to_string(&page).expect("the page as JSON"),
            r#"{"empty":{"recordingSince":null}}"#
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn each_command_answers_for_the_tiny_history() {
        let dir = tempfile::tempdir().expect("a temp folder");
        let tiny = fixture::write(dir.path()).expect("the tiny fixture");
        let app = AppPaths::at(dir.path());
        let parts = stats_ctx_parts(&app, &tiny.history).expect("the statistics parts");
        let query = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: None,
        };
        let now = tiny.now;
        let offset = tiny.offset;

        assert_eq!(
            stats_hosts_at(&tiny.history).expect("the hosts"),
            vec!["atlas".to_string()]
        );

        let overview =
            stats_overview_at(&parts, &tiny.history, now, offset, &query).expect("the overview");
        match overview {
            Page::Data(overview) => {
                assert!(
                    (overview.kpis.used - 120.0).abs() < 0.001,
                    "used {}",
                    overview.kpis.used
                );
            }
            Page::Empty(empty) => panic!("the tiny history is not empty: {empty:?}"),
        }

        assert!(stats_weeks_at(&parts, &tiny.history, now, offset, &query).is_ok());
        assert!(stats_week_summary_at(
            &parts,
            &tiny.history,
            now,
            offset,
            &query,
            "2026-11-09T00:00:00Z".parse().expect("a week")
        )
        .is_ok());
        assert!(stats_window_detail_at(
            &parts,
            &tiny.history,
            now,
            offset,
            &query,
            &tiny.accounts[0].id,
            "2026-11-09T17:00:00Z".parse().expect("a window end")
        )
        .is_ok());
        assert!(stats_projects_at(&parts, &tiny.history, now, offset, &query).is_ok());
        assert!(stats_project_detail_at(
            &parts,
            &tiny.history,
            now,
            offset,
            &query,
            &ProjectKey::Repo("github.com/sam/atlas".to_string())
        )
        .is_ok());
        assert!(stats_subscriptions_at(&parts, &tiny.history, now, offset, &query).is_ok());
        assert!(stats_what_if_at(
            &parts,
            &tiny.history,
            now,
            offset,
            &query,
            &tiny.accounts[1].id
        )
        .is_ok());
        assert!(stats_rhythm_at(&parts, &tiny.history, now, offset, &query).is_ok());
        assert!(stats_activity_at(&parts, &tiny.history, now, offset, &query).is_ok());

        let inputs = Inputs {
            now,
            offset,
            accounts: vec![
                AccountState {
                    id: tiny.accounts[0].id.clone(),
                    may_use: true,
                    login_ok: true,
                    week_percent: 30.0,
                    week_resets_at: "2026-11-23T17:00:00Z".parse().expect("a reset"),
                    session: Some(SessionState {
                        percent: 0.0,
                        resets_at: None,
                    }),
                    model_blocked: false,
                    rates: Some(Rates {
                        c: 4.0,
                        s: Some(80.0),
                    }),
                    thinking_held: 0,
                },
                AccountState {
                    id: tiny.accounts[1].id.clone(),
                    may_use: true,
                    login_ok: true,
                    week_percent: 40.0,
                    week_resets_at: "2026-11-23T02:00:00Z".parse().expect("a reset"),
                    session: Some(SessionState {
                        percent: 0.0,
                        resets_at: None,
                    }),
                    model_blocked: false,
                    rates: Some(Rates {
                        c: 4.0,
                        s: Some(100.0),
                    }),
                    thinking_held: 0,
                },
            ],
            active: Some(tiny.accounts[0].id.clone()),
            rhythm: Rhythm { wu: [1.0; 168] },
        };
        let regime = Regime {
            early: false,
            why: RegimeWhy::NoCostYet,
            lazy_blocked_h: 0.0,
            eager_blocked_h: 0.0,
            extra_switches: 0,
            cost_h: None,
        };
        let forecast = stats_forecast_at(
            &parts,
            &tiny.history,
            now,
            offset,
            Some((inputs, regime, now)),
        )
        .expect("the forecast");
        assert!(matches!(forecast, Page::Data(_)), "{forecast:?}");

        let error = stats_forecast_at(&parts, &tiny.history, now, offset, None)
            .expect_err("no reading, no forecast");
        assert_eq!(error, "the forecast needs a reading of every account first");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_ctx_names_a_broken_prices_file() {
        let dir = tempfile::tempdir().expect("a temp folder");
        let tiny = fixture::write(dir.path()).expect("the tiny fixture");
        let app = AppPaths::at(dir.path());
        std::fs::write(app.prices_file(), "{not json").expect("the broken prices");

        let error = stats_ctx_parts(&app, &tiny.history).expect_err("the prices cannot be read");
        assert!(error.starts_with("the prices cannot be read"), "{error}");
    }

    // ---- proof tests of the review ----

    /// The fixture must stay the fixture: a second `fixture::write` into the
    /// same folder is refused, because the first write is already there. The
    /// pages then count the history once.
    #[cfg(feature = "gui")]
    #[test]
    fn the_fixture_written_twice_counts_the_history_once() {
        let dir = tempfile::tempdir().expect("a temp folder");
        let tiny = fixture::write(dir.path()).expect("the tiny fixture");
        let app = AppPaths::at(dir.path());
        let parts = stats_ctx_parts(&app, &tiny.history).expect("the statistics parts");
        let query = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: None,
        };

        // The second write into the same folder must not add a second copy of
        // every record: it is refused.
        let error = fixture::write(dir.path())
            .err()
            .expect("the second fixture is refused");
        assert!(error.ends_with("is not empty"), "{error}");

        let overview = stats_overview_at(&parts, &tiny.history, tiny.now, tiny.offset, &query)
            .expect("the overview");
        let Page::Data(overview) = overview else {
            panic!("the tiny history is not empty");
        };
        assert_eq!(
            overview.kpis.switches.user, 1,
            "the switches are counted twice"
        );
    }

    /// The forecast is the plan of the reading it was built from. A reading
    /// older than the freshness the burn runtime itself uses (`FRESH_MIN`) is
    /// refused like no reading at all: its seven days would lie in the past.
    #[cfg(feature = "gui")]
    #[test]
    fn the_forecast_refuses_a_reading_that_is_too_old() {
        let dir = tempfile::tempdir().expect("a temp folder");
        let tiny = fixture::write(dir.path()).expect("the tiny fixture");
        let app = AppPaths::at(dir.path());
        let parts = stats_ctx_parts(&app, &tiny.history).expect("the statistics parts");
        let now = tiny.now;
        let offset = tiny.offset;
        let regime = Regime {
            early: false,
            why: RegimeWhy::NoCostYet,
            lazy_blocked_h: 0.0,
            eager_blocked_h: 0.0,
            extra_switches: 0,
            cost_h: None,
        };
        let state = |id: String, percent: f64, resets_at: &str, s: f64| AccountState {
            id,
            may_use: true,
            login_ok: true,
            week_percent: percent,
            week_resets_at: resets_at.parse().expect("a reset"),
            session: Some(SessionState {
                percent: 0.0,
                resets_at: None,
            }),
            model_blocked: false,
            rates: Some(Rates { c: 4.0, s: Some(s) }),
            thinking_held: 0,
        };
        let inputs = |at: DateTime<Utc>| Inputs {
            now: at,
            offset,
            accounts: vec![
                state(
                    tiny.accounts[0].id.clone(),
                    30.0,
                    "2026-11-23T17:00:00Z",
                    80.0,
                ),
                state(
                    tiny.accounts[1].id.clone(),
                    40.0,
                    "2026-11-23T02:00:00Z",
                    100.0,
                ),
            ],
            active: Some(tiny.accounts[0].id.clone()),
            rhythm: Rhythm { wu: [1.0; 168] },
        };

        // A reading of this moment is a plan.
        let fresh = stats_forecast_at(
            &parts,
            &tiny.history,
            now,
            offset,
            Some((inputs(now), regime.clone(), now)),
        )
        .expect("the forecast of a fresh reading");
        assert!(matches!(fresh, Page::Data(_)), "{fresh:?}");

        // A reading of half an hour ago is not: it is older than the
        // freshness the burn runtime itself uses.
        let old = now - Duration::minutes(seatswap_core::burn::FRESH_MIN + 10);
        let error = stats_forecast_at(
            &parts,
            &tiny.history,
            now,
            offset,
            Some((inputs(old), regime.clone(), old)),
        )
        .expect_err("a reading that is too old is no plan");
        assert_eq!(
            error, "the forecast needs a reading of every account first",
            "{error}"
        );
    }

    /// The plan's own tick time is not the age of its readings: a tick one
    /// minute ago can still be built on readings two hours old when the usage
    /// fetch failed and only a cached regime kept the plan alive.
    #[cfg(feature = "gui")]
    #[test]
    fn the_forecast_refuses_readings_that_are_too_old() {
        let dir = tempfile::tempdir().expect("a temp folder");
        let tiny = fixture::write(dir.path()).expect("the tiny fixture");
        let app = AppPaths::at(dir.path());
        let parts = stats_ctx_parts(&app, &tiny.history).expect("the statistics parts");
        let now = tiny.now;
        let offset = tiny.offset;
        let regime = Regime {
            early: false,
            why: RegimeWhy::NoCostYet,
            lazy_blocked_h: 0.0,
            eager_blocked_h: 0.0,
            extra_switches: 0,
            cost_h: None,
        };
        let inputs = Inputs {
            now: now - Duration::minutes(1),
            offset,
            accounts: vec![AccountState {
                id: tiny.accounts[0].id.clone(),
                may_use: true,
                login_ok: true,
                week_percent: 30.0,
                week_resets_at: "2026-11-23T17:00:00Z".parse().expect("a reset"),
                session: Some(SessionState {
                    percent: 0.0,
                    resets_at: None,
                }),
                model_blocked: false,
                rates: Some(Rates {
                    c: 4.0,
                    s: Some(80.0),
                }),
                thinking_held: 0,
            }],
            active: Some(tiny.accounts[0].id.clone()),
            rhythm: Rhythm { wu: [1.0; 168] },
        };

        let oldest = now - Duration::hours(2);
        let error = stats_forecast_at(
            &parts,
            &tiny.history,
            now,
            offset,
            Some((inputs, regime, oldest)),
        )
        .expect_err("readings two hours old are no plan");
        assert_eq!(
            error, "the forecast needs a reading of every account first",
            "{error}"
        );
    }

    /// A range with no record says "nothing recorded yet" on every page, not
    /// only in the core: every command that takes a query builds its range and
    /// asks `stats::empty` before it computes.
    #[cfg(feature = "gui")]
    #[test]
    fn every_page_but_the_forecast_says_nothing_recorded_for_an_empty_range() {
        fn assert_empty<T: std::fmt::Debug>(page: Page<T>) {
            match page {
                Page::Empty(_) => {}
                Page::Data(data) => panic!("the empty range has data: {data:?}"),
            }
        }

        let dir = tempfile::tempdir().expect("a temp folder");
        let tiny = fixture::write(dir.path()).expect("the tiny fixture");
        let app = AppPaths::at(dir.path());
        let parts = stats_ctx_parts(&app, &tiny.history).expect("the statistics parts");
        let now = "2026-12-10T10:00:00Z".parse().expect("a later month");
        let offset = tiny.offset;
        let query = Query {
            range: RangeKey::ThisWeek,
            accounts: Vec::new(),
            host: None,
        };

        assert_empty(
            stats_overview_at(&parts, &tiny.history, now, offset, &query)
                .expect("the overview command"),
        );
        assert_empty(
            stats_weeks_at(&parts, &tiny.history, now, offset, &query).expect("the weeks command"),
        );
        assert_empty(
            stats_week_summary_at(
                &parts,
                &tiny.history,
                now,
                offset,
                &query,
                "2026-12-07T00:00:00Z".parse().expect("this week"),
            )
            .expect("the week summary command"),
        );
        assert_empty(
            stats_window_detail_at(
                &parts,
                &tiny.history,
                now,
                offset,
                &query,
                &tiny.accounts[0].id,
                "2026-11-09T17:00:00Z".parse().expect("a window end"),
            )
            .expect("the window detail command"),
        );
        assert_empty(
            stats_projects_at(&parts, &tiny.history, now, offset, &query)
                .expect("the projects command"),
        );
        assert_empty(
            stats_project_detail_at(
                &parts,
                &tiny.history,
                now,
                offset,
                &query,
                &ProjectKey::Repo("github.com/sam/atlas".to_string()),
            )
            .expect("the project detail command"),
        );
        assert_empty(
            stats_subscriptions_at(&parts, &tiny.history, now, offset, &query)
                .expect("the subscriptions command"),
        );
        assert_empty(
            stats_what_if_at(
                &parts,
                &tiny.history,
                now,
                offset,
                &query,
                &tiny.accounts[1].id,
            )
            .expect("the what-if command"),
        );
        assert_empty(
            stats_rhythm_at(&parts, &tiny.history, now, offset, &query)
                .expect("the rhythm command"),
        );
        assert_empty(
            stats_activity_at(&parts, &tiny.history, now, offset, &query)
                .expect("the activity command"),
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_preview_takes_its_auto_burn_from_burn_next_json() {
        use seatswap_core::burn::burn_next;
        use seatswap_core::engine::AccountView;
        use seatswap_core::store::LoginState;

        use super::super::burn_rt::BurnView;
        use super::super::worker::Snapshot;

        const AC_ID: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
        let now = DateTime::<Utc>::from_timestamp(1_794_473_100, 0).expect("a test time");
        let switch_at = now + Duration::hours(2);
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = AppPaths::at(&tmp.path().join("data"));

        let mut snapshot = Snapshot::empty(None, None, None);
        snapshot.views.push(AccountView {
            id: AC_ID.to_string(),
            label: "AC team".to_string(),
            abbr: "AC".to_string(),
            color: "#3b82f6".to_string(),
            email: None,
            plan: Some("Team".to_string()),
            auto_burn: true,
            is_active: false,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            login_days_left: None,
            usage: None,
            usable: true,
            blocked_until: None,
            blocker: None,
            fetched_at: None,
            last_error: None,
        });
        let mut burn = BurnView::default();
        burn.planned.insert(AC_ID.to_string(), switch_at);
        snapshot.burn = Some(burn);

        let source = SnapshotFacts {
            snapshot: &snapshot,
            app: &app,
        };
        assert_eq!(
            source.burn_next(now),
            Ok(None),
            "no burn-next.json, so no auto-burn value even with a filled plan"
        );

        burn_next::write(
            &app,
            &burn_next::BurnNext {
                account: AC_ID.to_string(),
                at: switch_at,
                written: now,
            },
        )
        .expect("burn-next.json");
        assert_eq!(
            source.burn_next(now),
            Ok(Some(("AC".to_string(), switch_at))),
            "the preview shows the line's next account and time"
        );
    }
}
