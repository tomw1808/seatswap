//! The large window: its label, its areas and the one command that opens it
//! (spec §3; docs/implementation.md "app: GUI", "The large window").
//!
//! The window itself is a second Tauri webview window with its own page
//! (`window.html`). The pop-up keeps `panel.rs`: both windows exist at the same
//! time, and a `snapshot` event reaches each of them.

#[cfg(feature = "gui")]
use std::collections::BTreeMap;

#[cfg(feature = "gui")]
use chrono::{DateTime, Duration, Utc};
#[cfg(feature = "gui")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "gui")]
use seatswap_core::live_sessions::{
    switch_kind, LiveSession, LiveStatus, OtherHost, SwitchKind, Values,
};
#[cfg(feature = "gui")]
use seatswap_core::projects::ProjectId;
#[cfg(feature = "gui")]
use seatswap_core::transcripts::Tokens;

#[cfg(feature = "gui")]
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

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
    let snapshot = state.snapshot()?;
    let mut view = tauri::async_runtime::spawn_blocking(move || client.view(options))
        .await
        .map_err(|error| error.to_string())??;
    super::fill_in_use(&mut view, &snapshot);
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwitchEffect {
    pub loses_thinking: u32,
    pub loses_thinking_busy: u32,
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
    /// The id of the account in use; filled in by `gui::fill_in_use`.
    pub in_use: Option<String>,
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
            sessions.extend(closed_rows.into_iter().cloned());
        }
    }

    let mut switch_effect = SwitchEffect::default();
    for row in running {
        match switch_kind(row, thinking.get(&row.session).copied().unwrap_or(0), at) {
            SwitchKind::LosesThinking => {
                switch_effect.loses_thinking += 1;
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
        in_use: None,
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
                resend: 2,
                free: 4,
            }
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
    #[test]
    fn in_use_is_the_active_account() {
        use seatswap_core::engine::AccountView;
        use seatswap_core::store::LoginState;

        fn account_view(id: &str, is_active: bool) -> AccountView {
            AccountView {
                id: id.to_string(),
                label: id.to_string(),
                abbr: "AC".to_string(),
                color: "#3b82f6".to_string(),
                email: None,
                plan: None,
                auto_burn: true,
                is_active,
                login_state: LoginState::Ok,
                login_ends_at_ms: None,
                login_days_left: None,
                usage: None,
                usable: true,
                blocked_until: None,
                blocker: None,
                fetched_at: None,
                last_error: None,
            }
        }

        let now = time("2026-11-14T10:00:00Z");
        let mut view = view_of(
            now,
            Some("atlas".to_string()),
            &[],
            &[],
            &[],
            &BTreeMap::new(),
            None,
            None,
            &SessionsOptions::default(),
        );
        assert_eq!(view.in_use, None);
        let snapshot = super::super::worker::Snapshot {
            views: vec![account_view("id-ac", false), account_view("id-lb", true)],
            accounts: Vec::new(),
            active_id: Some("id-lb".to_string()),
            live: super::super::worker::LiveView::NoLogin,
            burn: None,
            warnings: Vec::new(),
            pending: None,
            secrets_warning: None,
            last_poll_at: None,
            error: None,
            sessions: None,
            settings: None,
            claude_config_dir: String::new(),
        };
        super::super::fill_in_use(&mut view, &snapshot);
        assert_eq!(view.in_use.as_deref(), Some("id-lb"));
    }
}
