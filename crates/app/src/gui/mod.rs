//! The tray app (docs/implementation.md "app: GUI").
//!
//! `run` starts Tauri, one engine thread, one action thread, the file watcher,
//! the single-instance socket and the tray. The main thread owns the windows and
//! the `Snapshot`; the worker threads never touch Tauri objects except through
//! the `UiEvent` channel.

#[cfg(feature = "gui")]
pub(crate) use crate::format;

pub(crate) mod ipc;
// `parse_target` is used by the CLI and the socket listener; the window itself
// is built only with the GUI.
pub(crate) mod window;

#[cfg(feature = "gui")]
pub(crate) mod autostart;
#[cfg(feature = "gui")]
pub(crate) mod burn_rt;
#[cfg(feature = "gui")]
pub(crate) mod lookup_rt;
#[cfg(feature = "gui")]
pub(crate) mod notify;
#[cfg(feature = "gui")]
pub(crate) mod panel;
#[cfg(feature = "gui")]
pub(crate) mod registry;
#[cfg(feature = "gui")]
pub(crate) mod sessions_rt;
#[cfg(feature = "gui")]
pub(crate) mod tray;
#[cfg(feature = "gui")]
pub(crate) mod worker;

#[cfg(feature = "gui")]
use std::collections::{HashMap, HashSet};
#[cfg(feature = "gui")]
use std::path::PathBuf;
#[cfg(feature = "gui")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "gui")]
use std::sync::mpsc;
#[cfg(feature = "gui")]
use std::sync::{Arc, Mutex, MutexGuard};
#[cfg(feature = "gui")]
use std::thread;

#[cfg(feature = "gui")]
use chrono::{DateTime, Local, Utc};

#[cfg(feature = "gui")]
use seatswap_core::history::By;
#[cfg(feature = "gui")]
use seatswap_core::paths::AppPaths;
#[cfg(feature = "gui")]
use seatswap_core::store;
#[cfg(feature = "gui")]
use seatswap_core::switcher::UnsavedPolicy;

#[cfg(feature = "gui")]
use notify::Notification;

#[cfg(feature = "gui")]
use tauri::{AppHandle, Emitter, Manager};

#[cfg(feature = "gui")]
use lookup_rt::{LookupClient, LookupHandle};
#[cfg(feature = "gui")]
use sessions_rt::{SessionsClient, SessionsHandle};
#[cfg(feature = "gui")]
use window::SessionsView;
#[cfg(feature = "gui")]
use worker::SessionsSummary;
#[cfg(feature = "gui")]
use worker::{ActionCommand, ActionReply, EngineCommand, PendingSwitch, Snapshot, UiEvent};

/// The one pending 15 s re-check: when it wakes and whether its thread runs.
#[cfg(feature = "gui")]
#[derive(Default)]
struct Recheck {
    at: Option<DateTime<Utc>>,
    running: bool,
}

/// Everything the Tauri commands and the tray share. The snapshot is updated
/// only on the main thread (from `UiEvent`s) or by a command that then emits it.
#[cfg(feature = "gui")]
pub(crate) struct GuiState {
    snapshot: Mutex<Snapshot>,
    /// The paths the app was started with, for the settings page's history and
    /// skill rows.
    pub(crate) app: AppPaths,
    pub(crate) claude: seatswap_core::paths::ClaudePaths,
    /// The history folder the recorder opened at startup, or why it could not
    /// be opened. A changed setting takes effect after a restart.
    pub(crate) history: Result<seatswap_core::history::HistoryDir, String>,
    /// The latest sessions summary; every snapshot is emitted with it.
    sessions: Mutex<Option<SessionsSummary>>,
    /// The sessions thread's commands; `None` while `readSessions` is off.
    pub(crate) sessions_client: Option<SessionsClient>,
    pub(crate) engine: std::sync::mpsc::Sender<EngineCommand>,
    pub(crate) action: std::sync::mpsc::Sender<ActionCommand>,
    /// The look-up thread's view, and the handle that keeps the thread alive.
    pub(crate) lookup: LookupClient,
    _lookup: Mutex<Option<LookupHandle>>,
    deduper: Mutex<notify::Deduper>,
    /// The waiting notifications that can still be withdrawn, by session id.
    waiting: Mutex<HashMap<String, notify::WaitingHandle>>,
    /// The one pending 15 s re-check for a session that has just started to
    /// wait (`notify::Deduper::recheck_at`).
    recheck: Mutex<Recheck>,
    notified_errors: Mutex<HashSet<String>>,
    /// Where the panel was opened, its last height and when focus loss last
    /// hid it (`panel::PanelState`).
    panel: Mutex<panel::PanelState>,
    tray: Mutex<Option<tauri::tray::TrayIcon>>,
    _watcher: Mutex<Option<seatswap_core::watcher::Watcher>>,
    _sessions: Mutex<Option<SessionsHandle>>,
    startup_error: Option<String>,
    /// A normal quit sets this before the engine thread stops, so the UI
    /// disconnect is not mistaken for a crash.
    exiting: AtomicBool,
}

#[cfg(feature = "gui")]
impl GuiState {
    pub(crate) fn snapshot(&self) -> Result<Snapshot, String> {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .map_err(|_| "the snapshot lock is poisoned".to_string())
    }

    /// A save that the action thread has confirmed: the snapshot holds the
    /// saved settings now, not when the engine's next `Publish` arrives. The
    /// settings page reads the saved values before its restart note is drawn.
    pub(crate) fn remember_settings(&self, settings: seatswap_core::store::Settings) {
        lock(&self.snapshot).settings = Some(settings);
    }

    /// The sessions thread's client; when `readSessions` is off, the one text
    /// every sessions command answers with.
    pub(crate) fn sessions_commands(&self) -> Result<SessionsClient, String> {
        self.sessions_client
            .clone()
            .ok_or_else(|| sessions_rt::SESSIONS_OFF.to_string())
    }
}

/// The one way a view learns the account in use: the snapshot's active view.
/// Used for the command's answer and for every pushed view.
#[cfg(feature = "gui")]
pub(crate) fn fill_in_use(view: &mut SessionsView, snapshot: &Snapshot) {
    view.in_use = snapshot
        .views
        .iter()
        .find(|account| account.is_active)
        .map(|account| account.id.clone());
}

#[cfg(feature = "gui")]
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What the person is told when a worker thread has ended: the tray must never
/// keep looking alive.
#[cfg(feature = "gui")]
pub(crate) const THREAD_STOPPED: &str = "seatswap stopped working: restart it";

#[cfg(feature = "gui")]
pub(crate) fn thread_stopped_message() -> &'static str {
    THREAD_STOPPED
}

/// A send to a worker thread whose receiver is gone. The closed channel is the
/// same sentence everywhere, never a silent return.
#[cfg(feature = "gui")]
pub(crate) fn send_to_worker<T>(result: Result<(), mpsc::SendError<T>>) -> Result<(), String> {
    result.map_err(|_| thread_stopped_message().to_string())
}

/// A reply from a worker thread whose sender is gone.
#[cfg(feature = "gui")]
pub(crate) fn worker_reply<T>(result: Result<T, mpsc::RecvError>) -> Result<T, String> {
    result.map_err(|_| thread_stopped_message().to_string())
}

/// The snapshot the tray shows after its UI channel closed: the one error that
/// explains why nothing will update again.
#[cfg(feature = "gui")]
fn stopped_snapshot(mut snapshot: Snapshot) -> Snapshot {
    snapshot.error = Some(thread_stopped_message().to_string());
    snapshot
}

/// Start the tray app. Returns the process exit code.
#[cfg(feature = "gui")]
pub(crate) fn run(
    app: AppPaths,
    claude_override: Option<PathBuf>,
    data_override: Option<PathBuf>,
) -> std::process::ExitCode {
    let result = tauri::Builder::default()
        .setup(move |tauri_app| {
            setup(
                tauri_app,
                app.clone(),
                claude_override.clone(),
                data_override.clone(),
            )
        })
        .invoke_handler(tauri::generate_handler![
            panel::get_snapshot,
            panel::refresh_now,
            panel::switch_account,
            panel::cancel_pending,
            panel::save_current,
            panel::edit_account,
            panel::remove_account,
            panel::save_setting,
            panel::set_auto_burn,
            panel::pause_burn,
            panel::money_view,
            panel::price_list,
            panel::set_price_override,
            panel::lookup_now,
            panel::icon_preview,
            panel::time_texts,
            panel::fit_panel,
            panel::hide_panel,
            panel::agent_calls,
            panel::skill_status,
            panel::skill_install,
            panel::projects_view,
            panel::projects_edit,
            panel::history_view,
            panel::history_import,
            panel::history_export,
            window::open_window,
            window::sessions_view,
            window::sessions_subscribe,
            window::sessions_unsubscribe,
        ])
        .build(tauri::generate_context!());
    match result {
        Ok(app) => {
            app.run(|handle, event| match event {
                // The tray app has no window of its own: closing the large
                // window (it is destroyed, not hidden) must leave the tray
                // running. A quit calls `app.exit(0)`, which carries a code.
                tauri::RunEvent::ExitRequested { code, api, .. } if keeps_running(code) => {
                    api.prevent_exit();
                }
                tauri::RunEvent::Exit => {
                    if let Some(state) = handle.try_state::<GuiState>() {
                        state.exiting.store(true, Ordering::SeqCst);
                        // A quit drops the waiting notifications; the process
                        // is ending, so nothing has to close them one by one.
                        lock(&state.waiting).clear();
                        let _ = state.engine.send(EngineCommand::Shutdown);
                    }
                }
                _ => {}
            });
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("seatswap: cannot start the tray app: {error}");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(feature = "gui")]
fn setup(
    tauri_app: &mut tauri::App,
    app: AppPaths,
    claude_override: Option<PathBuf>,
    data_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read the settings without opening the secret store: the main thread never
    // probes the keyring. A file that cannot be read is reported in the snapshot
    // and the tray keeps running.
    let (settings, settings_error) = match store::load_accounts(&app) {
        Ok(file) => (Some(file.settings), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let setting_dir = settings
        .as_ref()
        .and_then(|settings| settings.claude_config_dir.clone());

    let autostart_args =
        autostart::startup_arguments(data_override.as_deref(), claude_override.as_deref())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;

    let paths = crate::cli::resolve_paths(app, claude_override.as_deref(), setting_dir.as_deref())?;
    let (app, claude) = paths.into_parts();

    // Store errors are reported by the engine, which retries and can clear them;
    // watcher and single-instance problems cannot be retried and stay here.
    let mut startup_errors = Vec::new();
    if let Some(error) = settings_error {
        startup_errors.push(accounts_error(&error, &claude));
    }

    // Before the listener, the first place that can send a notification.
    #[cfg(windows)]
    if let Err(error) = notify::register_app_id() {
        startup_errors.push(format!("notification name: {error}"));
    }

    // Single instance: a held `gui.lock` takes the socket; a busy one sends
    // `open-panel main` to the instance that is already running and exits 0.
    let handle = tauri_app.handle().clone();
    let listener_action = handle.clone();
    // The listener can receive a feed before the worker exists; the slot is
    // filled as soon as it does, and a feed before that is simply dropped.
    let engine_slot: Arc<std::sync::OnceLock<mpsc::Sender<EngineCommand>>> =
        Arc::new(std::sync::OnceLock::new());
    let listener_engine = Arc::clone(&engine_slot);
    let listener = match ipc::Listener::start(&app, move |command| match command {
        ipc::IpcCommand::OpenPanel(view) => {
            let handle = listener_action.clone();
            let _ = listener_action.run_on_main_thread(move || {
                let _ = panel::show(&handle, &view);
            });
        }
        ipc::IpcCommand::OpenWindow(target) => {
            let handle = listener_action.clone();
            let _ = listener_action.run_on_main_thread(move || {
                show_window(&handle, &target);
            });
        }
        ipc::IpcCommand::Feed => {
            if let Some(engine) = listener_engine.get() {
                let _ = engine.send(EngineCommand::Feed);
            }
        }
    }) {
        Ok(listener) => Some(listener),
        Err(ipc::IpcError::AlreadyRunning) => {
            // Another instance holds the lock. Ask it to show the panel; if that
            // fails, say so and exit 1 instead of pretending it opened.
            match second_instance_outcome(ipc::send_open_panel(&app, "main")) {
                Ok(()) => handle.exit(0),
                Err(error) => {
                    eprintln!("seatswap: {error}");
                    handle.exit(1);
                }
            }
            return Ok(());
        }
        Err(error) => {
            // Never run without the single-instance lock: a second tray would
            // duplicate every notification and fight over the same files.
            let message = listener_start_failure(&error);
            eprintln!("seatswap: {message}");
            notify::send_error(&message);
            handle.exit(1);
            return Ok(());
        }
    };

    let (ui_tx, ui_rx) = mpsc::channel();
    let mut worker = worker::spawn(
        app.clone(),
        claude.clone(),
        autostart_args.clone(),
        ui_tx,
        settings.clone(),
    )?;
    let _ = engine_slot.set(worker.engine.clone());

    let watcher = {
        let action = worker.action.clone();
        match seatswap_core::watcher::watch(&claude, move || {
            let _ = action.send(ActionCommand::SyncLive);
        }) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                startup_errors.push(format!("file watcher: {error}"));
                None
            }
        }
    };

    if let Some(settings) = &settings {
        if let Err(error) = autostart::apply_autostart(settings.start_at_login, &autostart_args) {
            startup_errors.push(error);
        }
    }

    let startup_error = if startup_errors.is_empty() {
        None
    } else {
        Some(startup_errors.join("; "))
    };
    let initial = Snapshot::empty(startup_error.clone(), None, settings);
    let sessions = worker.sessions.take();
    let lookup_handle = worker
        .lookup
        .take()
        .ok_or_else(|| "the look-up thread did not start".to_string())?;
    let lookup = lookup_handle.client();
    let history = worker.history.clone();

    tauri_app.manage(GuiState {
        snapshot: Mutex::new(initial.clone()),
        app: app.clone(),
        claude: claude.clone(),
        history,
        sessions: Mutex::new(None),
        sessions_client: sessions.as_ref().map(SessionsHandle::client),
        engine: worker.engine.clone(),
        action: worker.action.clone(),
        lookup,
        _lookup: Mutex::new(Some(lookup_handle)),
        deduper: Mutex::new(notify::Deduper::new()),
        waiting: Mutex::new(HashMap::new()),
        recheck: Mutex::new(Recheck::default()),
        notified_errors: Mutex::new(HashSet::new()),
        panel: Mutex::new(panel::PanelState::new()),
        tray: Mutex::new(None),
        _watcher: Mutex::new(watcher),
        _sessions: Mutex::new(sessions),
        startup_error,
        exiting: AtomicBool::new(false),
    });
    if let Some(listener) = listener {
        tauri_app.manage(ListenerHolder {
            _listener: Mutex::new(Some(listener)),
        });
    }

    tray::start(tauri_app.handle());

    // Build the (hidden) panel now, so its page has loaded and sized itself
    // before the first opening; otherwise that opening shows a white window
    // that then shrinks and grows.
    panel::preload(tauri_app.handle())?;

    // The page is not loaded yet; the engine's first snapshot follows shortly.
    let _ = handle.emit("snapshot", &initial);

    let event_handle = tauri_app.handle().clone();
    thread::Builder::new()
        .name("seatswap-ui".to_string())
        .spawn(move || {
            while let Ok(event) = ui_rx.recv() {
                let handle = event_handle.clone();
                let _ = event_handle.run_on_main_thread(move || {
                    handle_ui_event(&handle, event);
                });
            }
            // Every sender is gone: the engine thread ended (normally it runs
            // until shutdown) and no new snapshot will arrive. Say so instead
            // of leaving the tray looking alive.
            let handle = event_handle.clone();
            let _ = event_handle.run_on_main_thread(move || {
                if let Some(state) = handle.try_state::<GuiState>() {
                    if state.exiting.load(Ordering::SeqCst) {
                        return;
                    }
                    let snapshot = stopped_snapshot(lock(&state.snapshot).clone());
                    apply_snapshot(&handle, snapshot);
                }
            });
        })?;

    Ok(())
}

/// The visible startup message when accounts.json cannot be read: the error
/// plus the Claude folder the tray is using meanwhile.
#[cfg(feature = "gui")]
fn accounts_error(error: &str, claude: &seatswap_core::paths::ClaudePaths) -> String {
    format!(
        "cannot read accounts.json ({error}); using Claude config folder {}",
        claude.config_dir.display()
    )
}

/// The exit code and message of a second instance. A successful send exits 0;
/// a failed one is a visible error and exit 1.
#[cfg(feature = "gui")]
fn second_instance_outcome(result: Result<(), ipc::IpcError>) -> Result<(), String> {
    result.map_err(|error| error.to_string())
}

/// The visible text for a listener that failed for any reason other than
/// "already running": the app must stop, because it cannot take the lock.
#[cfg(feature = "gui")]
fn listener_start_failure(error: &ipc::IpcError) -> String {
    format!("cannot start the single-instance listener: {error}")
}

/// Owns the socket listener so it lives as long as the app.
#[cfg(feature = "gui")]
struct ListenerHolder {
    _listener: Mutex<Option<ipc::Listener>>,
}

#[cfg(feature = "gui")]
fn handle_ui_event(app: &AppHandle, event: UiEvent) {
    match event {
        UiEvent::Snapshot(snapshot) => apply_snapshot(app, snapshot),
        UiEvent::Sessions(summary) => {
            let state = app.state::<GuiState>();
            *lock(&state.sessions) = Some(summary);
            let snapshot = lock(&state.snapshot).clone();
            apply_snapshot(app, snapshot);
        }
        UiEvent::Money(view) => {
            let _ = app.emit("money", &view);
        }
        UiEvent::SessionsView(mut view) => {
            let state = app.state::<GuiState>();
            let snapshot = lock(&state.snapshot).clone();
            fill_in_use(&mut view, &snapshot);
            let _ = app.emit_to(window::WINDOW_LABEL, "sessions-view", &view);
        }
    }
}

#[cfg(feature = "gui")]
fn apply_snapshot(app: &AppHandle, mut snapshot: Snapshot) {
    let state = app.state::<GuiState>();
    snapshot.sessions = lock(&state.sessions).clone();
    if let Some(startup) = &state.startup_error {
        snapshot.error = Some(match snapshot.error.take() {
            Some(engine) => format!("{engine}; {startup}"),
            None => startup.clone(),
        });
    }
    let now = Utc::now();
    let (due, recheck) = {
        let mut current = lock(&state.snapshot);
        snapshot.retain_pending(current.pending.as_ref());
        let mut deduper = lock(&state.deduper);
        let due = deduper.due(&snapshot, now);
        let recheck = deduper.recheck_at(&snapshot, now);
        drop(deduper);
        *current = snapshot.clone();
        (due, recheck)
    };

    let _ = app.emit("snapshot", &snapshot);
    if let Some(tray) = lock(&state.tray).as_ref() {
        tray::update(tray, &snapshot);
    }

    if let Some(error) = &snapshot.error {
        let new = {
            let mut seen = lock(&state.notified_errors);
            seen.insert(error.clone())
        };
        if new {
            eprintln!("seatswap: {error}");
            notify::send_error(error);
        }
    }

    for notification in due {
        match notification {
            Notification::Suggestion(suggestion) => {
                notify::send_suggestion(app.clone(), &suggestion);
            }
            Notification::LoginWarning(warning) => {
                let id = notify::warning_id(&warning).to_string();
                let name = view_name(&snapshot, &id);
                notify::send_login_warning(&warning, &name);
            }
            Notification::UnknownLogin { email, org_name } => {
                notify::send_unknown_login(app.clone(), email.as_deref(), org_name.as_deref());
            }
            Notification::UsableAgain { id, reason } => {
                let abbr = view_abbr(&snapshot, &id);
                let switch_app = app.clone();
                let switch_id = id.clone();
                let on_switch = move || {
                    request_switch(&switch_app, switch_id, UnsavedPolicy::Refuse, None);
                };
                let open_app = app.clone();
                let on_open = move || {
                    let handle = open_app.clone();
                    let _ = open_app.run_on_main_thread(move || show_panel(&handle, "main"));
                };
                notify::send_usable_again(&abbr, &reason, on_switch, on_open);
            }
            Notification::AllFull { next } => match next {
                Some((id, at)) => {
                    let abbr = view_abbr(&snapshot, &id);
                    let time = format::format_reset_clock(at, Utc::now(), &Local);
                    notify::send_all_full(Some((&abbr, &time)));
                }
                None => notify::send_all_full(None),
            },
            Notification::Waiting(waiting) => {
                let this_host = state
                    .history
                    .as_ref()
                    .ok()
                    .map(|history| history.host().to_string());
                let open_app = app.clone();
                let session = waiting.session.clone();
                let on_open = move || {
                    let handle = open_app.clone();
                    let target = format!("sessions?{session}");
                    let _ = open_app.run_on_main_thread(move || show_window(&handle, &target));
                };
                let handle = notify::send_waiting(&waiting, this_host.as_deref(), on_open);
                let previous = lock(&state.waiting).insert(waiting.session.clone(), handle);
                if let Some(previous) = previous {
                    previous.withdraw();
                }
            }
            Notification::WaitingOver { session } => {
                let handle = lock(&state.waiting).remove(&session);
                if let Some(handle) = handle {
                    handle.withdraw();
                }
            }
        }
    }
    schedule_recheck(app, recheck);
}

/// Keep at most one thread for the next 15 s re-check. A newer moment replaces
/// the pending one's purpose: the thread that wakes for nothing finds nothing
/// due and stops.
#[cfg(feature = "gui")]
fn schedule_recheck(app: &AppHandle, at: Option<DateTime<Utc>>) {
    let Some(at) = at else {
        return;
    };
    let state = app.state::<GuiState>();
    let start = {
        let mut recheck = lock(&state.recheck);
        recheck.at = Some(at);
        if recheck.running {
            false
        } else {
            recheck.running = true;
            true
        }
    };
    if !start {
        return;
    }
    let handle = app.clone();
    let spawned = thread::Builder::new()
        .name("seatswap-recheck".to_string())
        .spawn(move || loop {
            let Some(state) = handle.try_state::<GuiState>() else {
                return;
            };
            let target = {
                let recheck = lock(&state.recheck);
                match recheck.at {
                    Some(target) => target,
                    None => {
                        drop(recheck);
                        let mut recheck = lock(&state.recheck);
                        recheck.running = false;
                        return;
                    }
                }
            };
            let now = Utc::now();
            if target <= now {
                apply_recheck_now(&handle);
                return;
            }
            let wait = match (target - now).to_std() {
                Ok(wait) => wait,
                Err(_) => {
                    // The target is not in the future (the clock moved past it
                    // after the read above): apply the snapshot at once, as
                    // the branch above does.
                    apply_recheck_now(&handle);
                    return;
                }
            };
            thread::sleep(wait);
        });
    if let Err(error) = spawned {
        eprintln!("seatswap: re-check thread: {error}");
        let state = app.state::<GuiState>();
        let mut recheck = lock(&state.recheck);
        recheck.at = None;
        recheck.running = false;
    }
}

/// Apply the stored snapshot again because a pending 15 s re-check is due.
#[cfg(feature = "gui")]
fn apply_recheck_now(handle: &AppHandle) {
    let Some(state) = handle.try_state::<GuiState>() else {
        return;
    };
    {
        let mut recheck = lock(&state.recheck);
        recheck.at = None;
        recheck.running = false;
    }
    let snapshot = lock(&state.snapshot).clone();
    let wake = handle.clone();
    let _ = handle.run_on_main_thread(move || apply_snapshot(&wake, snapshot));
}

#[cfg(feature = "gui")]
fn view_abbr(snapshot: &Snapshot, id: &str) -> String {
    snapshot
        .views
        .iter()
        .find(|view| view.id == id)
        .map(|view| view.abbr.clone())
        .unwrap_or_else(|| id.to_string())
}

#[cfg(feature = "gui")]
fn view_name(snapshot: &Snapshot, id: &str) -> String {
    snapshot
        .views
        .iter()
        .find(|view| view.id == id)
        .map(|view| format!("{} — {}", view.abbr, view.label))
        .unwrap_or_else(|| id.to_string())
}

#[cfg(feature = "gui")]
pub(crate) fn show_panel(app: &AppHandle, view: &str) {
    if let Err(error) = panel::show(app, view) {
        eprintln!("seatswap: panel: {error}");
    }
}

#[cfg(feature = "gui")]
pub(crate) fn show_window(app: &AppHandle, target: &str) {
    if let Err(error) = window::show(app, target) {
        eprintln!("seatswap: window: {error}");
    }
}

/// Whether the app keeps running after a window asked to exit. Closing the
/// large window sends `code: None`; `app.exit(0)` from the tray's quit carries
/// a code and ends the app.
#[cfg(feature = "gui")]
fn keeps_running(code: Option<i32>) -> bool {
    code.is_none()
}

#[cfg(feature = "gui")]
pub(crate) fn set_pending(
    app: &AppHandle,
    state: &GuiState,
    pending: PendingSwitch,
) -> Result<(), String> {
    let snapshot = {
        let mut current = lock(&state.snapshot);
        current.set_pending(pending);
        current.clone()
    };
    let _ = app.emit("snapshot", &snapshot);
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_panel(&handle, "main"));
    Ok(())
}

#[cfg(feature = "gui")]
pub(crate) fn clear_pending(app: &AppHandle, state: &GuiState) -> Result<(), String> {
    let snapshot = {
        let mut current = lock(&state.snapshot);
        current.clear_pending();
        current.clone()
    };
    let _ = app.emit("snapshot", &snapshot);
    Ok(())
}

/// A switch started from the tray menu or a notification. The panel is opened
/// only when the live login must be saved or discarded first.
#[cfg(feature = "gui")]
pub(crate) fn request_switch(
    app: &AppHandle,
    key: String,
    unsaved: seatswap_core::switcher::UnsavedPolicy,
    label: Option<String>,
) {
    let state = app.state::<GuiState>();
    let action = state.action.clone();
    let app = app.clone();
    thread::spawn(move || {
        let (reply, received) = mpsc::channel();
        if let Err(error) = send_to_worker(action.send(ActionCommand::Switch {
            key,
            unsaved,
            label,
            by: By::User,
            reason: None,
            reply,
        })) {
            notify::send_error(&error);
            return;
        }
        match worker_reply(received.recv()) {
            Ok(ActionReply::NotSaved(pending)) => {
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(state) = handle.try_state::<GuiState>() {
                        let _ = set_pending(&handle, &state, pending);
                    }
                });
            }
            Ok(_) => {}
            Err(error) => notify::send_error(&error),
        }
    });
}

#[cfg(all(test, feature = "gui"))]
mod tests {
    use super::*;

    use std::path::Path;

    use seatswap_core::paths::ClaudePaths;

    #[test]
    fn closing_a_window_keeps_the_app_running() {
        assert!(
            keeps_running(None),
            "closing a window must not end the tray app"
        );
        assert!(!keeps_running(Some(0)), "Quit seatswap still ends the app");
    }

    #[test]
    fn a_stopped_worker_reports_one_sentence_and_a_closed_channel_is_it() {
        assert_eq!(
            thread_stopped_message(),
            "seatswap stopped working: restart it"
        );

        let (sender, receiver) = mpsc::channel::<()>();
        drop(receiver);
        assert_eq!(
            send_to_worker(sender.send(())),
            Err("seatswap stopped working: restart it".to_string())
        );

        let (sender, receiver) = mpsc::channel::<()>();
        drop(sender);
        assert_eq!(
            worker_reply(receiver.recv()),
            Err("seatswap stopped working: restart it".to_string())
        );
    }

    #[test]
    fn a_disconnected_ui_thread_puts_the_sentence_in_the_snapshot() {
        let snapshot = Snapshot::empty(None, None, None);
        assert_eq!(
            stopped_snapshot(snapshot).error.as_deref(),
            Some("seatswap stopped working: restart it")
        );
    }

    #[test]
    fn an_unreadable_accounts_file_names_the_claude_folder_in_use() {
        let claude =
            ClaudePaths::for_config_dir(Path::new("/home/u/.claude"), Path::new("/home/u"));
        let message = accounts_error("accounts.json is not valid JSON", &claude);
        assert!(
            message.contains("accounts.json is not valid JSON"),
            "{message}"
        );
        assert!(
            message.contains("using Claude config folder /home/u/.claude"),
            "{message}"
        );
    }
    #[test]
    fn a_second_instance_that_cannot_reach_the_first_is_an_error() {
        assert_eq!(second_instance_outcome(Ok(())), Ok(()));
        assert_eq!(
            second_instance_outcome(Err(ipc::IpcError::NotRunning)),
            Err("seatswap is not running".to_string())
        );
    }

    #[test]
    fn a_listener_failure_is_reported_with_its_cause() {
        let error = ipc::IpcError::Io {
            path: std::path::PathBuf::from("/run/seatswap.sock"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        let message = listener_start_failure(&error);
        assert!(
            message.contains("cannot start the single-instance listener"),
            "{message}"
        );
        assert!(message.contains("/run/seatswap.sock"), "{message}");
    }

    #[test]
    fn startup_settings_come_from_accounts_json_or_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = AppPaths::at(&tmp.path().join("data"));
        std::fs::create_dir_all(&app.data_dir).expect("data folder");

        // Missing accounts.json is an empty list with default settings, so it is
        // readable and the real (default) settings are shown.
        let (settings, error) = match store::load_accounts(&app) {
            Ok(file) => (Some(file.settings), None),
            Err(error) => (None, Some(error.to_string())),
        };
        assert!(settings.is_some());
        assert!(error.is_none());

        // A corrupt file is None plus an error, never defaults.
        std::fs::write(app.accounts_file(), b"{not json").expect("corrupt accounts");
        let (settings, error) = match store::load_accounts(&app) {
            Ok(file) => (Some(file.settings), None),
            Err(error) => (None, Some(error.to_string())),
        };
        assert!(settings.is_none());
        assert!(error.is_some());
    }
}
