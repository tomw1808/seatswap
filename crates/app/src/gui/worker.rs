//! The two worker threads behind the tray app (docs/implementation.md
//! "app: GUI", **Threads**).
//!
//! - the engine thread owns `Engine<Api>` and its own `Store`; it polls the
//!   usage, notices a wake-up and publishes `Snapshot`s;
//! - the action thread owns a second `Store`; switches, edits, removals and
//!   settings changes run there one at a time, so no Tauri command blocks the
//!   main thread and no core call runs on the watcher thread.
//!
//! Nothing here reads or writes Claude Code's files directly: only `switcher`,
//! `engine` and `store` do.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use serde::Serialize;

use seatswap_core::api::Api;
#[cfg(test)]
use seatswap_core::api::ApiError;
#[cfg(test)]
use seatswap_core::burn::decide::Regime;
#[cfg(test)]
use seatswap_core::burn::plan::Inputs;
#[cfg(test)]
use seatswap_core::engine::Remote;
use seatswap_core::engine::{
    AccountView, Engine, EngineError, LoginWarning, PollReason, UsageCache,
};
use seatswap_core::feed;
use seatswap_core::history::{
    AccountRef, By, Event, EventKind, HistoryDir, HistoryError, Hosts, Range, Recorder,
    SessionCounts,
};
use seatswap_core::locks::{self, LockError};
use seatswap_core::paths::{AppPaths, ClaudePaths};
use seatswap_core::store::{self, Account, AccountsFile, Settings, Store};
use seatswap_core::switcher::{self, LiveState, SwitchError, Synced, UnsavedPolicy};
#[cfg(test)]
use seatswap_core::types::Login;

use super::autostart;
use super::burn_rt::{BurnRuntime, BurnTick, KeptForecast};
use super::lookup_rt::{self, LookupHandle, MoneyView};
use super::notify;
use super::providers_rt::{self, ProviderView, ProvidersClient, ProvidersHandle};
use super::sessions_rt::{self, SessionsHandle};
use super::ReportedErrors;
use crate::format::{expand_tilde, with_tilde};
use crate::ops;

/// How often the engine thread polls even without a command.
const TICK: Duration = Duration::from_secs(30);

/// The next timer poll as a fixed deadline. A command may arrive first, but it
/// never moves the deadline: the timer poll runs as soon as it has passed.
struct TickDeadline {
    next: Instant,
}

impl TickDeadline {
    fn new(now: Instant) -> TickDeadline {
        TickDeadline { next: now + TICK }
    }

    /// How long to wait for a command before the timer poll is due.
    fn wait(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }

    fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// Move past `now`: the next deadline is the first one that has not passed.
    fn advance(&mut self, now: Instant) {
        self.next += TICK;
        while self.next <= now {
            self.next += TICK;
        }
    }
}

/// More wall-clock time than this beyond the monotonic clock between two ticks
/// means the machine slept.
const WAKE_THRESHOLD: Duration = Duration::from_secs(90);

/// What the panel sees: the whole GUI state, serde camelCase (docs/implementation.md).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub views: Vec<AccountView>,
    /// The stored accounts behind the views: the Accounts group needs fields the
    /// card does not (`price`, `orgName`) and never a usage payload.
    pub accounts: Vec<AccountRow>,
    pub active_id: Option<String>,
    pub live: LiveView,
    pub burn: Option<super::burn_rt::BurnView>,
    pub warnings: Vec<LoginWarning>,
    pub pending: Option<PendingSwitch>,
    pub secrets_warning: Option<String>,
    pub last_poll_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    /// `None` when `readSessions` is off; the sessions thread fills it in on the
    /// main thread.
    pub sessions: Option<SessionsSummary>,
    /// The switched-on provider cards; the provider thread fills them in on the
    /// main thread.
    pub providers: Vec<ProviderView>,
    /// `None` when accounts.json cannot be read: the panel shows the error and
    /// disables Save instead of offering defaults that would overwrite it.
    pub settings: Option<Settings>,
    /// Claude Code's config folder as the app runs with it, with `~` for the
    /// home folder. The General group shows it as the empty field's placeholder.
    pub claude_config_dir: String,
}

/// One stored account as the Accounts group shows it. `loginDaysLeft` comes
/// from the matching view, so the page need not do date arithmetic.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountRow {
    pub id: String,
    pub label: String,
    pub abbr: String,
    pub color: String,
    pub email: Option<String>,
    pub org_name: Option<String>,
    pub plan: Option<String>,
    pub price: Option<f64>,
    /// The currency `price` was entered in; `None` when there is no price or it
    /// was entered before a currency was set.
    pub price_currency: Option<String>,
    pub auto_burn: bool,
    pub login_ends_at_ms: Option<i64>,
    pub login_days_left: Option<i64>,
    pub is_active: bool,
}

impl AccountRow {
    fn of(account: &Account, views: &[AccountView]) -> AccountRow {
        let view = views.iter().find(|view| view.id == account.id);
        AccountRow {
            id: account.id.clone(),
            label: account.label.clone(),
            abbr: account.abbr.clone(),
            color: account.color.clone(),
            email: account.email.clone(),
            org_name: account.org_name.clone(),
            plan: account.plan.clone(),
            price: account.price,
            price_currency: account.price_currency.clone(),
            auto_burn: account.auto_burn,
            login_ends_at_ms: account.login_ends_at_ms,
            login_days_left: view.and_then(|view| view.login_days_left),
            is_active: view.is_some_and(|view| view.is_active),
        }
    }
}

/// Claude Code's running sessions as the panel shows them (spec §1.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionsSummary {
    pub running: u32,
    pub busy: u32,
    /// Sessions whose last model ties its thinking to the account (spec §2.11).
    pub tied: u32,
    /// Of those, the sessions that are busy now.
    pub tied_busy: u32,
    pub waiting: Vec<Waiting>,
    pub unavailable: Option<String>,
    pub warning: Option<String>,
}

/// A running session that waits for the person (spec §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Waiting {
    pub session: String,
    pub label: String,
    pub project: String,
    pub since: DateTime<Utc>,
    /// The host the session runs on; `None` is this host.
    pub host: Option<String>,
}

impl Snapshot {
    pub(crate) fn empty(
        error: Option<String>,
        secrets_warning: Option<String>,
        settings: Option<Settings>,
    ) -> Snapshot {
        Snapshot {
            views: Vec::new(),
            accounts: Vec::new(),
            active_id: None,
            live: LiveView::NoLogin,
            burn: None,
            warnings: Vec::new(),
            pending: None,
            secrets_warning,
            last_poll_at: None,
            error,
            sessions: None,
            providers: Vec::new(),
            settings,
            claude_config_dir: String::new(),
        }
    }

    pub(crate) fn set_pending(&mut self, pending: PendingSwitch) {
        self.pending = Some(pending);
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending = None;
    }

    /// A pending switch survives only while its target is still in the views.
    pub(crate) fn retain_pending(&mut self, previous: Option<&PendingSwitch>) {
        self.pending = previous
            .filter(|pending| self.views.iter().any(|view| view.id == pending.target_id))
            .cloned();
    }
}

/// The live login as the panel shows it. Carries no token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub(crate) enum LiveView {
    NoLogin,
    Ended {
        account_id: Option<String>,
    },
    Known {
        account_id: String,
    },
    Unknown {
        email: Option<String>,
        org_name: Option<String>,
    },
}

impl From<LiveState> for LiveView {
    fn from(state: LiveState) -> LiveView {
        match state {
            LiveState::NoLogin => LiveView::NoLogin,
            LiveState::Ended { account_id } => LiveView::Ended { account_id },
            LiveState::Known { account_id } => LiveView::Known { account_id },
            LiveState::Unknown { email, org_name } => LiveView::Unknown { email, org_name },
        }
    }
}

/// A switch waiting for "save or discard the current login?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingSwitch {
    pub target_id: String,
    pub target_label: String,
    pub email: Option<String>,
    pub org_name: Option<String>,
}

/// Engine thread commands.
#[derive(Debug, Clone)]
pub(crate) enum EngineCommand {
    Poll(PollReason),
    Publish,
    /// The status line wrote `feed.json`; read it and merge it into the cache.
    Feed,
    /// The sessions thread saw these models in use (lower-case ids), or `None`
    /// when it cannot tell, how many sessions are on a model whose thinking is
    /// tied to the account, and those sessions' answers with thinking minute by
    /// minute (spec §2.11, §2.12).
    ModelsInUse {
        models: Option<BTreeSet<String>>,
        tied_busy: u32,
        thinking: Vec<(DateTime<Utc>, u32)>,
    },
    /// A pause or resume was written to the history; learn it before deciding.
    HistoryChanged,
    Shutdown,
}

/// Action thread commands. Each request that needs an answer carries its own
/// one-shot sender, so a Tauri command never waits on a shared reply slot.
pub(crate) enum ActionCommand {
    Switch {
        key: String,
        unsaved: UnsavedPolicy,
        label: Option<String>,
        by: By,
        reason: Option<String>,
        reply: Sender<ActionReply>,
    },
    SaveCurrent {
        label: Option<String>,
        reply: Sender<ActionReply>,
    },
    Edit {
        key: String,
        label: Option<String>,
        abbr: Option<String>,
        color: Option<String>,
        auto_burn: Option<bool>,
        /// `None` leaves the price alone; `Some(None)` clears it.
        price: Option<Option<f64>>,
        reply: Sender<ActionReply>,
    },
    Remove {
        key: String,
        reply: Sender<ActionReply>,
    },
    SaveSetting {
        field: String,
        value: serde_json::Value,
        reply: Sender<ActionReply>,
    },
    /// The watcher saw a change; mirror it when `mirrorLive` is on.
    SyncLive,
    /// Pause auto-burn for `hours`, or resume with `None` (spec §2.5).
    PauseBurn { hours: Option<i64> },
}

/// One saved setting: the settings as they are after the merge, and a warning
/// when the settings were saved but the autostart entry could not be written.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedSetting {
    pub(crate) settings: Settings,
    pub(crate) warning: Option<String>,
}

/// `Saved` carries the whole settings, including the status line's previous
/// value; a settings save happens once per edit, so boxing it would only move
/// the allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum ActionReply {
    Done,
    Switched(String),
    NotSaved(PendingSwitch),
    Saved(SavedSetting),
    Failed(String),
}

/// What the engine thread hands to the main thread.
///
/// The snapshot is much larger than a sessions summary; boxing it would add an
/// allocation to every tick and change the event's shape for the UI thread.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum UiEvent {
    Snapshot(Snapshot),
    Sessions(SessionsSummary),
    Providers(Vec<ProviderView>),
    Money(MoneyView),
    /// A pushed Sessions area view; the main thread sends it to the large
    /// window only.
    SessionsView(super::window::SessionsView),
}

/// The handles the Tauri side keeps.
pub(crate) struct Worker {
    pub engine: Sender<EngineCommand>,
    pub action: Sender<ActionCommand>,
    /// Present only while `readSessions` is on; the tray keeps it alive.
    pub sessions: Option<SessionsHandle>,
    /// The look-up thread runs whatever `lookUpPrices` says.
    pub lookup: Option<LookupHandle>,
    /// The provider thread; it runs whatever the two card switches say.
    pub providers: Option<ProvidersHandle>,
    /// The history folder the recorder opened at startup. Every reader uses
    /// this one; a changed setting takes effect after a restart.
    pub history: Result<seatswap_core::history::HistoryDir, String>,
    /// The plan inputs, regime and oldest reading of the last auto-burn
    /// decision; the Forecast page reads them through `GuiState`.
    pub forecast: Arc<Mutex<Option<KeptForecast>>>,
}

pub(crate) fn spawn(
    app: AppPaths,
    claude: ClaudePaths,
    autostart_args: Vec<String>,
    ui: Sender<UiEvent>,
    settings: Option<Settings>,
    reported_errors: Arc<ReportedErrors>,
    printed_warnings: Arc<Mutex<PrintedWarnings>>,
) -> Result<Worker, String> {
    let (engine_tx, engine_rx) = mpsc::channel::<EngineCommand>();
    let (action_tx, action_rx) = mpsc::channel::<ActionCommand>();

    // The one history folder this run uses: the recorder's and every read's.
    // Opening it here means a changed `hostName` or `historyDir` is not read
    // from a newer snapshot while the recorder still writes the old folder.
    let history = match settings.as_ref() {
        Some(settings) => ops::open_history(&app, settings).map(|recorder| recorder.dir().clone()),
        None => Err(ops::history_unwritable(&"the settings are not loaded")),
    };
    let forecast = Arc::new(Mutex::new(None));

    let action_engine = engine_tx.clone();
    let engine_action = action_tx.clone();
    let engine_commands = engine_tx.clone();
    let engine_app = app.clone();
    let engine_claude = claude.clone();
    let engine_ui = ui.clone();
    let engine_history = history.clone();
    let engine_forecast = Arc::clone(&forecast);
    let engine_warnings = Arc::clone(&printed_warnings);
    thread::Builder::new()
        .name("seatswap-engine".to_string())
        .spawn(move || {
            EngineRuntime::new(
                engine_app,
                engine_claude,
                engine_ui,
                engine_action,
                engine_commands,
                engine_history,
                engine_forecast,
                engine_warnings,
            )
            .run(engine_rx)
        })
        .map_err(|error| format!("cannot start the engine thread: {error}"))?;

    let lookup = Some(lookup_rt::spawn(app.clone(), Api::new(), ui.clone())?);

    let providers = Some(providers_rt::spawn(
        app.clone(),
        history.clone(),
        Api::new(),
        ui.clone(),
        Arc::clone(&reported_errors),
        Arc::clone(&printed_warnings),
    )?);
    let providers_client = providers.as_ref().map(ProvidersHandle::client);

    let sessions = match settings.as_ref().filter(|settings| settings.read_sessions) {
        Some(_) => Some(sessions_rt::spawn(
            app.clone(),
            claude.clone(),
            history.clone(),
            ui,
            engine_tx.clone(),
        )?),
        None => None,
    };
    let latest = sessions
        .as_ref()
        .map(SessionsHandle::latest)
        .unwrap_or_else(|| Arc::new(Mutex::new(None)));

    let action_claude = claude;
    let action_history = history.clone();
    thread::Builder::new()
        .name("seatswap-action".to_string())
        .spawn(move || {
            ActionRuntime::new(
                app,
                action_claude,
                autostart_args,
                action_engine,
                action_history,
                reported_errors,
                printed_warnings,
            )
            .with_sessions(latest)
            .with_providers(providers_client)
            .run(action_rx)
        })
        .map_err(|error| format!("cannot start the action thread: {error}"))?;

    Ok(Worker {
        engine: engine_tx,
        action: action_tx,
        sessions,
        lookup,
        providers,
        history,
        forecast,
    })
}

// ---------------------------------------------------------------------------
// Engine thread

/// Join two error texts without losing either.
fn combine(first: Option<String>, second: String) -> Option<String> {
    Some(match first {
        Some(first) => format!("{first}; {second}"),
        None => second,
    })
}

/// Everything [`build_snapshot`] needs. The accounts file and the live state
/// are results, not values, because their read errors belong in the snapshot.
pub(crate) struct SnapshotParts {
    pub views: Vec<AccountView>,
    pub accounts: Result<AccountsFile, String>,
    pub live: Result<LiveState, String>,
    pub engine_error: Option<String>,
    pub secrets_warning: Option<String>,
    pub last_poll_at: Option<DateTime<Utc>>,
    pub pending: Option<PendingSwitch>,
    /// A one-shot message (the moved usage cache) for this snapshot only.
    pub notice: Option<String>,
    /// The config folder in effect, already written with `~` for the home folder.
    pub claude_config_dir: String,
}

/// The one place that turns the engine's data into a [`Snapshot`]. The engine
/// thread and the tests both use it, so a test cannot pass against a copy of
/// the real rules.
pub(crate) fn build_snapshot(parts: SnapshotParts) -> Snapshot {
    let mut error = parts.engine_error;
    let mut accounts = Vec::new();
    let (settings, live, warnings) = match parts.accounts {
        Ok(file) => {
            accounts = file
                .accounts
                .iter()
                .map(|account| AccountRow::of(account, &parts.views))
                .collect();
            let live = match parts.live {
                Ok(state) => LiveView::from(state),
                Err(live_error) => {
                    error = combine(error, live_error);
                    LiveView::NoLogin
                }
            };
            (
                Some(file.settings.clone()),
                live,
                seatswap_core::engine::login_warnings(&parts.views),
            )
        }
        Err(store_error) => {
            error = combine(error, format!("cannot read accounts.json: {store_error}"));
            (None, LiveView::NoLogin, Vec::new())
        }
    };
    for view in &parts.views {
        if crate::format::parse_color(&view.color).is_none() {
            error = combine(
                error,
                format!(
                    "account {} has an invalid colour {:?}; set one in the account editor",
                    view.abbr, view.color
                ),
            );
        }
    }
    if let Some(notice) = parts.notice {
        error = combine(error, notice);
    }
    let active_id = parts
        .views
        .iter()
        .find(|view| view.is_active)
        .map(|view| view.id.clone());
    Snapshot {
        views: parts.views,
        accounts,
        active_id,
        live,
        burn: None,
        warnings,
        pending: parts.pending,
        secrets_warning: parts.secrets_warning,
        last_poll_at: parts.last_poll_at,
        error,
        sessions: None,
        providers: Vec::new(),
        settings,
        claude_config_dir: parts.claude_config_dir,
    }
}

/// Open the store and the engine. A corrupt usage cache is moved aside and the
/// engine opened again; the returned notice says where the cache went and is
/// reported once for this run.
struct OpenedEngine {
    engine: Engine<Api>,
    secrets_warning: Option<String>,
    notice: Option<String>,
    history_error: Option<String>,
}

fn open_engine(
    app: &AppPaths,
    claude: &ClaudePaths,
    history: &Result<HistoryDir, String>,
) -> Result<OpenedEngine, String> {
    let (store, mut warning) = store::open_store(app.clone()).map_err(|error| error.to_string())?;
    let (engine, notice) = match Engine::new(claude.clone(), store, Api::new()) {
        Ok(engine) => (engine, None),
        Err(EngineError::CacheCorrupt { .. }) => {
            let moved = UsageCache::set_aside(app).map_err(|error| error.to_string())?;
            let (store, second_warning) =
                store::open_store(app.clone()).map_err(|error| error.to_string())?;
            warning = warning.or(second_warning);
            let engine = Engine::new(claude.clone(), store, Api::new())
                .map_err(|error| error.to_string())?;
            (
                engine,
                Some(format!(
                    "the usage cache was unreadable and was moved to {}; usage is fetched again",
                    moved.display()
                )),
            )
        }
        Err(error) => return Err(error.to_string()),
    };
    let (engine, history_error) = match history {
        Ok(dir) => (engine.with_recorder(Recorder::new(dir.clone())), None),
        Err(error) => (engine, Some(error.clone())),
    };
    Ok(OpenedEngine {
        engine,
        secrets_warning: warning,
        notice,
        history_error,
    })
}

struct EngineRuntime {
    app: AppPaths,
    claude: ClaudePaths,
    ui: Sender<UiEvent>,
    engine: Option<Engine<Api>>,
    /// The history folder opened at startup, shared with every other reader.
    history: Result<HistoryDir, String>,
    secrets_warning: Option<String>,
    /// The engine-wide error of the last open/poll attempt.
    open_error: Option<String>,
    /// The engine thread's history state: why the history cannot be opened,
    /// or the error of the last failed write. Cleared by the next successful
    /// `Live` or heartbeat write; `publish()` never touches it.
    history_error: Option<String>,
    /// The startup `Live` event has been written.
    live_written: bool,
    /// A one-shot message (the moved usage cache) for the next snapshot.
    notice: Option<String>,
    snapshot: Snapshot,
    baseline: ClockBaseline,
    burn: BurnRuntime,
    /// The last auto-burn plan inputs, regime and oldest reading, shared with
    /// `GuiState` for the Forecast page.
    forecast: Arc<Mutex<Option<KeptForecast>>>,
    /// The one set of history read warnings this run has printed.
    printed_warnings: Arc<Mutex<PrintedWarnings>>,
    /// The sessions thread's busy sessions on a thinking-tied model. It is 0
    /// when the sessions thread cannot tell (with `readSessions` off): nothing
    /// is held back then (spec §2.12).
    tied_busy: u32,
    /// The running tied sessions' answers with thinking, minute by minute. It is
    /// emptied together with `tied_busy`.
    thinking: Vec<(DateTime<Utc>, u32)>,
}

/// The wall and monotonic clocks at one moment. Wake detection compares two of
/// these, so the baseline is updated after every `recv_timeout` result.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClockBaseline {
    pub wall: DateTime<Utc>,
    pub monotonic: Instant,
}

impl ClockBaseline {
    fn now() -> ClockBaseline {
        ClockBaseline {
            wall: Utc::now(),
            monotonic: Instant::now(),
        }
    }
}

impl EngineRuntime {
    #[allow(clippy::too_many_arguments)]
    fn new(
        app: AppPaths,
        claude: ClaudePaths,
        ui: Sender<UiEvent>,
        action: Sender<ActionCommand>,
        engine: Sender<EngineCommand>,
        history: Result<HistoryDir, String>,
        forecast: Arc<Mutex<Option<KeptForecast>>>,
        printed_warnings: Arc<Mutex<PrintedWarnings>>,
    ) -> EngineRuntime {
        let mut runtime = EngineRuntime {
            app,
            claude,
            ui,
            engine: None,
            history,
            secrets_warning: None,
            open_error: None,
            history_error: None,
            live_written: false,
            notice: None,
            snapshot: Snapshot::empty(None, None, None),
            baseline: ClockBaseline::now(),
            burn: BurnRuntime::new(action, engine),
            forecast,
            printed_warnings,
            tied_busy: 0,
            thinking: Vec::new(),
        };
        runtime.open();
        // Show the real settings even when the engine could not open.
        let views = runtime
            .engine
            .as_ref()
            .and_then(|engine| engine.views(Utc::now()).ok())
            .unwrap_or_default();
        runtime.capture(views, None);
        runtime
    }

    fn run(mut self, commands: Receiver<EngineCommand>) {
        // The first snapshot is the one built above; it carries a one-shot
        // notice (the moved usage cache) and the settings read at startup.
        self.send();
        let mut deadline = TickDeadline::new(Instant::now());
        loop {
            let wait = deadline.wait(Instant::now());
            match commands.recv_timeout(wait) {
                Ok(command) => {
                    // A command can arrive after any gap, including one that
                    // hid a suspend; check and update the baseline every time.
                    let (baseline, wake) = wake_step(self.baseline, ClockBaseline::now());
                    self.baseline = baseline;
                    let now = Instant::now();
                    let due = deadline.due(now);
                    if !matches!(&command, EngineCommand::Shutdown) && (wake || due) {
                        self.poll(if wake {
                            PollReason::Wake
                        } else {
                            PollReason::Timer
                        });
                    }
                    if due {
                        deadline.advance(now);
                    }
                    match command {
                        EngineCommand::Poll(reason) => self.poll(reason),
                        EngineCommand::Publish => self.publish(),
                        EngineCommand::Feed => self.apply_feed(),
                        EngineCommand::ModelsInUse {
                            models,
                            tied_busy,
                            thinking,
                        } => {
                            self.tied_busy = if models.is_some() { tied_busy } else { 0 };
                            self.thinking = if models.is_some() {
                                thinking
                            } else {
                                Vec::new()
                            };
                            let now = Utc::now();
                            let views = match self.engine.as_mut() {
                                Some(engine) => {
                                    engine.set_models_in_use(models);
                                    match engine.views(now) {
                                        Ok(views) => Some(views),
                                        Err(error) => {
                                            self.open_error = Some(error.to_string());
                                            None
                                        }
                                    }
                                }
                                None => None,
                            }
                            .unwrap_or_else(|| self.snapshot.views.clone());
                            self.capture(views, self.snapshot.last_poll_at);
                            self.send();
                        }
                        EngineCommand::HistoryChanged => {
                            self.burn.force_learn();
                            self.publish();
                        }
                        EngineCommand::Shutdown => return,
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    let (baseline, wake) = wake_step(self.baseline, ClockBaseline::now());
                    self.baseline = baseline;
                    self.poll(if wake {
                        PollReason::Wake
                    } else {
                        PollReason::Timer
                    });
                    deadline.advance(Instant::now());
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Open the store and the engine. False when this failed; the message is in
    /// `open_error` and reaches the next snapshot.
    fn open(&mut self) -> bool {
        match open_engine(&self.app, &self.claude, &self.history) {
            Ok(opened) => {
                self.engine = Some(opened.engine);
                self.history_error = opened.history_error;
                self.secrets_warning = opened
                    .secrets_warning
                    .or_else(|| self.secrets_warning.clone());
                if let Some(notice) = opened.notice {
                    self.notice = Some(match self.notice.take() {
                        Some(previous) => format!("{previous}; {notice}"),
                        None => notice,
                    });
                }
                self.open_error = None;
                self.write_live_once();
                true
            }
            Err(error) => {
                self.open_error = Some(error);
                false
            }
        }
    }

    /// `EventKind::Live{account}` once, after the first successful open with a
    /// recorder. An unreadable live state is tried again on the next tick.
    fn write_live_once(&mut self) {
        if self.live_written {
            return;
        }
        let now = Utc::now();
        let account = match store::load_accounts(&self.app) {
            Ok(file) => match switcher::live_state(&self.claude, &file) {
                Ok(live) => ops::live_account_ref(&live, &file),
                Err(_) => return,
            },
            Err(_) => return,
        };
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let Some(recorder) = engine.recorder() else {
            return;
        };
        match recorder.event(now, EventKind::Live { account }) {
            Ok(()) => {
                self.live_written = true;
                self.history_error = None;
            }
            Err(error) => self.history_error = Some(ops::history_unwritable(&error)),
        }
    }

    /// The engine thread's heartbeat: one `up` line per poll when it has a
    /// recorder. A failed write is the history state; a successful one clears it.
    fn heartbeat(&mut self, now: DateTime<Utc>) {
        let result = self
            .engine
            .as_mut()
            .and_then(|engine| engine.recorder().map(|recorder| recorder.up(now)));
        match result {
            Some(Ok(())) => self.history_error = None,
            Some(Err(error)) => self.history_error = Some(ops::history_unwritable(&error)),
            None => {}
        }
    }

    /// The recorder's read warnings (spec §1.2): a torn last line, a line this
    /// version does not know. The app prints each one once; core never prints.
    fn print_history_warnings(&mut self) {
        let warnings = self
            .engine
            .as_mut()
            .and_then(|engine| engine.recorder())
            .map(|recorder| recorder.take_warnings());
        if let Some(warnings) = warnings {
            super::lock(&self.printed_warnings).print(&warnings);
        }
    }

    fn poll(&mut self, reason: PollReason) {
        if self.engine.is_none() && !self.open() {
            self.capture(Vec::new(), self.snapshot.last_poll_at);
            self.send();
            return;
        }
        self.write_live_once();
        let now = Utc::now();
        let result = match self.engine.as_mut() {
            Some(engine) => engine.poll(reason, now),
            None => return,
        };
        self.print_history_warnings();
        // The heartbeat belongs to the engine thread: it runs with
        // readSessions off too. Its error is the thread's history state, not
        // the engine's open error, so `publish()` cannot clear it.
        self.heartbeat(now);
        match result {
            Ok(views) => {
                self.open_error = None;
                self.capture(views, Some(now));
            }
            Err(error) => {
                self.open_error = Some(error.to_string());
                let views = self.snapshot.views.clone();
                self.capture(views, self.snapshot.last_poll_at);
            }
        }
        self.send();
    }

    /// Read the status line's feed and merge it into the cache. A read or apply
    /// error is the engine thread's open error, as a failed poll is; the
    /// snapshot is published either way.
    fn apply_feed(&mut self) {
        if self.engine.is_none() && !self.open() {
            self.capture(Vec::new(), self.snapshot.last_poll_at);
            self.send();
            return;
        }
        let now = Utc::now();
        let result = match feed::read(&self.app) {
            Ok(Some(feed)) => match self.engine.as_mut() {
                Some(engine) => engine
                    .apply_feed(&feed, now)
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                None => Ok(()),
            },
            Ok(None) => Ok(()),
            Err(error) => Err(format!(
                "cannot read {}: {error}",
                self.app.feed_file().display()
            )),
        };
        match result {
            Ok(()) => {
                self.open_error = None;
                let views = match self.engine.as_ref() {
                    Some(engine) => match engine.views(now) {
                        Ok(views) => views,
                        Err(error) => {
                            self.open_error = Some(error.to_string());
                            self.snapshot.views.clone()
                        }
                    },
                    None => self.snapshot.views.clone(),
                };
                self.capture(views, self.snapshot.last_poll_at);
            }
            Err(error) => {
                self.open_error = Some(error);
                self.capture(self.snapshot.views.clone(), self.snapshot.last_poll_at);
            }
        }
        self.send();
    }

    fn publish(&mut self) {
        if self.engine.is_none() {
            self.open();
        }
        let now = Utc::now();
        let (views, last_poll_at) = match self.engine.as_ref() {
            Some(engine) => match engine.views(now) {
                Ok(views) => {
                    self.open_error = None;
                    (views, self.snapshot.last_poll_at)
                }
                Err(error) => {
                    self.open_error = Some(error.to_string());
                    (self.snapshot.views.clone(), self.snapshot.last_poll_at)
                }
            },
            None => (self.snapshot.views.clone(), self.snapshot.last_poll_at),
        };
        self.capture(views, last_poll_at);
        self.send();
    }

    fn capture(&mut self, views: Vec<AccountView>, last_poll_at: Option<DateTime<Utc>>) {
        let accounts = store::load_accounts(&self.app).map_err(|error| error.to_string());
        let live = match &accounts {
            Ok(file) => switcher::live_state(&self.claude, file).map_err(|error| error.to_string()),
            Err(_) => Err(
                "the live login was not checked because accounts.json could not be read"
                    .to_string(),
            ),
        };
        let mut engine_error = self.open_error.clone();
        if let Some(history_error) = &self.history_error {
            engine_error = combine(engine_error, history_error.clone());
        }
        let accounts_for_burn = accounts.as_ref().ok().map(|file| file.accounts.clone());
        let claude_config_dir = with_tilde(
            &self.claude.config_dir.to_string_lossy(),
            self.claude.home(),
        );
        self.snapshot = build_snapshot(SnapshotParts {
            views,
            accounts,
            live,
            engine_error,
            secrets_warning: self.secrets_warning.clone(),
            last_poll_at,
            pending: None,
            notice: self.notice.take(),
            claude_config_dir,
        });
        self.update_burn(accounts_for_burn);
    }

    /// Run auto-burn after the snapshot is built: learn, decide, send. Its error
    /// joins the snapshot's error; a warning is printed once.
    fn update_burn(&mut self, accounts: Option<Vec<Account>>) {
        let (Some(accounts), Some(settings)) = (accounts, self.snapshot.settings.as_ref().cloned())
        else {
            return;
        };
        let now = Utc::now();
        let history = self
            .engine
            .as_mut()
            .and_then(|engine| engine.recorder())
            .map(|recorder| recorder.dir());
        let tick = BurnTick {
            app: &self.app,
            history,
            views: &self.snapshot.views,
            accounts: &accounts,
            settings: &settings,
            active: self.snapshot.active_id.as_deref(),
            now,
            offset: *Local::now().offset(),
            tied_busy: self.tied_busy,
            thinking: &self.thinking,
        };
        let report = self.burn.after_snapshot(&tick);
        *super::lock(&self.forecast) = report.forecast;
        self.snapshot.burn = report.view;
        if let Some(error) = report.error {
            self.snapshot.error = combine(self.snapshot.error.take(), error);
        }
        super::lock(&self.printed_warnings).print(&report.warnings);
        if let Some(text) = report.notification {
            if settings.notify_on_switch {
                notify::send_switch(&self.app, &text);
            }
        }
    }

    fn send(&self) {
        let _ = self.ui.send(UiEvent::Snapshot(self.snapshot.clone()));
    }
}

/// True when the wall clock moved more than [`WAKE_THRESHOLD`] further than the
/// monotonic clock: the machine was asleep in between.
pub(crate) fn wake_detected(
    previous_wall: DateTime<Utc>,
    previous_monotonic: Instant,
    now_wall: DateTime<Utc>,
    now_monotonic: Instant,
) -> bool {
    let wall_ms = now_wall
        .signed_duration_since(previous_wall)
        .num_milliseconds();
    let monotonic_ms = i64::try_from(now_monotonic.duration_since(previous_monotonic).as_millis())
        .unwrap_or(i64::MAX);
    wall_ms.saturating_sub(monotonic_ms)
        > i64::try_from(WAKE_THRESHOLD.as_millis()).unwrap_or(i64::MAX)
}

/// The engine loop's wake decision as one pure step: the next baseline is
/// `now`, and `wake` is true when the clocks disagree by more than
/// [`WAKE_THRESHOLD`]. Called after every command and every timeout.
pub(crate) fn wake_step(previous: ClockBaseline, now: ClockBaseline) -> (ClockBaseline, bool) {
    let wake = wake_detected(previous.wall, previous.monotonic, now.wall, now.monotonic);
    (now, wake)
}

// ---------------------------------------------------------------------------
// Action thread

/// A notification a successful switch sends, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SwitchNotice {
    Switched(String),
    Error(String),
}

/// `notifyOnSwitch` followed by `false` is silence; a read failure sends the
/// switch itself *and* the error, because the switch happened and the person
/// must learn both. An unknown setting is never treated as `true`.
pub(crate) fn switch_notices(
    message: &str,
    notify_on_switch: Result<bool, String>,
) -> Vec<SwitchNotice> {
    match notify_on_switch {
        Ok(true) => vec![SwitchNotice::Switched(message.to_string())],
        Ok(false) => Vec::new(),
        Err(error) => vec![
            SwitchNotice::Switched(message.to_string()),
            SwitchNotice::Error(format!(
                "the switch notification setting could not be read: {error}"
            )),
        ],
    }
}

struct ActionRuntime {
    app: AppPaths,
    claude: ClaudePaths,
    autostart_args: Vec<String>,
    engine: Sender<EngineCommand>,
    store: Option<Store>,
    /// The recorder, or the sentence that says why there is none.
    history: Result<Recorder, String>,
    latest_sessions: Arc<Mutex<Option<SessionsSummary>>>,
    /// The provider thread's commands; every settings save tells it.
    providers: Option<ProvidersClient>,
    /// The last account seen live, so an external switch has a `from`.
    live: Option<AccountRef>,
    /// The one set of history read warnings this run has printed, so a torn
    /// line is not printed again at every live-account change.
    printed_warnings: Arc<Mutex<PrintedWarnings>>,
    /// The one app error reporter for a failed history write.
    reported_errors: Arc<ReportedErrors>,
}

impl ActionRuntime {
    fn new(
        app: AppPaths,
        claude: ClaudePaths,
        autostart_args: Vec<String>,
        engine: Sender<EngineCommand>,
        history: Result<HistoryDir, String>,
        reported_errors: Arc<ReportedErrors>,
        printed_warnings: Arc<Mutex<PrintedWarnings>>,
    ) -> ActionRuntime {
        let loaded = store::load_accounts(&app);
        let history = history.map(Recorder::new);
        let live = loaded.ok().and_then(|file| {
            switcher::live_state(&claude, &file)
                .ok()
                .and_then(|state| ops::live_account_ref(&state, &file))
        });
        ActionRuntime {
            app,
            claude,
            autostart_args,
            engine,
            store: None,
            history,
            latest_sessions: Arc::new(Mutex::new(None)),
            providers: None,
            live,
            printed_warnings,
            reported_errors,
        }
    }

    fn with_sessions(mut self, latest: Arc<Mutex<Option<SessionsSummary>>>) -> ActionRuntime {
        self.latest_sessions = latest;
        self
    }

    fn with_providers(mut self, providers: Option<ProvidersClient>) -> ActionRuntime {
        self.providers = providers;
        self
    }

    fn recorder(&self) -> Result<&Recorder, String> {
        self.history.as_ref().map_err(Clone::clone)
    }

    fn session_counts(&self) -> Option<SessionCounts> {
        let latest = match self.latest_sessions.lock() {
            Ok(latest) => latest,
            Err(poisoned) => poisoned.into_inner(),
        };
        latest.as_ref().map(|summary| SessionCounts {
            running: summary.running,
            busy: summary.busy,
            tied: Some(summary.tied),
        })
    }

    fn run(mut self, commands: Receiver<ActionCommand>) {
        for command in commands {
            match command {
                ActionCommand::Switch {
                    key,
                    unsaved,
                    label,
                    by,
                    reason,
                    reply,
                } => {
                    let result = self.switch(&key, unsaved, label, by, reason);
                    let _ = reply.send(result);
                }
                ActionCommand::SaveCurrent { label, reply } => {
                    let _ = reply.send(self.save_current(label));
                }
                ActionCommand::Edit {
                    key,
                    label,
                    abbr,
                    color,
                    auto_burn,
                    price,
                    reply,
                } => {
                    let _ = reply.send(self.edit(&key, label, abbr, color, auto_burn, price));
                }
                ActionCommand::Remove { key, reply } => {
                    let _ = reply.send(self.remove(&key));
                }
                ActionCommand::SaveSetting {
                    field,
                    value,
                    reply,
                } => {
                    let _ = reply.send(self.save_setting(field, value));
                }
                ActionCommand::SyncLive => self.sync_live(),
                ActionCommand::PauseBurn { hours } => {
                    if let Err(error) = self.pause_burn(hours) {
                        notify::send_error(&self.app, &error);
                    }
                    let _ = self.engine.send(EngineCommand::HistoryChanged);
                }
            }
        }
    }

    fn ensure_store(&mut self) -> Result<(), String> {
        if self.store.is_none() {
            let (store, _warning) =
                store::open_store(self.app.clone()).map_err(|e| e.to_string())?;
            self.store = Some(store);
        }
        Ok(())
    }

    fn store(&mut self) -> Result<&Store, String> {
        self.ensure_store()?;
        self.store
            .as_ref()
            .ok_or_else(|| "the store is not open".to_string())
    }

    /// The recorder for one action and, when there is none, the reason. The
    /// reason is reported even when the action's record was not attempted, so
    /// a history that cannot be opened is never silently dropped.
    fn history_for_action(&self) -> (Option<&Recorder>, Option<String>) {
        match &self.history {
            Ok(recorder) => (Some(recorder), None),
            Err(error) => (None, Some(error.clone())),
        }
    }

    fn switch(
        &mut self,
        key: &str,
        unsaved: UnsavedPolicy,
        label: Option<String>,
        by: By,
        reason: Option<String>,
    ) -> ActionReply {
        let policy = match (unsaved, label) {
            (UnsavedPolicy::Save { .. }, Some(label)) => UnsavedPolicy::Save { label: Some(label) },
            (policy, _) => policy,
        };
        let claude = self.claude.clone();
        let result = match self.store() {
            Ok(store) => switcher::switch_to(&claude, store, key, policy, Utc::now()),
            Err(error) => return ActionReply::Failed(error),
        };
        match result {
            Ok(outcome) => {
                let message = match self.store() {
                    Ok(store) => match ops::switch_message(store, &outcome.to) {
                        Ok(message) => message,
                        Err(error) => ops::switched_without_list(&error),
                    },
                    Err(error) => ops::switched_without_list(&error),
                };
                let now = Utc::now();
                let counts = self.session_counts();
                let loaded = self
                    .store()
                    .and_then(|store| store.load().map_err(|error| error.to_string()));
                let mut to_ref = None;
                let mut history_error = None;
                match loaded {
                    Ok(file) => match file.find(&outcome.to) {
                        Some(account) => {
                            let target = AccountRef::of(account);
                            to_ref = Some(target.clone());
                            let from = outcome
                                .from
                                .as_deref()
                                .and_then(|id| file.find(id))
                                .map(AccountRef::of);
                            let saved_new =
                                outcome.saved_new.as_deref().and_then(|id| file.find(id));
                            match self.recorder() {
                                Ok(recorder) => {
                                    if let Err(error) = ops::record_switch(
                                        recorder, from, target, saved_new, by, reason, None,
                                        counts, now,
                                    ) {
                                        history_error = Some(error.to_string());
                                    } else if let Some(change) = &outcome.plan_change {
                                        if let Err(error) =
                                            ops::record_plan_change(recorder, change, now)
                                        {
                                            history_error = Some(error.to_string());
                                        }
                                    }
                                }
                                Err(error) => history_error = Some(error),
                            }
                        }
                        None => {
                            history_error = Some(format!("the store has no account {}", outcome.to))
                        }
                    },
                    Err(error) => history_error = Some(error),
                }
                if to_ref.is_some() && history_error.is_none() {
                    // The event is written: the next learn must see the switch
                    // (its hold and its cost) before the next snapshot decides.
                    let _ = self.engine.send(EngineCommand::HistoryChanged);
                }
                if let Some(target) = to_ref {
                    self.live = Some(target);
                }
                let reply_message = match history_error {
                    Some(error) => format!("{message} (not written to the history: {error})"),
                    None => message.clone(),
                };
                let notify_on_switch = match self
                    .store()
                    .and_then(|store| store.load().map_err(|e| e.to_string()))
                {
                    Ok(file) => Ok(file.settings.notify_on_switch),
                    Err(error) => Err(error),
                };
                for notice in switch_notices(&message, notify_on_switch) {
                    match notice {
                        // An automatic switch has its own text (spec §2.5); the
                        // engine thread sends it once the switch has landed.
                        SwitchNotice::Switched(_) if by == By::Auto => {}
                        SwitchNotice::Switched(text) => notify::send_switch(&self.app, &text),
                        SwitchNotice::Error(text) => notify::send_error(&self.app, &text),
                    }
                }
                let _ = self
                    .engine
                    .send(EngineCommand::Poll(PollReason::AfterSwitch));
                ActionReply::Switched(reply_message)
            }
            Err(SwitchError::CurrentNotSaved { email }) => match self.pending_for(key, email) {
                Ok(pending) => ActionReply::NotSaved(pending),
                Err(message) => {
                    notify::send_error(&self.app, &message);
                    ActionReply::Failed(message)
                }
            },
            Err(error) => {
                let message = error.to_string();
                // A switch error is always shown, whatever notifyOnSwitch says.
                notify::send_error(&self.app, &message);
                ActionReply::Failed(message)
            }
        }
    }

    fn pending_for(&mut self, key: &str, email: Option<String>) -> Result<PendingSwitch, String> {
        let file = self.store()?.load().map_err(|error| error.to_string())?;
        let live = switcher::live_state(&self.claude, &file).map_err(|error| error.to_string())?;
        pending_switch(&file, &live, key, email)
    }

    /// Pause auto-burn for `hours`, or resume with `None`, as one history
    /// event (spec §2.5). A zero or negative pause is refused, never clamped.
    fn pause_burn(&mut self, hours: Option<i64>) -> Result<(), String> {
        let now = Utc::now();
        let paused_until = match hours {
            Some(hours) if hours <= 0 => {
                return Err(format!("a pause must be at least one hour, got {hours}"))
            }
            Some(hours) => Some(now + chrono::Duration::hours(hours)),
            None => None,
        };
        let on = self
            .store()?
            .load()
            .map_err(|error| error.to_string())?
            .settings
            .auto_burn;
        self.record(EventKind::AutoBurn { on, paused_until })
    }

    /// Write one event to the history. A pause or resume and a settings save
    /// that turns auto-burn on or off both come here, each with its own event.
    fn record(&mut self, kind: EventKind) -> Result<(), String> {
        let recorder = self.recorder()?;
        recorder
            .event(Utc::now(), kind)
            .map_err(|error| ops::history_unwritable(&error))
    }

    fn save_current(&mut self, label: Option<String>) -> ActionReply {
        if let Err(error) = self.ensure_store() {
            return ActionReply::Failed(error);
        }
        let claude = self.claude.clone();
        let Some(store) = self.store.as_ref() else {
            return ActionReply::Failed("the store is not open".to_string());
        };
        let (history, open_error) = self.history_for_action();
        let result = ops::save_live_login(&claude, store, history, label, Utc::now());
        match result {
            Ok((_, history_error)) => {
                if let Some(error) = history_error.or(open_error) {
                    self.reported_errors.report(&error);
                }
                let _ = self.engine.send(EngineCommand::Publish);
                ActionReply::Done
            }
            Err(error) => ActionReply::Failed(error),
        }
    }

    fn edit(
        &mut self,
        key: &str,
        label: Option<String>,
        abbr: Option<String>,
        color: Option<String>,
        auto_burn: Option<bool>,
        price: Option<Option<f64>>,
    ) -> ActionReply {
        let result = match self.store() {
            Ok(store) => switcher::edit_account(
                store,
                key,
                label.as_deref(),
                abbr.as_deref(),
                color.as_deref(),
                auto_burn,
                price,
            ),
            Err(error) => return ActionReply::Failed(error),
        };
        match result {
            Ok(edited) => {
                if edited.price_changed {
                    let event = EventKind::PriceChanged {
                        account: AccountRef::of(&edited.account),
                        amount: edited.account.price,
                        currency: edited.account.price_currency.clone(),
                    };
                    if let Err(error) = self.record(event) {
                        self.reported_errors.report(&error);
                    }
                }
                let _ = self.engine.send(EngineCommand::Publish);
                ActionReply::Done
            }
            Err(error) => ActionReply::Failed(error.to_string()),
        }
    }

    fn remove(&mut self, key: &str) -> ActionReply {
        if let Err(error) = self.ensure_store() {
            return ActionReply::Failed(error);
        }
        let Some(store) = self.store.as_ref() else {
            return ActionReply::Failed("the store is not open".to_string());
        };
        let (history, open_error) = self.history_for_action();
        match ops::remove_account(store, key, history, Utc::now()) {
            Ok((_, history_error)) => {
                if let Some(error) = history_error.or(open_error) {
                    self.reported_errors.report(&error);
                }
                let _ = self.engine.send(EngineCommand::Publish);
                ActionReply::Done
            }
            Err(error) => ActionReply::Failed(error),
        }
    }

    fn save_setting(&mut self, field: String, value: serde_json::Value) -> ActionReply {
        let home = self.claude.home.clone();
        let result = (|| -> Result<(Settings, Settings), String> {
            let store = self.store()?;
            let _lock = locks::acquire_app_lock(store.app_paths(), switcher::APP_LOCK_TIMEOUT)
                .map_err(|error| error.to_string())?;
            let mut file = store.load().map_err(|error| error.to_string())?;
            let previous = file.settings.clone();
            let settings = merge_setting(&previous, &field, value, &home)?;
            file.settings = settings.clone();
            store.save(&file).map_err(|error| error.to_string())?;
            Ok((previous, settings))
        })();
        let (previous, settings) = match result {
            Ok(saved) => saved,
            Err(error) => return ActionReply::Failed(error),
        };
        // The setting is saved; its history event is one place, also used when
        // the pop-up changes `autoBurn`.
        if let Some(event) = settings_change_event(&previous, &settings) {
            match self.record(event) {
                Ok(()) => {
                    let _ = self.engine.send(EngineCommand::HistoryChanged);
                }
                Err(error) => self.reported_errors.report(&error),
            }
        }
        let warning = autostart::apply_autostart(settings.start_at_login, &self.autostart_args)
            .err()
            .map(|error| autostart_failure_message(&error));
        if let Some(providers) = &self.providers {
            let _ = providers.settings_saved();
        }
        let _ = self.engine.send(EngineCommand::Publish);
        ActionReply::Saved(SavedSetting { settings, warning })
    }

    fn sync_live(&mut self) {
        let mirror = match self.store() {
            Ok(store) => match store.load() {
                Ok(file) => file.settings.mirror_live,
                Err(error) => {
                    eprintln!("seatswap: watcher: {error}");
                    false
                }
            },
            Err(error) => {
                eprintln!("seatswap: watcher: {error}");
                false
            }
        };
        let mut lock_busy = false;
        if mirror {
            let claude = self.claude.clone();
            let result = match self.store() {
                Ok(store) => Some(switcher::sync_live(&claude, store, None, Utc::now())),
                Err(_) => None,
            };
            match result {
                Some(Ok((
                    _,
                    Synced::Updated {
                        plan_change: Some(change),
                    },
                ))) => match self.recorder() {
                    Ok(recorder) => {
                        if let Err(error) = ops::record_plan_change(recorder, &change, Utc::now()) {
                            eprintln!("seatswap: watcher: {}", ops::history_unwritable(&error));
                        }
                    }
                    Err(error) => eprintln!("seatswap: watcher: {error}"),
                },
                Some(Ok(_)) | None => {}
                // Claude Code is mid-renewal; its next change calls us again.
                Some(Err(SwitchError::Lock(LockError::Busy { .. }))) => lock_busy = true,
                Some(Err(error)) => eprintln!("seatswap: watcher: {error}"),
            }
        }
        if !lock_busy {
            let loaded = self
                .store()
                .and_then(|store| store.load().map_err(|error| error.to_string()));
            match loaded {
                Ok(file) => match switcher::live_state(&self.claude, &file) {
                    Ok(state) => {
                        let current = ops::live_account_ref(&state, &file);
                        if current != self.live {
                            if let Some(to) = &current {
                                let counts = self.session_counts();
                                match &self.history {
                                    Ok(recorder) => match record_external_switch(
                                        recorder,
                                        &self.printed_warnings,
                                        self.live.clone(),
                                        to,
                                        counts,
                                        Utc::now(),
                                    ) {
                                        Ok(RecentSwitch::Already { by: By::Agent, cwd }) => {
                                            if let Some(account) =
                                                file.find_by_identity(&to.acc, &to.org)
                                            {
                                                notify::send_switch(
                                                    &self.app,
                                                    &agent_switch_message(
                                                        &account.abbr,
                                                        cwd.as_deref(),
                                                        self.claude.home(),
                                                    ),
                                                );
                                            }
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            eprintln!("seatswap: watcher: {error}")
                                        }
                                    },
                                    Err(error) => eprintln!("seatswap: watcher: {error}"),
                                }
                            }
                            self.live = current;
                        }
                    }
                    Err(error) => eprintln!("seatswap: watcher: {error}"),
                },
                Err(error) => eprintln!("seatswap: watcher: {error}"),
            }
        }
        let _ = self.engine.send(EngineCommand::Publish);
    }
}

/// The settings with one field replaced, checked as a whole. `field` must name
/// a key of the settings' JSON object; a bad value is the field's name followed
/// by serde's text, and an invalid value is [`Settings::validate`]'s error.
/// A `~` path typed into one of the path settings starts at the home folder
/// (`format::expand_tilde`: `~/`, and `~\` on Windows).
fn merge_setting(
    settings: &Settings,
    field: &str,
    value: serde_json::Value,
    home: &Path,
) -> Result<Settings, String> {
    let mut object =
        serde_json::to_value(settings).map_err(|error| format!("settings: {error}"))?;
    let entries = object
        .as_object_mut()
        .ok_or_else(|| "the settings are not a JSON object".to_string())?;
    if !entries.contains_key(field) {
        return Err(format!("unknown setting {field}"));
    }
    let value = match value {
        serde_json::Value::String(text)
            if matches!(field, "claudeConfigDir" | "historyDir" | "codexHome") =>
        {
            // `expand_tilde` owns which spellings name the home folder (`~\`
            // too on Windows, as the page shows it); any other text stays.
            let expanded = expand_tilde(&text, home);
            let text = expanded
                .into_os_string()
                .into_string()
                .map_err(|_| format!("{field}: the home folder is not valid UTF-8"))?;
            serde_json::Value::String(text)
        }
        other => other,
    };
    entries.insert(field.to_string(), value);
    let merged: Settings =
        serde_json::from_value(object).map_err(|error| format!("{field}: {error}"))?;
    merged.validate().map_err(|error| error.to_string())?;
    Ok(merged)
}

/// The history event a settings save writes, or `None` when the save changes
/// none of the settings the history records. Turning auto-burn on or off is
/// `AutoBurnSet`, never a pause or resume (spec §2.5).
fn settings_change_event(previous: &Settings, next: &Settings) -> Option<EventKind> {
    if previous.auto_burn != next.auto_burn {
        return Some(EventKind::AutoBurnSet { on: next.auto_burn });
    }
    None
}

/// The history read warnings this run of the app has already printed. A month
/// file is read again at every live-account change, so its torn lines would
/// otherwise be printed again each time.
#[derive(Debug, Default)]
pub(crate) struct PrintedWarnings {
    seen: BTreeSet<String>,
}

impl PrintedWarnings {
    pub(crate) fn new() -> PrintedWarnings {
        PrintedWarnings {
            seen: BTreeSet::new(),
        }
    }

    /// The warnings in `warnings` this run has not printed yet; they are
    /// remembered so the next call returns none of them.
    pub(crate) fn new_texts(&mut self, warnings: &[String]) -> Vec<String> {
        let mut texts = Vec::new();
        for warning in warnings {
            if self.seen.insert(warning.clone()) {
                texts.push(warning.clone());
            }
        }
        texts
    }

    /// Print the warnings this run has not printed yet, in the spec's one form
    /// (`seatswap: history: <warning>`, spec §1.2).
    pub(crate) fn print(&mut self, warnings: &[String]) {
        for warning in self.new_texts(warnings) {
            eprintln!("seatswap: history: {warning}");
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.seen.len()
    }
}

/// What [`record_external_switch`] found: it wrote a new external switch, or a
/// switch to the same account is already in this host's last 15 s.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecentSwitch {
    Written,
    Already { by: By, cwd: Option<String> },
}

/// Write `Switch{by: External}` for a live account, unless this host's events
/// already hold a switch to the same account in the last 15 s; that event's
/// `by` and `cwd` are returned so an agent's switch can be announced. The
/// month file's read warnings are printed once per run through `printed`.
fn record_external_switch(
    recorder: &Recorder,
    printed: &Mutex<PrintedWarnings>,
    from: Option<AccountRef>,
    to: &AccountRef,
    sessions: Option<SessionCounts>,
    now: DateTime<Utc>,
) -> Result<RecentSwitch, HistoryError> {
    let read = recorder.dir().read::<Event>(
        &Range {
            from: now - chrono::Duration::seconds(15),
            to: now + chrono::Duration::seconds(1),
        },
        &Hosts::One(recorder.dir().host().to_string()),
    )?;
    super::lock(printed).print(&read.warnings);
    let already = read
        .records
        .iter()
        .rev()
        .find_map(|hosted| match &hosted.record.kind {
            EventKind::Switch {
                to: existing,
                by,
                cwd,
                ..
            } if existing == to => Some((*by, cwd.clone())),
            _ => None,
        });
    if let Some((by, cwd)) = already {
        return Ok(RecentSwitch::Already { by, cwd });
    }
    ops::record_switch(
        recorder,
        from,
        to.clone(),
        None,
        By::External,
        None,
        None,
        sessions,
        now,
    )?;
    Ok(RecentSwitch::Written)
}

/// The tray app announces every switch an agent made (spec §6.2), whatever
/// `notifyOnSwitch` says.
fn agent_switch_message(abbr: &str, cwd: Option<&str>, home: &Path) -> String {
    match cwd {
        Some(cwd) => format!(
            "An agent switched to {abbr} (from {}).",
            with_tilde(cwd, home)
        ),
        None => format!("An agent switched to {abbr}."),
    }
}

/// A switch waiting for "save or discard?" only when the target key names a
/// real account; an unknown key is the store's error, never a pending carrying
/// the raw key.
fn pending_switch(
    file: &AccountsFile,
    live: &LiveState,
    key: &str,
    email: Option<String>,
) -> Result<PendingSwitch, String> {
    let account = file
        .find_by_key(key)
        .ok_or_else(|| format!("unknown account {key}"))?;
    let org_name = match live {
        LiveState::Unknown { org_name, .. } => org_name.clone(),
        _ => None,
    };
    Ok(PendingSwitch {
        target_id: account.id.clone(),
        target_label: account.label.clone(),
        email,
        org_name,
    })
}

/// The reply when accounts.json was saved but the autostart entry could not be
/// written. It must say that the settings are saved, not that the save failed.
fn autostart_failure_message(error: &str) -> String {
    format!("settings saved; start at login could not be set: {error}")
}

/// A `Remote` used only by the worker tests.
#[cfg(test)]
#[derive(Clone)]
struct FakeRemote {
    usage: seatswap_core::api::Usage,
}

#[cfg(test)]
impl Remote for FakeRemote {
    fn usage(&self, _access_token: &str) -> Result<seatswap_core::api::Usage, ApiError> {
        Ok(self.usage.clone())
    }

    fn refresh(
        &self,
        _login: &Login,
        _expected: (&str, &str),
        _now_ms: i64,
    ) -> Result<Login, ApiError> {
        Err(ApiError::Decode("the fake does not refresh".to_string()))
    }
}

/// Gather the real inputs for [`build_snapshot`] from a test engine; all the
/// snapshot rules stay in the one function the engine thread uses too.
#[cfg(test)]
fn snapshot_from_engine<R: Remote>(
    engine: &Engine<R>,
    views: Vec<AccountView>,
    secrets_warning: Option<String>,
    last_poll_at: Option<DateTime<Utc>>,
) -> Snapshot {
    let accounts = engine.store().load().map_err(|error| error.to_string());
    let live = match &accounts {
        Ok(file) => {
            switcher::live_state(engine.claude_paths(), file).map_err(|error| error.to_string())
        }
        Err(_) => Err("accounts.json could not be read".to_string()),
    };
    build_snapshot(SnapshotParts {
        views,
        accounts,
        live,
        engine_error: None,
        secrets_warning,
        last_poll_at,
        pending: None,
        notice: None,
        claude_config_dir: with_tilde(
            &engine.claude_paths().config_dir.to_string_lossy(),
            engine.claude_paths().home(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use seatswap_core::api::{Limit, LimitKind, Severity, Usage};
    use seatswap_core::live;
    use seatswap_core::store::FileSecrets;
    use seatswap_core::store::{self, Account, AccountsFile, SecretsBackend};
    use serde_json::json;

    const ACCOUNT_UUID: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const ORG_UUID: &str = "e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62";
    const EMAIL: &str = "someone@example.com";

    /// The app's one error reporter with a sink a test ignores.
    fn test_errors() -> Arc<ReportedErrors> {
        Arc::new(ReportedErrors::new(Box::new(|_| {})))
    }

    /// The app's one warning printer for a runtime a test builds by hand.
    fn test_printed_warnings() -> Arc<Mutex<PrintedWarnings>> {
        Arc::new(Mutex::new(PrintedWarnings::new()))
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        claude: ClaudePaths,
        app: AppPaths,
        account: Account,
    }

    fn usage(session: f64, week: f64) -> Usage {
        Usage {
            limits: vec![
                Limit {
                    kind: LimitKind::Session,
                    percent: session,
                    severity: Severity::Critical,
                    resets_at: Some(Utc::now() + chrono::Duration::hours(1)),
                    model: None,
                    is_active: true,
                },
                Limit {
                    kind: LimitKind::WeeklyAll,
                    percent: week,
                    severity: Severity::Normal,
                    resets_at: None,
                    model: None,
                    is_active: false,
                },
            ],
            extra_usage_enabled: false,
            warnings: Vec::new(),
        }
    }

    fn credentials_json(refresh_token: &str) -> serde_json::Value {
        json!({
            "mcpOAuth": {
                "plugin:engineering:linear|5f1c2a9be0d34c11": {
                    "serverName": "plugin:engineering:linear",
                    "accessToken": "lin_oauth_placeholder",
                    "expiresAt": 1_794_400_000_000_i64
                }
            },
            "claudeAiOauth": {
                "accessToken": format!("sk-ant-oat01-{}", "a".repeat(95)),
                "refreshToken": refresh_token,
                "expiresAt": 1_794_356_040_000_i64,
                "refreshTokenExpiresAt": 1_796_772_840_000_i64,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "team",
                "subscriptionTypeSource": "profile",
                "rateLimitTier": "default_claude_max_5x"
            },
            "organizationUuid": ORG_UUID
        })
    }

    fn identity_json(account_uuid: &str, org_uuid: &str) -> serde_json::Value {
        json!({
            "numStartups": 41,
            "oauthAccount": {
                "accountUuid": account_uuid,
                "emailAddress": EMAIL,
                "organizationUuid": org_uuid,
                "organizationName": "Acme",
                "organizationRole": "admin",
                "billingType": "stripe_subscription",
                "hasExtraUsageEnabled": false
            },
            "projects": {}
        })
    }

    impl Fixture {
        fn new() -> Fixture {
            let tmp = tempfile::tempdir().expect("tempdir");
            let config = tmp.path().join("claude");
            fs::create_dir_all(&config).expect("config folder");
            let claude = ClaudePaths::for_config_dir(&config, tmp.path());
            let app = AppPaths::at(&tmp.path().join("data"));
            fs::create_dir_all(app.data_dir.as_path()).expect("data folder");

            fs::write(
                &claude.credentials_file,
                credentials_json(&format!("sk-ant-ort01-{}", "b".repeat(95))).to_string(),
            )
            .expect("credentials");
            fs::write(
                &claude.identity_file,
                identity_json(ACCOUNT_UUID, ORG_UUID).to_string(),
            )
            .expect("identity");

            let store = Store::new(app.clone(), Box::new(FileSecrets::new(app.secrets_dir())));
            let login = live::read_login(&claude)
                .expect("read login")
                .expect("live login");
            let identity = live::read_identity(&claude)
                .expect("read identity")
                .expect("live identity");
            let file = store.load().expect("empty accounts");
            let account = store::new_account(&login, &identity, Some("Acme"), &file, Utc::now())
                .expect("new account");
            store
                .secrets()
                .set(&account.id, &login)
                .expect("store secret");
            let mut file: AccountsFile = store.load().expect("accounts");
            file.accounts.push(account.clone());
            // Record the file backend so `open_store` never probes the real keyring.
            file.secrets_backend = Some(SecretsBackend::File);
            // The display currency a price change is recorded in (Task 4).
            file.settings.currency = Some("EUR".to_string());
            store.save(&file).expect("save accounts");

            Fixture {
                _tmp: tmp,
                claude,
                app,
                account,
            }
        }

        fn history(&self) -> Result<seatswap_core::history::HistoryDir, String> {
            let file = store::load_accounts(&self.app).map_err(|error| error.to_string())?;
            ops::open_history(&self.app, &file.settings).map(|recorder| recorder.dir().clone())
        }

        fn engine(&self) -> Engine<FakeRemote> {
            let store = Store::new(
                self.app.clone(),
                Box::new(FileSecrets::new(self.app.secrets_dir())),
            );
            Engine::new(
                self.claude.clone(),
                store,
                FakeRemote {
                    usage: usage(97.0, 48.0),
                },
            )
            .expect("engine")
        }

        fn write_live(&self, credentials: serde_json::Value, identity: serde_json::Value) {
            fs::write(&self.claude.credentials_file, credentials.to_string())
                .expect("write credentials");
            fs::write(&self.claude.identity_file, identity.to_string()).expect("write identity");
        }
    }

    /// The two senders a runtime needs. The receivers are dropped: these tests
    /// read the snapshot, not the commands.
    fn burn_channels() -> (Sender<ActionCommand>, Sender<EngineCommand>) {
        let (action, _action_rx) = mpsc::channel();
        let (engine, _engine_rx) = mpsc::channel();
        (action, engine)
    }

    #[test]
    fn snapshot_names_the_active_account_and_the_live_login() {
        let fixture = Fixture::new();
        let mut engine = fixture.engine();
        let now = Utc::now();
        let views = engine.poll(PollReason::Timer, now).expect("poll");
        let snapshot = snapshot_from_engine(&engine, views, None, Some(now));

        assert_eq!(
            snapshot.active_id.as_deref(),
            Some(fixture.account.id.as_str())
        );
        assert_eq!(
            snapshot.live,
            LiveView::Known {
                account_id: fixture.account.id.clone()
            }
        );
        assert_eq!(snapshot.views.len(), 1);
        assert!(snapshot.views[0].is_active);
        assert!(snapshot.views[0].usage.is_some());
        assert_eq!(snapshot.last_poll_at, Some(now));
    }

    #[test]
    fn every_live_state_maps_to_the_panel_view() {
        let fixture = Fixture::new();
        let engine = fixture.engine();

        // Known.
        let views = engine.views(Utc::now()).expect("views");
        assert_eq!(
            snapshot_from_engine(&engine, views, None, None).live,
            LiveView::Known {
                account_id: fixture.account.id.clone()
            }
        );

        // Ended: the refresh token is empty, the identity still matches.
        fixture.write_live(credentials_json(""), identity_json(ACCOUNT_UUID, ORG_UUID));
        let views = engine.views(Utc::now()).expect("views");
        assert_eq!(
            snapshot_from_engine(&engine, views, None, None).live,
            LiveView::Ended {
                account_id: Some(fixture.account.id.clone())
            }
        );

        // Unknown: the identity belongs to no stored account.
        fixture.write_live(
            credentials_json(&format!("sk-ant-ort01-{}", "b".repeat(95))),
            identity_json(
                "8e1d5b7a-6c3f-4a2b-9d8e-1f2a3b4c5d6e",
                "5f2c8a1b-9d4e-4f6a-8b3c-7e2d1a9c4b5e",
            ),
        );
        let views = engine.views(Utc::now()).expect("views");
        assert_eq!(
            snapshot_from_engine(&engine, views, None, None).live,
            LiveView::Unknown {
                email: Some(EMAIL.to_string()),
                org_name: Some("Acme".to_string())
            }
        );

        // No login at all.
        fs::remove_file(&fixture.claude.credentials_file).expect("remove credentials");
        let views = engine.views(Utc::now()).expect("views");
        assert_eq!(
            snapshot_from_engine(&engine, views, None, None).live,
            LiveView::NoLogin
        );
    }

    #[test]
    fn pending_is_set_and_cleared() {
        let fixture = Fixture::new();
        let engine = fixture.engine();
        let mut snapshot = snapshot_from_engine(&engine, Vec::new(), None, None);
        assert!(snapshot.pending.is_none());

        let pending = PendingSwitch {
            target_id: "target".to_string(),
            target_label: "Private".to_string(),
            email: Some(EMAIL.to_string()),
            org_name: None,
        };
        snapshot.set_pending(pending.clone());
        assert_eq!(snapshot.pending, Some(pending));

        snapshot.clear_pending();
        assert!(snapshot.pending.is_none());
    }

    #[test]
    fn snapshot_json_has_the_contract_field_names() {
        let fixture = Fixture::new();
        let mut engine = fixture.engine();
        let now = Utc::now();
        let views = engine.poll(PollReason::Timer, now).expect("poll");
        let snapshot = snapshot_from_engine(&engine, views, Some("warning".to_string()), Some(now));
        let value = serde_json::to_value(&snapshot).expect("snapshot JSON");
        let object = value.as_object().expect("snapshot is an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "accounts",
                "activeId",
                "burn",
                "claudeConfigDir",
                "error",
                "lastPollAt",
                "live",
                "pending",
                "providers",
                "secretsWarning",
                "sessions",
                "settings",
                "views",
                "warnings",
            ]
        );
        assert_eq!(
            object["claudeConfigDir"],
            crate::test_support::tilde("claude")
        );
        assert_eq!(object["live"]["kind"], "known");
        assert_eq!(object["live"]["accountId"], fixture.account.id.as_str());
        assert_eq!(object["views"][0]["isActive"], true);
        assert!(object["settings"].get("trayText").is_some(), "{value}");
    }

    #[test]
    fn pending_survives_only_while_its_target_is_listed() {
        let fixture = Fixture::new();
        let engine = fixture.engine();
        let views = engine.views(Utc::now()).expect("views");
        let mut snapshot = snapshot_from_engine(&engine, views, None, None);
        let pending = PendingSwitch {
            target_id: fixture.account.id.clone(),
            target_label: fixture.account.label.clone(),
            email: Some(EMAIL.to_string()),
            org_name: None,
        };
        snapshot.set_pending(pending.clone());
        snapshot.retain_pending(Some(&pending));
        assert_eq!(snapshot.pending, Some(pending.clone()));

        // The target disappeared from the views: the pending must go too.
        snapshot.views.clear();
        snapshot.retain_pending(Some(&pending));
        assert!(snapshot.pending.is_none());
    }

    #[test]
    fn a_switch_notification_read_error_sends_the_switch_and_the_error() {
        let message = "Switched to AC — Acme";
        assert_eq!(
            switch_notices(message, Err("accounts.json is corrupt".to_string())),
            vec![
                SwitchNotice::Switched(message.to_string()),
                SwitchNotice::Error(
                    "the switch notification setting could not be read: accounts.json is corrupt"
                        .to_string()
                ),
            ]
        );
        assert_eq!(
            switch_notices(message, Ok(true)),
            vec![SwitchNotice::Switched(message.to_string())]
        );
        assert!(switch_notices(message, Ok(false)).is_empty());
    }

    #[test]
    fn commands_every_10_s_then_a_5_minute_jump_are_a_wake_at_the_next_command() {
        let wall = Utc::now();
        let monotonic = Instant::now();
        let mut baseline = ClockBaseline { wall, monotonic };

        // Commands every 10 s: the wall and monotonic clocks agree, no wake.
        for step in 1..=3 {
            let now = ClockBaseline {
                wall: wall + chrono::Duration::seconds(10 * step),
                monotonic: monotonic + Duration::from_secs(10 * step as u64),
            };
            let (next, wake) = wake_step(baseline, now);
            baseline = next;
            assert!(!wake, "a 10 s command gap is not a wake");
        }

        // The machine slept: 5 minutes of wall time, only 5 seconds monotonic.
        let now = ClockBaseline {
            wall: baseline.wall + chrono::Duration::seconds(300),
            monotonic: baseline.monotonic + Duration::from_secs(5),
        };
        let (next, wake) = wake_step(baseline, now);
        assert!(wake, "the 5-minute jump is a wake at the next command");
        assert_eq!(next.wall, now.wall);
        assert_eq!(next.monotonic, now.monotonic);
    }

    #[test]
    fn save_current_without_a_live_login_is_an_error_not_a_success() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.claude.credentials_file).expect("remove credentials");
        let (engine, _engine_rx) = mpsc::channel();
        let history = fixture.history();
        let mut action = ActionRuntime::new(
            fixture.app,
            fixture.claude,
            Vec::new(),
            engine,
            history,
            test_errors(),
            test_printed_warnings(),
        );
        match action.save_current(None) {
            ActionReply::Failed(message) => {
                assert!(message.contains("no live login"), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Turning auto-burn on or off writes `AutoBurnSet`, not the pause/resume
    /// event: a toggle must not end a manual switch's hold. The event a save
    /// writes comes from `settings_change_event`, so this needs no
    /// `save_setting` (which applies `startAtLogin`).
    #[test]
    fn a_toggle_writes_auto_burn_set() {
        let fixture = Fixture::new();
        let (engine, _engine_rx) = mpsc::channel();
        let history = fixture.history();
        let mut action = ActionRuntime::new(
            fixture.app.clone(),
            fixture.claude.clone(),
            Vec::new(),
            engine,
            history,
            test_errors(),
            test_printed_warnings(),
        );
        let off = Settings {
            auto_burn: false,
            ..Settings::default()
        };
        let on = Settings {
            auto_burn: true,
            ..off.clone()
        };
        let event = settings_change_event(&off, &on).expect("a toggle writes one event");
        assert_eq!(event, EventKind::AutoBurnSet { on: true });
        assert_eq!(
            serde_json::to_value(&event).expect("the event as JSON"),
            json!({"type": "autoBurnSet", "on": true})
        );
        assert_eq!(
            settings_change_event(&on, &on),
            None,
            "a save that leaves auto-burn alone writes no event"
        );
        let other = Settings {
            notify_on_switch: !on.notify_on_switch,
            ..on.clone()
        };
        assert_eq!(
            settings_change_event(&on, &other),
            None,
            "another setting is not auto-burn"
        );

        action.record(event).expect("the event is written");
        let host = seatswap_core::paths::host_name(None).expect("the host name");
        let history = HistoryDir::at(&fixture.app.history_dir(), &host);
        let now = Utc::now();
        let events = history
            .read::<Event>(
                &Range {
                    from: now - chrono::Duration::hours(1),
                    to: now + chrono::Duration::hours(1),
                },
                &Hosts::One(host),
            )
            .expect("the events are read");
        assert_eq!(events.records.len(), 1, "one event for one toggle");
        assert_eq!(
            events.records[0].record.kind,
            EventKind::AutoBurnSet { on: true }
        );
    }

    #[test]
    fn merge_setting_changes_one_field_and_keeps_the_others() {
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let merged = merge_setting(&settings, "currency", json!("EUR"), Path::new("/home/sam"))
            .expect("the currency is merged");
        assert_eq!(merged.currency.as_deref(), Some("EUR"));
        assert!(merged.auto_burn, "another writer's autoBurn is kept");
        assert_eq!(
            merged.poll_active_secs, settings.poll_active_secs,
            "every other field is kept"
        );
    }

    #[test]
    fn merge_setting_refuses_an_unknown_field() {
        let error = merge_setting(
            &Settings::default(),
            "noSuchSetting",
            json!(true),
            Path::new("/home/sam"),
        )
        .expect_err("an unknown field is refused");
        assert_eq!(error, "unknown setting noSuchSetting");
    }

    #[test]
    fn merge_setting_names_the_field_of_a_bad_value() {
        let error = merge_setting(
            &Settings::default(),
            "pollActiveSecs",
            json!("x"),
            Path::new("/home/sam"),
        )
        .expect_err("a string is not a poll interval");
        assert!(error.starts_with("pollActiveSecs: "), "{error}");
    }

    #[test]
    fn merge_setting_refuses_an_invalid_value() {
        let error = merge_setting(
            &Settings::default(),
            "currency",
            json!("eur"),
            Path::new("/home/sam"),
        )
        .expect_err("a currency is three capital letters");
        assert!(
            error.contains("currency must be three capital letters"),
            "{error}"
        );
    }

    #[test]
    fn merge_setting_expands_a_tilde_in_the_two_path_settings() {
        let home = crate::test_support::home();
        let history = merge_setting(
            &Settings::default(),
            "historyDir",
            json!(crate::test_support::tilde("hist")),
            home,
        )
        .expect("the history folder is a path");
        assert_eq!(history.history_dir, Some(home.join("hist")));
        let config = merge_setting(
            &Settings::default(),
            "claudeConfigDir",
            json!(crate::test_support::tilde("claude")),
            home,
        )
        .expect("the config folder is a path");
        assert_eq!(config.claude_config_dir, Some(home.join("claude")));
        // Only the path settings expand: the host name stays "~x" and is
        // refused by its own rule, exactly as it would be without this merge.
        let error = merge_setting(&Settings::default(), "hostName", json!("~x"), home)
            .expect_err("a host name may not start with ~");
        assert!(error.contains("host name \"~x\""), "{error}");
        assert!(
            !error.contains(&home.display().to_string()),
            "the host name is not a path: {error}"
        );
    }

    #[test]
    fn merge_setting_expands_a_tilde_in_the_codex_folder() {
        let home = crate::test_support::home();
        let merged = merge_setting(
            &Settings::default(),
            "codexHome",
            json!(crate::test_support::tilde("codex")),
            home,
        )
        .expect("the Codex folder is a path");
        assert_eq!(merged.codex_home, Some(home.join("codex")));
    }

    #[test]
    fn a_changed_price_is_written_to_the_history() {
        let fixture = Fixture::new();
        let (engine, _engine_rx) = mpsc::channel();
        let history = fixture.history();
        let mut action = ActionRuntime::new(
            fixture.app.clone(),
            fixture.claude.clone(),
            Vec::new(),
            engine,
            history,
            test_errors(),
            test_printed_warnings(),
        );
        let reply = action.edit(
            &fixture.account.id,
            None,
            None,
            None,
            None,
            Some(Some(180.0)),
        );
        assert!(matches!(reply, ActionReply::Done), "{reply:?}");

        let host = seatswap_core::paths::host_name(None).expect("the host name");
        let history = HistoryDir::at(&fixture.app.history_dir(), &host);
        let read = history
            .read::<Event>(
                &Range {
                    from: DateTime::<Utc>::MIN_UTC,
                    to: DateTime::<Utc>::MAX_UTC,
                },
                &Hosts::One(host),
            )
            .expect("the events are read");
        let prices: Vec<_> = read
            .records
            .iter()
            .filter(|hosted| matches!(hosted.record.kind, EventKind::PriceChanged { .. }))
            .collect();
        assert_eq!(prices.len(), 1, "one event for one price change");
        let value = serde_json::to_value(&prices[0].record.kind).expect("the event as JSON");
        assert_eq!(value["type"], "priceChanged");
        assert_eq!(value["amount"], json!(180.0));
        assert_eq!(
            value["currency"],
            json!("EUR"),
            "the currency setting at the time of the change"
        );
    }

    /// The mirror copies the live login into the stored account. When the plan
    /// of that account changes, the change belongs in the history, written by
    /// the code that records the events of this path.
    #[test]
    fn a_changed_plan_is_written_to_the_history_when_the_live_login_is_mirrored() {
        let fixture = Fixture::new();
        let store = Store::new(
            fixture.app.clone(),
            Box::new(FileSecrets::new(fixture.app.secrets_dir())),
        );
        // The stored plan is stale; the live login's tier says "Team 5x".
        let mut file = store.load().expect("accounts");
        file.accounts[0].plan = Some("Team".to_string());
        store.save(&file).expect("the stale plan is stored");

        let (engine, _engine_rx) = mpsc::channel();
        let history = fixture.history();
        let mut action = ActionRuntime::new(
            fixture.app.clone(),
            fixture.claude.clone(),
            Vec::new(),
            engine,
            history,
            test_errors(),
            test_printed_warnings(),
        );
        action.sync_live();

        assert_eq!(
            store.load().expect("accounts").accounts[0].plan.as_deref(),
            Some("Team 5x"),
            "the plan of the account is the login's"
        );

        let host = seatswap_core::paths::host_name(None).expect("the host name");
        let history = HistoryDir::at(&fixture.app.history_dir(), &host);
        let read = history
            .read::<Event>(
                &Range {
                    from: DateTime::<Utc>::MIN_UTC,
                    to: DateTime::<Utc>::MAX_UTC,
                },
                &Hosts::One(host),
            )
            .expect("the events are read");
        let changed: Vec<_> = read
            .records
            .iter()
            .filter(|hosted| matches!(hosted.record.kind, EventKind::PlanChanged { .. }))
            .collect();
        assert_eq!(changed.len(), 1, "one event for one plan change");
    }

    /// One event per real change: the same price again writes nothing, and
    /// clearing the price is a change of its own.
    #[test]
    fn an_unchanged_price_is_not_written_to_the_history() {
        let fixture = Fixture::new();
        let (engine, _engine_rx) = mpsc::channel();
        let history = fixture.history();
        let mut action = ActionRuntime::new(
            fixture.app.clone(),
            fixture.claude.clone(),
            Vec::new(),
            engine,
            history,
            test_errors(),
            test_printed_warnings(),
        );
        let host = seatswap_core::paths::host_name(None).expect("the host name");
        let history = HistoryDir::at(&fixture.app.history_dir(), &host);
        let prices = || {
            let read = history
                .read::<Event>(
                    &Range {
                        from: DateTime::<Utc>::MIN_UTC,
                        to: DateTime::<Utc>::MAX_UTC,
                    },
                    &Hosts::One(host.clone()),
                )
                .expect("the events are read");
            read.records
                .iter()
                .filter(|hosted| matches!(hosted.record.kind, EventKind::PriceChanged { .. }))
                .count()
        };

        assert!(matches!(
            action.edit(
                &fixture.account.id,
                None,
                None,
                None,
                None,
                Some(Some(180.0))
            ),
            ActionReply::Done
        ));
        assert_eq!(prices(), 1, "the first change is written");

        assert!(matches!(
            action.edit(
                &fixture.account.id,
                None,
                None,
                None,
                None,
                Some(Some(180.0))
            ),
            ActionReply::Done
        ));
        assert_eq!(prices(), 1, "the same price writes nothing");

        assert!(matches!(
            action.edit(&fixture.account.id, None, None, None, None, None),
            ActionReply::Done
        ));
        assert_eq!(prices(), 1, "a field that does not change writes nothing");

        assert!(matches!(
            action.edit(&fixture.account.id, None, None, None, None, Some(None)),
            ActionReply::Done
        ));
        assert_eq!(prices(), 2, "clearing the price is a change");
    }

    #[test]
    fn a_failed_autostart_says_the_settings_were_saved() {
        assert_eq!(
            autostart_failure_message("$HOME is empty"),
            "settings saved; start at login could not be set: $HOME is empty"
        );
    }

    #[test]
    fn an_unknown_pending_key_is_an_error_not_a_raw_pending() {
        let fixture = Fixture::new();
        let file = fixture.engine().store().load().expect("accounts");
        let live = LiveState::Unknown {
            email: Some(EMAIL.to_string()),
            org_name: Some("Acme".to_string()),
        };
        let pending =
            pending_switch(&file, &live, "AC", Some(EMAIL.to_string())).expect("known key");
        assert_eq!(pending.target_id, fixture.account.id);
        assert_eq!(pending.target_label, "Acme");
        assert_eq!(pending.org_name.as_deref(), Some("Acme"));

        let error = pending_switch(&file, &live, "ZZ", None).expect_err("unknown key");
        assert!(error.contains("unknown account ZZ"), "{error}");
    }

    #[test]
    fn an_invalid_account_colour_is_named_in_the_snapshot() {
        let fixture = Fixture::new();
        let store = Store::new(
            fixture.app.clone(),
            Box::new(FileSecrets::new(fixture.app.secrets_dir())),
        );
        let mut file = store.load().expect("accounts");
        file.accounts[0].color = "xyz".to_string();
        store.save(&file).expect("save the invalid colour");

        let (sender, _receiver) = mpsc::channel();
        let (action, engine_commands) = burn_channels();
        let history = fixture.history();
        let runtime = EngineRuntime::new(
            fixture.app,
            fixture.claude,
            sender,
            action,
            engine_commands,
            history,
            Arc::new(Mutex::new(None)),
            test_printed_warnings(),
        );
        let error = runtime
            .snapshot
            .error
            .expect("an invalid account colour is reported");
        assert_eq!(
            error,
            "account AC has an invalid colour \"xyz\"; set one in the account editor"
        );
    }

    #[test]
    fn a_corrupt_usage_cache_is_moved_aside_and_the_engine_reopens() {
        let fixture = Fixture::new();
        fs::write(fixture.app.usage_cache_file(), b"{ not json").expect("corrupt cache");

        let opened = open_engine(&fixture.app, &fixture.claude, &fixture.history())
            .expect("the engine reopens");
        let message = opened.notice.expect("a notice names the moved cache");
        assert!(message.contains("usage cache was unreadable"), "{message}");
        assert!(message.contains("usage is fetched again"), "{message}");
        assert!(opened.engine.views(Utc::now()).is_ok());
        assert!(
            !fixture.app.usage_cache_file().exists(),
            "the corrupt cache is renamed"
        );
        let aside = fs::read_dir(&fixture.app.data_dir)
            .expect("data dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("usage-cache.json.corrupt-")
            })
            .count();
        assert_eq!(aside, 1, "one cache was moved aside");
    }

    #[test]
    fn a_moved_cache_is_reported_in_the_first_snapshot() {
        let fixture = Fixture::new();
        fs::write(fixture.app.usage_cache_file(), b"{ not json").expect("corrupt cache");
        let (sender, _receiver) = mpsc::channel();
        let (action, engine_commands) = burn_channels();
        let history = fixture.history();
        let runtime = EngineRuntime::new(
            fixture.app,
            fixture.claude,
            sender,
            action,
            engine_commands,
            history,
            Arc::new(Mutex::new(None)),
            test_printed_warnings(),
        );
        assert!(runtime.engine.is_some(), "the engine reopened");
        let error = runtime
            .snapshot
            .error
            .expect("the notice is in the snapshot");
        assert!(error.contains("usage cache was unreadable"), "{error}");
        assert!(error.contains("usage is fetched again"), "{error}");
    }

    #[test]
    fn settings_come_from_accounts_json_even_when_the_engine_is_not_open() {
        let fixture = Fixture::new();
        let store = Store::new(
            fixture.app.clone(),
            Box::new(FileSecrets::new(fixture.app.secrets_dir())),
        );
        let mut file = store.load().expect("accounts");
        file.settings.poll_active_secs = 77;
        store.save(&file).expect("save settings");
        // A folder where the cache file should be makes Engine::new fail while
        // accounts.json stays readable.
        fs::create_dir(fixture.app.usage_cache_file()).expect("cache path is a folder");

        let (sender, _receiver) = mpsc::channel();
        let (action, engine_commands) = burn_channels();
        let history = fixture.history();
        let runtime = EngineRuntime::new(
            fixture.app,
            fixture.claude,
            sender,
            action,
            engine_commands,
            history,
            Arc::new(Mutex::new(None)),
            test_printed_warnings(),
        );
        assert!(runtime.engine.is_none(), "the engine could not open");
        assert_eq!(
            runtime
                .snapshot
                .settings
                .as_ref()
                .map(|settings| settings.poll_active_secs),
            Some(77),
            "the real settings are shown, not defaults"
        );
        assert!(
            runtime.snapshot.error.is_some(),
            "the engine failure is still reported"
        );
    }

    #[test]
    fn unreadable_settings_are_none_with_an_error_and_no_defaults() {
        let fixture = Fixture::new();
        fs::write(fixture.app.accounts_file(), b"{ not json").expect("corrupt accounts");
        let (sender, _receiver) = mpsc::channel();
        let (action, engine_commands) = burn_channels();
        let history = fixture.history();
        let runtime = EngineRuntime::new(
            fixture.app,
            fixture.claude,
            sender,
            action,
            engine_commands,
            history,
            Arc::new(Mutex::new(None)),
            test_printed_warnings(),
        );
        assert!(runtime.snapshot.settings.is_none());
        let error = runtime.snapshot.error.expect("an error is shown");
        assert!(error.contains("accounts.json"), "{error}");
    }

    #[test]
    fn commands_do_not_delay_the_timer_tick() {
        let start = Instant::now();
        let mut deadline = TickDeadline::new(start);
        assert_eq!(deadline.wait(start), TICK, "the first wait is one tick");

        let mut ticks = 0;
        // A command every two seconds: each one is handled, but none moves the
        // deadline, so the timer still runs at 30 s and 60 s.
        for seconds in (2..=70).step_by(2) {
            let now = start + Duration::from_secs(seconds);
            if deadline.due(now) {
                ticks += 1;
                deadline.advance(now);
            }
        }
        assert_eq!(ticks, 2, "two timer polls in seventy seconds");
    }

    #[test]
    fn wake_detection_ignores_89_s_and_reports_91_s() {
        let monotonic = Instant::now();
        let wall = Utc::now();
        let previous_monotonic = monotonic - Duration::from_secs(1);

        assert!(!wake_detected(
            wall - chrono::Duration::seconds(90),
            previous_monotonic,
            wall,
            monotonic
        ));
        assert!(wake_detected(
            wall - chrono::Duration::seconds(92),
            previous_monotonic,
            wall,
            monotonic
        ));
    }

    #[test]
    fn an_external_change_is_not_recorded_twice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = AccountRef {
            acc: ACCOUNT_UUID.to_string(),
            org: ORG_UUID.to_string(),
        };
        let now = Utc::now();

        // A CLI switch to the same account 5 s ago: the watcher must not write
        // a second event for it.
        let first = HistoryDir::at(&tmp.path().join("first"), "atlas");
        let recorder = Recorder::new(HistoryDir::at(first.root(), "atlas"));
        ops::record_switch(
            &recorder,
            None,
            account.clone(),
            None,
            By::Cli,
            None,
            None,
            None,
            now - chrono::Duration::seconds(5),
        )
        .expect("the CLI switch is written");
        assert_eq!(
            record_external_switch(
                &recorder,
                &Mutex::new(PrintedWarnings::new()),
                None,
                &account,
                None,
                now
            )
            .expect("the events can be read"),
            RecentSwitch::Already {
                by: By::Cli,
                cwd: None
            }
        );

        // With no such event, an external switch is written exactly once.
        let second = HistoryDir::at(&tmp.path().join("second"), "atlas");
        let recorder = Recorder::new(HistoryDir::at(second.root(), "atlas"));
        assert_eq!(
            record_external_switch(
                &recorder,
                &Mutex::new(PrintedWarnings::new()),
                None,
                &account,
                None,
                now
            )
            .expect("the external switch is written"),
            RecentSwitch::Written
        );
        let read = second
            .read::<Event>(
                &Range {
                    from: now - chrono::Duration::seconds(1),
                    to: now + chrono::Duration::seconds(1),
                },
                &Hosts::One("atlas".to_string()),
            )
            .expect("the events can be read");
        let switches: Vec<&EventKind> = read
            .records
            .iter()
            .map(|hosted| &hosted.record.kind)
            .filter(|kind| matches!(kind, EventKind::Switch { .. }))
            .collect();
        assert_eq!(switches.len(), 1);
        match switches[0] {
            EventKind::Switch { by, to, .. } => {
                assert_eq!(*by, By::External);
                assert_eq!(to, &account);
            }
            other => panic!("expected a switch, got {other:?}"),
        }
    }

    #[test]
    fn a_read_warning_is_printed_once_per_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = AccountRef {
            acc: ACCOUNT_UUID.to_string(),
            org: ORG_UUID.to_string(),
        };
        let now = Utc::now();
        let dir = HistoryDir::at(&tmp.path().join("history"), "atlas");
        let host_dir = dir.root().join("atlas");
        fs::create_dir_all(&host_dir).expect("history folder");
        // A torn last line: reading the month file warns about it every time.
        fs::write(
            host_dir.join(format!("events-{}.jsonl", now.format("%Y-%m"))),
            b"{\"t\":",
        )
        .expect("the torn event line");
        let recorder = Recorder::new(dir);
        let printed = Mutex::new(PrintedWarnings::new());

        record_external_switch(&recorder, &printed, None, &account, None, now)
            .expect("the events can be read");
        assert_eq!(
            super::super::lock(&printed).len(),
            1,
            "the torn line is one warning"
        );
        record_external_switch(&recorder, &printed, None, &account, None, now)
            .expect("the events can be read again");
        assert_eq!(
            super::super::lock(&printed).len(),
            1,
            "the same warning text is remembered, not printed again"
        );
    }

    #[test]
    fn an_agent_switch_is_announced_with_the_folder() {
        let home = crate::test_support::home();
        let folder = crate::test_support::at_home("projects/atlas")
            .display()
            .to_string();
        assert_eq!(
            agent_switch_message("PR", Some(&folder), home),
            format!(
                "An agent switched to PR (from {}).",
                crate::test_support::tilde("projects/atlas")
            )
        );
        assert_eq!(
            agent_switch_message("PR", None, home),
            "An agent switched to PR."
        );
    }

    /// A history that cannot be written is shown once. When the history writes
    /// again, the error is gone: it must not stay in every snapshot.
    #[test]
    fn a_history_error_is_cleared_when_the_history_writes_again() {
        let fixture = Fixture::new();
        let history_root = fixture.app.history_dir();
        let host_dir = history_root.join("atlas");
        fs::create_dir_all(&host_dir).expect("history folder");
        // A folder where the events file belongs: every append fails.
        let events = host_dir.join(format!("events-{}.jsonl", Utc::now().format("%Y-%m")));
        fs::create_dir_all(&events).expect("the blocked events file");

        let store = Store::new(
            fixture.app.clone(),
            Box::new(FileSecrets::new(fixture.app.secrets_dir())),
        );
        let engine = Engine::new(fixture.claude.clone(), store, Api::new())
            .expect("engine")
            .with_recorder(Recorder::new(HistoryDir::at(&history_root, "atlas")));
        let (ui, _rx) = mpsc::channel();
        let (action, engine_commands) = burn_channels();
        let mut runtime = EngineRuntime {
            app: fixture.app.clone(),
            claude: fixture.claude.clone(),
            ui,
            engine: Some(engine),
            history: fixture.history(),
            secrets_warning: None,
            open_error: None,
            history_error: None,
            live_written: false,
            notice: None,
            snapshot: Snapshot::empty(None, None, None),
            baseline: ClockBaseline::now(),
            burn: BurnRuntime::new(action, engine_commands),
            forecast: Arc::new(Mutex::new(None)),
            printed_warnings: test_printed_warnings(),
            tied_busy: 0,
            thinking: Vec::new(),
        };

        runtime.write_live_once();
        assert!(
            runtime.history_error.is_some(),
            "the failed write must be reported: {:?}",
            runtime.history_error
        );

        // The history works again: the error must be gone.
        fs::remove_dir(&events).expect("the blocked events file is gone");
        runtime.write_live_once();
        assert!(runtime.live_written, "the live event is written");
        assert!(
            runtime.history_error.is_none(),
            "the history error must be cleared when the history writes again: {:?}",
            runtime.history_error
        );
    }

    /// `publish()` writes nothing to the history, so it must not clear the
    /// engine thread's history error: the error stays until a write succeeds.
    #[test]
    fn a_heartbeat_error_survives_a_publish() {
        let fixture = Fixture::new();
        let history_root = fixture.app.history_dir();
        let host_dir = history_root.join("atlas");
        fs::create_dir_all(&host_dir).expect("history folder");
        // A folder where the up file belongs: every heartbeat write fails.
        let up = host_dir.join(format!("up-{}.jsonl", Utc::now().format("%Y-%m")));
        fs::create_dir_all(&up).expect("the blocked up file");

        let store = Store::new(
            fixture.app.clone(),
            Box::new(FileSecrets::new(fixture.app.secrets_dir())),
        );
        let engine = Engine::new(fixture.claude.clone(), store, Api::new())
            .expect("engine")
            .with_recorder(Recorder::new(HistoryDir::at(&history_root, "atlas")));
        let (ui, _rx) = mpsc::channel();
        let (action, engine_commands) = burn_channels();
        let mut runtime = EngineRuntime {
            app: fixture.app.clone(),
            claude: fixture.claude.clone(),
            ui,
            engine: Some(engine),
            history: fixture.history(),
            secrets_warning: None,
            open_error: None,
            history_error: None,
            live_written: false,
            notice: None,
            snapshot: Snapshot::empty(None, None, None),
            baseline: ClockBaseline::now(),
            burn: BurnRuntime::new(action, engine_commands),
            forecast: Arc::new(Mutex::new(None)),
            printed_warnings: test_printed_warnings(),
            tied_busy: 0,
            thinking: Vec::new(),
        };

        runtime.heartbeat(Utc::now());
        assert!(
            runtime
                .history_error
                .as_deref()
                .is_some_and(|text| text.starts_with("the history cannot be written:")),
            "the failed heartbeat must be reported: {:?}",
            runtime.history_error
        );

        runtime.publish();
        let error = runtime
            .snapshot
            .error
            .as_deref()
            .expect("the heartbeat error is shown");
        assert!(
            error.contains("the history cannot be written:"),
            "publish() must not clear the history error: {error}"
        );
    }

    /// A tick that computes no situation clears the kept forecast: the Forecast
    /// page must not show the plan of an older reading as if it were current.
    #[test]
    fn a_tick_without_a_situation_clears_the_forecast() {
        let fixture = Fixture::new();
        let file = store::load_accounts(&fixture.app).expect("accounts");
        let forecast = Arc::new(Mutex::new(Some((
            Inputs {
                now: Utc::now(),
                offset: chrono::FixedOffset::east_opt(0).expect("UTC"),
                accounts: Vec::new(),
                active: None,
                rhythm: seatswap_core::burn::Rhythm { wu: [0.0; 168] },
                last_switch_at: None,
                manual_hold_until: None,
            },
            Regime {
                early: false,
                why: seatswap_core::burn::decide::RegimeWhy::NoCostYet,
                lazy_blocked_h: 0.0,
                eager_blocked_h: 0.0,
                extra_switches: 0,
                cost_h: None,
            },
            Utc::now(),
        ))));
        let (ui, _rx) = mpsc::channel();
        let (action, engine_commands) = burn_channels();
        let mut runtime = EngineRuntime {
            app: fixture.app.clone(),
            claude: fixture.claude.clone(),
            ui,
            engine: None,
            history: fixture.history(),
            secrets_warning: None,
            open_error: None,
            history_error: None,
            live_written: false,
            notice: None,
            snapshot: Snapshot::empty(None, None, Some(file.settings)),
            baseline: ClockBaseline::now(),
            burn: BurnRuntime::new(action, engine_commands),
            forecast: Arc::clone(&forecast),
            printed_warnings: test_printed_warnings(),
            tied_busy: 0,
            thinking: Vec::new(),
        };

        // No account: the burn runtime has no situation to compute a plan from.
        runtime.update_burn(Some(Vec::new()));

        assert!(
            super::super::lock(&forecast).is_none(),
            "a tick without a situation must clear the forecast"
        );
    }
}
