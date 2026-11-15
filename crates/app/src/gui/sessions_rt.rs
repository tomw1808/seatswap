//! The sessions thread: Claude Code's running sessions and the transcript
//! scanner (spec §1.6).
//!
//! It runs only when `readSessions` is on. Every wake it asks which sessions are
//! alive, reads the new parts of their transcripts, writes the tokens and the
//! session records to the history, then hands a [`SessionsSummary`] to the main
//! thread and the models in use to the engine thread.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use seatswap_core::history::{EventKind, HistoryDir, Range, Recorder, SessionRecord};
use seatswap_core::live_sessions::{self, LiveSession, LiveStatus, OtherHost, Rows};
use seatswap_core::money::{Fx, Prices, Valuer};
use seatswap_core::paths::{AppPaths, ClaudePaths};
use seatswap_core::project::{self, ProjectKey};
use seatswap_core::projects;
use seatswap_core::sessions::{self, Liveness, SessionFile, SessionStatus, SystemProbe};
use seatswap_core::store::{self, Settings};
use seatswap_core::transcripts::{self, RecordedSession, ScanState, SessionSeen};
use seatswap_core::watcher::{watch_folder, FolderWatch, WatchError};

use super::window::{view_of, ClosedSince, SessionsOptions, SessionsView};
use super::worker::{EngineCommand, SessionsSummary, UiEvent, Waiting};
use crate::ops;

/// What the three sessions commands say when `readSessions` is off. The label
/// is the one the Settings page gives the toggle; a restart is needed because
/// the thread starts only with the app.
pub(crate) const SESSIONS_OFF: &str =
    "reading sessions is off: turn on \"Read Claude Code's session files\" in Settings → General and restart seatswap";

/// The regular wake, so a session folder that cannot be watched still works.
const WAKE: Duration = Duration::from_secs(60);
/// After a scan that did not finish, read the next piece soon.
const AFTER_INCOMPLETE: Duration = Duration::from_secs(1);
/// One transcript scan reads at most this much.
const SCAN_BUDGET: u64 = 64 * 1024 * 1024;
/// At most one pushed view in this window; one inside it follows when it is
/// over, so the last view always holds the final numbers (spec §3.8).
const VIEW_EVERY_MS: i64 = 500;
/// The closed rows are read again at least this often (the minute pass).
const CLOSED_EVERY_SECS: i64 = 60;
/// A closed choice only a command asked for is dropped this long after its last
/// ask, so a page that changes its mind often does not grow the kept rows.
const CLOSED_KEEP_SECS: i64 = 300;
/// A session record is rewritten at most this often while its summary changes.
const RECORD_EVERY_SECS: i64 = 10 * 60;
/// The projects, values and settings the rows are built with are at most this
/// old.
const ROWS_CONTEXT_MAX_AGE_SECS: i64 = 300;
/// A part of the rows context that could not be built is tried again after
/// this many seconds, not on every pass.
const ROWS_CONTEXT_RETRY_SECS: i64 = 60;
/// This host reports even when nothing changed after this many seconds, so
/// another host does not take a quiet host for a stale one.
const REPORT_HEARTBEAT_SECS: i64 = 300;

/// What one pass found, for the thread and the tests.
#[derive(Debug, Clone)]
pub(crate) struct PassOutcome {
    pub summary: SessionsSummary,
    pub models: Option<BTreeSet<String>>,
    pub tied_busy: u32,
    pub wake_soon: bool,
}

/// What the window sends to the sessions thread. `Wake` is the folder
/// watchers' plain wake; the other commands are the three Tauri commands.
enum SessionsCommand {
    View {
        options: SessionsOptions,
        reply: Sender<Result<SessionsView, String>>,
    },
    Subscribe(SessionsOptions),
    Unsubscribe,
    Stop,
    Wake,
}

/// The commands share with the thread, like the look-up's client.
#[derive(Clone)]
pub(crate) struct SessionsClient {
    commands: Sender<SessionsCommand>,
}

impl SessionsClient {
    pub(crate) fn view(&self, options: SessionsOptions) -> Result<SessionsView, String> {
        let (reply, received) = mpsc::channel();
        super::send_to_worker(self.commands.send(SessionsCommand::View { options, reply }))?;
        super::worker_reply(received.recv())?
    }

    pub(crate) fn subscribe(&self, options: SessionsOptions) -> Result<(), String> {
        super::send_to_worker(self.commands.send(SessionsCommand::Subscribe(options)))
    }

    pub(crate) fn unsubscribe(&self) -> Result<(), String> {
        super::send_to_worker(self.commands.send(SessionsCommand::Unsubscribe))
    }
}

/// At most one view per [`VIEW_EVERY_MS`], with a trailing one. The time is an
/// argument, so a test drives it without sleeping.
#[derive(Debug, Default)]
struct ViewThrottle {
    /// The last view sent.
    last_sent: Option<DateTime<Utc>>,
    /// A view was wanted inside the window and has to follow when it is over.
    pending: bool,
}

impl ViewThrottle {
    /// Whether a view is due now: a change or the minute pass, but never within
    /// [`VIEW_EVERY_MS`] of the last one. A wanted view inside the window is
    /// remembered and sent by the next pass after it.
    fn due(&mut self, now: DateTime<Utc>, changed: bool) -> bool {
        let minute = is_due(self.last_sent, now, 60);
        let wanted = changed || minute || self.pending;
        if !wanted {
            return false;
        }
        if self.last_sent.is_some_and(|last| {
            now >= last && now.signed_duration_since(last).num_milliseconds() < VIEW_EVERY_MS
        }) {
            self.pending = true;
            return false;
        }
        self.pending = false;
        self.last_sent = Some(now);
        true
    }

    /// How long until a pending view may be sent; `None` when none is pending
    /// (a clock that moved back makes it due now).
    fn wait(&self, now: DateTime<Utc>) -> Option<Duration> {
        if !self.pending {
            return None;
        }
        let last = self.last_sent?;
        let until = last + ChronoDuration::milliseconds(VIEW_EVERY_MS);
        let wait = until.signed_duration_since(now);
        if wait <= ChronoDuration::zero() {
            return Some(Duration::ZERO);
        }
        u64::try_from(wait.num_milliseconds())
            .ok()
            .map(Duration::from_millis)
    }
}

/// The projects and the pricing the rows are built with. They are built and
/// rebuilt apart from each other: a failing one does not keep the other from
/// being built, and the rows are built with what is there.
#[derive(Default)]
struct RowsContext {
    projects: Option<ProjectsBuilt>,
    /// Why the projects could not be built, and when that was tried.
    projects_failure: Option<ContextFailure>,
    pricing: Option<PricingBuilt>,
    /// Why the pricing could not be built, and when that was tried.
    pricing_failure: Option<ContextFailure>,
    /// The settings' currency from the accounts file, read whether or not the
    /// prices could be read.
    currency: Option<String>,
}

/// The closed rows read for one `ClosedSince`, with when they were read, when
/// that choice was last asked for and why their values are missing.
struct ClosedRows {
    rows: Rows,
    read_at: DateTime<Utc>,
    asked_at: DateTime<Utc>,
    failure: Option<String>,
}

struct ProjectsBuilt {
    projects: projects::Projects,
    /// Warnings from the project and session reads.
    warnings: Vec<String>,
    built: DateTime<Utc>,
}

struct PricingBuilt {
    valuer: Valuer,
    settings: Settings,
    built: DateTime<Utc>,
}

struct ContextFailure {
    text: String,
    tried: DateTime<Utc>,
}

/// What the thread remembers between two passes.
pub(crate) struct SessionMemory {
    home: std::path::PathBuf,
    projects: HashMap<String, ProjectKey>,
    waiting: HashMap<String, WaitingMemory>,
    /// The last session record written for each session.
    records: HashMap<String, SessionRecord>,
    /// Sessions whose one closing record has been written and whose entry is
    /// already pruned from the [`ScanState`], so `SessionSeen.recorded` cannot
    /// remember them. A session still in the state uses `recorded`, never this.
    closed: HashSet<String>,
    rows_context: Option<RowsContext>,
    /// The rows of the last report written successfully.
    last_rows: Option<Vec<LiveSession>>,
    /// When this host's report was last written successfully.
    last_report_at: Option<DateTime<Utc>>,
    /// The running sessions of the last pass that could read every file, so a
    /// session that left the list is reported at once.
    running_ids: BTreeSet<String>,
    /// The other hosts' reports of the last read, used on every pass until the
    /// next read so no wait or warning flickers.
    other_hosts: Option<live_sessions::OtherHosts>,
    /// When the other hosts' reports were last read.
    last_other_hosts_at: Option<DateTime<Utc>>,
    /// Why there is no history, when the thread started without one: the rows
    /// then say why their values are missing.
    history_error: Option<String>,
    /// The rows of the last pass, which the view is built from.
    rows: Option<Rows>,
    /// Each running session's answers with thinking since the user's last turn.
    thinking: BTreeMap<String, u32>,
    /// The last summary's `unavailable`, shown by the view.
    unavailable: Option<String>,
    /// The settings' currency the rows were valued with.
    currency: Option<String>,
    /// The history's host; `None` when the thread runs without a history.
    this_host: Option<String>,
    /// The last pass's time, the view's `at`.
    at: Option<DateTime<Utc>>,
    /// The closed rows the view may ask for, each with the choice it was read
    /// for; a choice is kept for [`CLOSED_KEEP_SECS`] after its last ask.
    closed_rows: BTreeMap<ClosedSince, ClosedRows>,
    /// The subscription's options; `None` while nothing is subscribed. A
    /// command's options are never kept here.
    subscription: Option<SessionsOptions>,
    /// Where pushed views go.
    ui: Option<Sender<UiEvent>>,
    /// The last pushed view, so a pass that changed nothing is not pushed.
    last_pushed: Option<SessionsView>,
    throttle: ViewThrottle,
    /// The transcript folders of the running sessions and their agents, from
    /// the keys the `ScanState` knows.
    transcript_folders: BTreeSet<PathBuf>,
}

impl SessionMemory {
    pub(crate) fn new(home: std::path::PathBuf) -> SessionMemory {
        SessionMemory {
            home,
            projects: HashMap::new(),
            waiting: HashMap::new(),
            records: HashMap::new(),
            closed: HashSet::new(),
            rows_context: None,
            last_rows: None,
            last_report_at: None,
            running_ids: BTreeSet::new(),
            other_hosts: None,
            last_other_hosts_at: None,
            history_error: None,
            rows: None,
            thinking: BTreeMap::new(),
            unavailable: None,
            currency: None,
            this_host: None,
            at: None,
            closed_rows: BTreeMap::new(),
            subscription: None,
            ui: None,
            last_pushed: None,
            throttle: ViewThrottle::default(),
            transcript_folders: BTreeSet::new(),
        }
    }

    /// Start pushing views for `options`: the next pass sends the first one.
    fn subscribe(&mut self, options: SessionsOptions) {
        self.subscription = Some(options);
        self.last_pushed = None;
        self.throttle = ViewThrottle::default();
    }

    /// Stop pushing views. The caller drops the transcript folder watches.
    fn unsubscribe(&mut self) {
        self.subscription = None;
        self.last_pushed = None;
        self.throttle = ViewThrottle::default();
        self.transcript_folders.clear();
    }

    /// The view the subscription pushes, or `None` when nothing is subscribed
    /// or before the first pass.
    fn view(&self) -> Option<SessionsView> {
        let options = self.subscription.as_ref()?;
        self.view_for(options)
    }

    /// The view for `options`, from what the thread holds. The command's answer
    /// and the pushed view both come through here; a command's options never
    /// replace the subscription's.
    fn view_for(&self, options: &SessionsOptions) -> Option<SessionsView> {
        let at = self.at?;
        let rows = self.rows.as_ref()?;
        let other: &[OtherHost] = self
            .other_hosts
            .as_ref()
            .map(|hosts| hosts.hosts.as_slice())
            .unwrap_or(&[]);
        let closed = self.closed_rows.get(&options.closed);
        let closed_sessions = closed
            .map(|read| read.rows.sessions.as_slice())
            .unwrap_or(&[]);
        let closed_failure = closed.and_then(|read| read.failure.as_deref());
        let unavailable = match (&self.unavailable, closed_failure) {
            // A broken accounts file fails both reads with the same text: say it once.
            (Some(first), Some(second)) if first.contains(second) => Some(first.clone()),
            (Some(first), Some(second)) => Some(format!("{first}; {second}")),
            (Some(first), None) => Some(first.clone()),
            (None, Some(second)) => Some(second.to_string()),
            (None, None) => None,
        };
        Some(view_of(
            at,
            self.this_host.clone(),
            &rows.sessions,
            closed_sessions,
            other,
            &self.thinking,
            unavailable.as_deref(),
            self.currency.as_deref(),
            options,
        ))
    }

    fn project_for(&mut self, cwd: &str) -> ProjectKey {
        if let Some(project) = self.projects.get(cwd) {
            return project.clone();
        }
        let project = project::identify(Path::new(cwd), &self.home);
        self.projects.insert(cwd.to_string(), project.clone());
        project
    }
}

/// Whether a time is due for the next round: `now` is at least `secs` after
/// `at`, or `at` is in the future (a clock that moved back must not silence
/// the thread). `None` is due at once. Every "is it time again" rule of the
/// thread goes through this one helper.
fn is_due(at: Option<DateTime<Utc>>, now: DateTime<Utc>, secs: i64) -> bool {
    match at {
        None => true,
        Some(at) => at > now || now.signed_duration_since(at).num_seconds() >= secs,
    }
}

/// Whether one part of the rows context has to be built now: never built,
/// older than [`ROWS_CONTEXT_MAX_AGE_SECS`], or a failed build whose
/// [`ROWS_CONTEXT_RETRY_SECS`] have passed.
fn part_needs_build(
    built: Option<DateTime<Utc>>,
    failure: Option<&ContextFailure>,
    now: DateTime<Utc>,
) -> bool {
    if !is_due(built, now, ROWS_CONTEXT_MAX_AGE_SECS) {
        return false;
    }
    failure.is_none_or(|failure| is_due(Some(failure.tried), now, ROWS_CONTEXT_RETRY_SECS))
}

/// Build the two parts of the rows context that are due, show every failure
/// that is still current once, and hand out the projects and the pricing the
/// rows are built with.
fn prepare_rows_context<'a>(
    context: &'a mut RowsContext,
    app: &AppPaths,
    history: &HistoryDir,
    now: DateTime<Utc>,
    summary: &mut SessionsSummary,
) -> (
    Option<&'a projects::Projects>,
    live_sessions::Pricing<'a>,
    Option<String>,
) {
    let projects_due = part_needs_build(
        context.projects.as_ref().map(|built| built.built),
        context.projects_failure.as_ref(),
        now,
    );
    let pricing_due = part_needs_build(
        context.pricing.as_ref().map(|built| built.built),
        context.pricing_failure.as_ref(),
        now,
    );
    if projects_due || pricing_due {
        let accounts = store::load_accounts(app)
            .map_err(|error| format!("the accounts cannot be read: {error}"));
        // The view's currency comes from the settings with the accounts file,
        // whether or not the prices and the dollar rate can be read.
        context.currency = accounts
            .as_ref()
            .ok()
            .and_then(|file| file.settings.currency.clone());
        if projects_due {
            match &accounts {
                Ok(file) => match projects::load(history, &file.accounts) {
                    Ok((projects, warnings)) => {
                        context.projects = Some(ProjectsBuilt {
                            projects,
                            warnings,
                            built: now,
                        });
                        context.projects_failure = None;
                    }
                    Err(error) => {
                        context.projects_failure = Some(ContextFailure {
                            text: format!("the projects cannot be read: {error}"),
                            tried: now,
                        })
                    }
                },
                Err(error) => {
                    context.projects_failure = Some(ContextFailure {
                        text: error.clone(),
                        tried: now,
                    })
                }
            }
        }
        if pricing_due {
            match &accounts {
                Ok(file) => match build_pricing(
                    app,
                    history,
                    &Range {
                        from: now - ChronoDuration::days(1),
                        to: now,
                    },
                    file,
                ) {
                    Ok((valuer, settings)) => {
                        context.pricing = Some(PricingBuilt {
                            valuer,
                            settings,
                            built: now,
                        });
                        context.pricing_failure = None;
                    }
                    Err(text) => {
                        context.pricing_failure = Some(ContextFailure { text, tried: now })
                    }
                },
                Err(error) => {
                    context.pricing_failure = Some(ContextFailure {
                        text: error.clone(),
                        tried: now,
                    })
                }
            }
        }
    }

    let projects_failure = context
        .projects_failure
        .as_ref()
        .map(|failure| failure.text.clone());
    let pricing_failure = context
        .pricing_failure
        .as_ref()
        .map(|failure| failure.text.clone());
    if let Some(text) = &projects_failure {
        add_unavailable(summary, text.clone());
    }
    if let Some(text) = &pricing_failure {
        // The accounts are one failure for both parts: show it once.
        if projects_failure.as_ref() != Some(text) {
            add_unavailable(summary, text.clone());
        }
    }
    if let Some(built) = &context.projects {
        for warning in &built.warnings {
            add_warning(summary, warning.clone());
        }
    }
    let projects = context.projects.as_ref().map(|built| &built.projects);
    let pricing = match &context.pricing {
        Some(built) => live_sessions::Pricing::Known {
            valuer: &built.valuer,
            settings: &built.settings,
        },
        None => live_sessions::Pricing::Unknown {
            reason: pricing_failure.unwrap_or_else(|| "the values cannot be read".to_string()),
        },
    };
    let currency = context.currency.clone();
    (projects, pricing, currency)
}

/// Read the prices, the dollar rate and the accounts and build the valuer for
/// `range`. Any failure is the error text the sessions area shows.
fn build_pricing(
    app: &AppPaths,
    history: &HistoryDir,
    range: &Range,
    file: &store::AccountsFile,
) -> Result<(Valuer, Settings), String> {
    let prices =
        Prices::load(app).map_err(|error| format!("the prices cannot be read: {error}"))?;
    let fx = Fx::load(app).map_err(|error| format!("the dollar rate cannot be read: {error}"))?;
    let valuer = Valuer::build(history, range, &prices, &fx, &file.settings, &file.accounts)
        .map_err(|error| format!("the values cannot be read: {error}"))?;
    Ok((valuer, file.settings.clone()))
}

struct WaitingMemory {
    project: ProjectKey,
    since: DateTime<Utc>,
}

/// The handle the tray keeps. Dropping it, or calling [`stop`](Self::stop),
/// stops and joins the thread.
pub(crate) struct SessionsHandle {
    stop: Arc<AtomicBool>,
    commands: Sender<SessionsCommand>,
    client: SessionsClient,
    thread: Option<JoinHandle<()>>,
    latest: Arc<Mutex<Option<SessionsSummary>>>,
}

impl SessionsHandle {
    /// The summary of the last pass, shared with the action thread so a switch
    /// can record how many sessions it interrupted.
    pub(crate) fn latest(&self) -> Arc<Mutex<Option<SessionsSummary>>> {
        Arc::clone(&self.latest)
    }

    /// The client the three sessions commands use.
    pub(crate) fn client(&self) -> SessionsClient {
        self.client.clone()
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.commands.send(SessionsCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SessionsHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start the sessions thread. `settings.read_sessions` must already be on; the
/// caller only starts it then. `history` is the folder the worker opened at
/// start, shared with the engine and action runtimes.
pub(crate) fn spawn(
    app: AppPaths,
    claude: ClaudePaths,
    history: Result<HistoryDir, String>,
    ui: Sender<UiEvent>,
    engine: Sender<EngineCommand>,
) -> Result<SessionsHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (commands, command_rx) = mpsc::channel::<SessionsCommand>();
    let latest = Arc::new(Mutex::new(None));
    let thread_stop = Arc::clone(&stop);
    let thread_latest = Arc::clone(&latest);
    let watch_wake = commands.clone();
    let client = SessionsClient {
        commands: commands.clone(),
    };
    let thread = thread::Builder::new()
        .name("seatswap-sessions".to_string())
        .spawn(move || {
            run(
                app,
                claude,
                history,
                ui,
                engine,
                thread_stop,
                command_rx,
                watch_wake,
                thread_latest,
            )
        })
        .map_err(|error| format!("cannot start the sessions thread: {error}"))?;
    Ok(SessionsHandle {
        stop,
        commands,
        client,
        thread: Some(thread),
        latest,
    })
}

#[allow(clippy::too_many_arguments)]
fn run(
    app: AppPaths,
    claude: ClaudePaths,
    history: Result<HistoryDir, String>,
    ui: Sender<UiEvent>,
    engine: Sender<EngineCommand>,
    stop: Arc<AtomicBool>,
    commands: Receiver<SessionsCommand>,
    watch_wake: Sender<SessionsCommand>,
    latest: Arc<Mutex<Option<SessionsSummary>>>,
) {
    let (recorder, history_error) = match history {
        Ok(history) => (Some(Recorder::new(history)), None),
        Err(error) => (None, Some(error)),
    };
    let (mut state, state_error) = load_state(&app);
    let mut memory = SessionMemory::new(claude.home().to_path_buf());
    memory.history_error = history_error.clone();
    memory.ui = Some(ui.clone());
    let mut watch: Option<FolderWatch> = None;
    let mut last_watch_error: Option<String> = None;
    let mut transcript_watches: BTreeMap<PathBuf, FolderWatch> = BTreeMap::new();
    let mut last_transcript_watch_error: Option<String> = None;

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if watch.is_none() {
            let wake = watch_wake.clone();
            match watch_folder(&claude.sessions_dir(), move || {
                let _ = wake.send(SessionsCommand::Wake);
            }) {
                Ok(found) => watch = Some(found),
                // The folder is made when the first session starts; the 60 s
                // wake tries again.
                Err(WatchError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    let text = error.to_string();
                    if last_watch_error.as_deref() != Some(text.as_str()) {
                        eprintln!("seatswap: sessions watcher: {text}");
                        last_watch_error = Some(text);
                    }
                }
            }
        }

        let now = Utc::now();
        let outcome = match SystemProbe::new() {
            Ok(probe) => one_pass(
                &app,
                &claude,
                &probe,
                recorder.as_ref(),
                &mut state,
                &mut memory,
                now,
                state_error.as_deref(),
                history_error.as_deref(),
            ),
            Err(error) => {
                let mut summary = SessionsSummary {
                    running: 0,
                    busy: 0,
                    tied: 0,
                    tied_busy: 0,
                    waiting: Vec::new(),
                    unavailable: None,
                    warning: None,
                };
                add_unavailable(
                    &mut summary,
                    format!("running sessions cannot be read: {error}"),
                );
                if let Some(error) = &history_error {
                    // The order the pop-up shows: the history error first, then the state.
                    add_warning(&mut summary, error.clone());
                }
                if let Some(error) = &state_error {
                    add_warning(&mut summary, error.clone());
                }
                save_state(
                    &app,
                    &claude,
                    &mut state,
                    now,
                    state_error.as_deref(),
                    &mut summary,
                );
                pass_without_running(&app, recorder.as_ref(), &mut memory, now, summary)
            }
        };

        update_transcript_watches(
            &mut transcript_watches,
            &memory.transcript_folders,
            &watch_wake,
            &mut last_transcript_watch_error,
        );

        {
            let mut shared = match latest.lock() {
                Ok(shared) => shared,
                Err(poisoned) => poisoned.into_inner(),
            };
            *shared = Some(outcome.summary.clone());
        }
        if ui.send(UiEvent::Sessions(outcome.summary.clone())).is_err() {
            return;
        }
        if engine
            .send(EngineCommand::ModelsInUse {
                models: outcome.models,
                tied_busy: outcome.tied_busy,
            })
            .is_err()
        {
            return;
        }

        let mut wait = if outcome.wake_soon {
            AFTER_INCOMPLETE
        } else {
            WAKE
        };
        if let Some(pending) = memory.throttle.wait(now) {
            if pending < wait {
                wait = pending;
            }
        }
        // One deadline for the next pass. A command handled below waits only
        // for the rest of it; a `View` or `Unsubscribe` never moves it later.
        let deadline = Instant::now() + wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match commands.recv_timeout(remaining) {
                Ok(SessionsCommand::Wake) => break,
                Ok(SessionsCommand::View { options, reply }) => {
                    let answer = answer_view(&mut memory, options, &app, recorder.as_ref());
                    let _ = reply.send(answer);
                }
                Ok(SessionsCommand::Subscribe(options)) => {
                    memory.subscribe(options);
                    break;
                }
                Ok(SessionsCommand::Unsubscribe) => {
                    memory.unsubscribe();
                    transcript_watches.clear();
                }
                Ok(SessionsCommand::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
            // A command handled past the deadline never keeps the next pass
            // waiting: the deadline is the pass's, not the command's.
            if Instant::now() >= deadline {
                break;
            }
        }
    }
}

/// Keep exactly the transcript folders of the running sessions and their
/// agents: a folder that left the list is dropped, a new one is watched. The
/// path comes from the `ScanState` key, never from a second naming rule.
fn update_transcript_watches(
    watches: &mut BTreeMap<PathBuf, FolderWatch>,
    folders: &BTreeSet<PathBuf>,
    wake: &Sender<SessionsCommand>,
    last_error: &mut Option<String>,
) {
    watches.retain(|folder, _| folders.contains(folder));
    for folder in folders {
        if watches.contains_key(folder) {
            continue;
        }
        let wake = wake.clone();
        match watch_folder(folder, move || {
            let _ = wake.send(SessionsCommand::Wake);
        }) {
            Ok(found) => {
                watches.insert(folder.clone(), found);
            }
            Err(WatchError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            }
            Err(error) => {
                let text = format!("{}: {error}", folder.display());
                if last_error.as_deref() != Some(text.as_str()) {
                    eprintln!("seatswap: sessions watcher: {text}");
                    *last_error = Some(text);
                }
            }
        }
    }
}

/// The transcript state at start: the files saved by the last run, or the
/// default with the error that keeps this pass from writing it back.
fn load_state(app: &AppPaths) -> (ScanState, Option<String>) {
    match ScanState::load(app) {
        Ok(state) => (state, None),
        Err(error) => (
            ScanState::default(),
            Some(format!(
                "the transcript state cannot be read: {error}; delete {} to read the transcripts again",
                app.transcripts_state_dir().display()
            )),
        ),
    }
}

/// Write what changed; a failure is a warning on the summary, never a pass
/// that stops.
fn save_state(
    app: &AppPaths,
    claude: &ClaudePaths,
    state: &mut ScanState,
    now: DateTime<Utc>,
    state_error: Option<&str>,
    summary: &mut SessionsSummary,
) {
    if state_error.is_some() {
        return;
    }
    if let Err(error) = state.save(claude, app, now) {
        add_warning(
            summary,
            format!("the transcript state cannot be written: {error}"),
        );
    }
}

/// One wake of the sessions thread: read what is new, write the history, save
/// the state. The thread calls it with [`SystemProbe`]; the tests with a fake
/// probe.
#[allow(clippy::too_many_arguments)]
fn one_pass(
    app: &AppPaths,
    claude: &ClaudePaths,
    probe: &dyn Liveness,
    recorder: Option<&Recorder>,
    state: &mut ScanState,
    memory: &mut SessionMemory,
    now: DateTime<Utc>,
    state_error: Option<&str>,
    history_error: Option<&str>,
) -> PassOutcome {
    let mut outcome = run_pass(
        app,
        claude,
        probe,
        recorder,
        state,
        memory,
        now,
        state_error,
    );
    memory.unavailable = outcome.summary.unavailable.clone();
    if let Some(error) = history_error {
        // The order the pop-up shows: the history error first, then the state.
        prepend_warning(&mut outcome.summary, error.to_string());
    }
    save_state(app, claude, state, now, state_error, &mut outcome.summary);
    outcome
}

/// One pass: running sessions, transcripts, tokens, session records, waits.
/// The thread calls it with [`SystemProbe`]; the tests call it with a fake probe.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_pass(
    app: &AppPaths,
    claude: &ClaudePaths,
    probe: &dyn Liveness,
    recorder: Option<&Recorder>,
    state: &mut ScanState,
    memory: &mut SessionMemory,
    now: DateTime<Utc>,
    state_error: Option<&str>,
) -> PassOutcome {
    let mut summary = SessionsSummary {
        running: 0,
        busy: 0,
        tied: 0,
        tied_busy: 0,
        waiting: Vec::new(),
        unavailable: None,
        warning: state_error.map(str::to_string),
    };
    let running = match sessions::running(claude, probe) {
        Ok(running) => running,
        Err(error) => {
            add_unavailable(
                &mut summary,
                format!("running sessions cannot be read: {error}"),
            );
            return pass_without_running(app, recorder, memory, now, summary);
        }
    };

    summary.running = match count_u32(running.sessions.len(), "the running session count") {
        Ok(count) => count,
        Err(error) => {
            add_warning(&mut summary, error);
            return PassOutcome {
                summary,
                models: None,
                tied_busy: 0,
                wake_soon: false,
            };
        }
    };
    let busy = running
        .sessions
        .iter()
        .filter(|session| session.status == SessionStatus::Busy)
        .count();
    summary.busy = match count_u32(busy, "the busy session count") {
        Ok(count) => count,
        Err(error) => {
            add_warning(&mut summary, error);
            return PassOutcome {
                summary,
                models: None,
                tied_busy: 0,
                wake_soon: false,
            };
        }
    };
    for session in &running.sessions {
        if let Some(version) = &session.version {
            if sessions::newer_than_tested(version) {
                add_warning(
                    &mut summary,
                    format!(
                        "Claude Code {version} is newer than the version seatswap was tested with ({}). Session data may be incomplete.",
                        sessions::TESTED_CLAUDE_CODE
                    ),
                );
                break;
            }
        }
    }
    if !running.skipped.is_empty() {
        let texts: Vec<String> = running
            .skipped
            .iter()
            .map(|name| format!("session file {name} cannot be read"))
            .collect();
        add_warning(
            &mut summary,
            format!("{}; no session was closed in this pass", texts.join("; ")),
        );
    }

    // Waiting sessions, and the waits that just ended. The rows are the one
    // source of a waiting session's text; a file with an invalid time is their
    // warning, not a second one here.
    let mut still_waiting: HashSet<String> = HashSet::new();
    for session in &running.sessions {
        if session.status != SessionStatus::Waiting {
            continue;
        }
        let Some(since) = DateTime::<Utc>::from_timestamp_millis(session.status_updated_at) else {
            continue;
        };
        let project = memory.project_for(&session.cwd);
        memory
            .waiting
            .insert(session.session_id.clone(), WaitingMemory { project, since });
        still_waiting.insert(session.session_id.clone());
    }
    let ended: Vec<String> = memory
        .waiting
        .keys()
        .filter(|session| !still_waiting.contains(*session))
        .cloned()
        .collect();
    for session in ended {
        if let Some(wait) = memory.waiting.remove(&session) {
            let secs = match u64::try_from((now - wait.since).num_seconds()) {
                Ok(secs) => secs,
                Err(_) => {
                    add_warning(
                        &mut summary,
                        format!(
                            "a wait of session {session} ended before it began (the clock moved back); it is not recorded"
                        ),
                    );
                    continue;
                }
            };
            if let Some(recorder) = recorder {
                if let Err(error) = recorder.event(
                    now,
                    EventKind::Waited {
                        session,
                        project: wait.project,
                        secs,
                    },
                ) {
                    add_history_error(&mut summary, error);
                }
            }
        }
    }

    // Transcripts. A state that could not be read keeps the session counts but
    // stops every transcript read, as the settings say.
    let mut scan_complete = true;
    if state_error.is_none() {
        match transcripts::scan(claude, state, SCAN_BUDGET) {
            Ok(mut scan) => {
                scan_complete = scan.complete;
                let tokens = transcripts::token_records(&scan.answers, scan.state(), &memory.home);
                if tokens.is_empty() {
                    // No token line can be lost, so the offsets move at once;
                    // the caller's save writes the state and its pruning.
                    scan.commit(state);
                } else if recorder.is_none() {
                    // Without a recorder the token lines are not written. The
                    // offsets stay where they were, so a later pass with a
                    // working history reads these answers again.
                    scan_complete = true;
                } else {
                    // Put the scanned offsets on disk before the token lines: a
                    // crash between the two steps loses those answers (the disk
                    // says they were read, but their lines were never written),
                    // never counts them twice. The other order counted them
                    // twice when the save failed.
                    let folders = scan.changed_folders();
                    match scan.save(claude, app, now) {
                        Ok(_) => {
                            let tokens_written = match recorder {
                                Some(recorder) => match recorder.tokens(&tokens) {
                                    Ok(()) => true,
                                    Err(error) => {
                                        add_history_error(&mut summary, error);
                                        false
                                    }
                                },
                                None => true,
                            };
                            if tokens_written {
                                scan.commit(state);
                            } else {
                                // The token lines are not written: put the old
                                // offsets back on disk, so a restart reads the
                                // same answers again and nothing is counted
                                // twice.
                                for folder in &folders {
                                    state.mark_changed(folder);
                                }
                                if let Err(error) = state.save(claude, app, now) {
                                    add_warning(
                                        &mut summary,
                                        format!(
                                            "the transcript state cannot be written back: {error}"
                                        ),
                                    );
                                }
                                // Retrying at once would read the same bytes
                                // again and make no progress until the history
                                // works: wait for the regular wake instead.
                                scan_complete = true;
                            }
                        }
                        Err(error) => {
                            add_warning(
                                &mut summary,
                                format!("the transcript state cannot be written: {error}"),
                            );
                            scan_complete = true;
                        }
                    }
                }
            }
            Err(error) => {
                scan_complete = false;
                add_warning(
                    &mut summary,
                    format!("the transcripts cannot be read: {error}"),
                );
            }
        }
    }

    let running_ids: BTreeSet<String> = running
        .sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect();

    // One record per running session whose summary changed and whose last
    // record is old enough (or which has none yet).
    for session in &running.sessions {
        let Some(seen) = state.sessions.get(&session.session_id) else {
            continue;
        };
        let record = match session_record(
            claude,
            &session.session_id,
            seen,
            session.entrypoint.clone(),
            false,
            memory,
            now,
        ) {
            Ok(record) => record,
            Err(error) => {
                add_warning(&mut summary, error);
                continue;
            }
        };
        if !record_is_due(memory, &record, now) {
            continue;
        }
        if let Some(recorder) = recorder {
            match recorder.session(&record) {
                Ok(()) => {
                    let last = record.last;
                    memory.records.insert(record.session.clone(), record);
                    memory.closed.remove(&session.session_id);
                    state.set_recorded(
                        &session.session_id,
                        RecordedSession {
                            last,
                            closed: false,
                        },
                    );
                }
                Err(error) => add_history_error(&mut summary, error),
            }
        }
    }

    // Every session seen in a transcript whose session file is not running gets
    // one closed record: a `claude -p` session that started and ended between
    // two wakes, and every session the backfill read. `recorded` remembers what
    // was written, so a restart writes no second record for an unchanged
    // session. When a session file could not be read this pass, the thread
    // cannot tell which session ended: write no closed record.
    if running.skipped.is_empty() {
        let stopped: Vec<String> = state
            .sessions
            .keys()
            .filter(|session| !running_ids.contains(*session))
            .cloned()
            .collect();
        for session in stopped {
            let Some(seen) = state.sessions.get(&session) else {
                continue;
            };
            let last = seen.last;
            if seen
                .recorded
                .as_ref()
                .is_some_and(|recorded| recorded.closed && recorded.last == last)
            {
                continue;
            }
            let entrypoint = memory
                .records
                .get(&session)
                .and_then(|record| record.entrypoint.clone());
            let record = match session_record(claude, &session, seen, entrypoint, true, memory, now)
            {
                Ok(record) => record,
                Err(error) => {
                    add_warning(&mut summary, error);
                    continue;
                }
            };
            if let Some(recorder) = recorder {
                match recorder.session(&record) {
                    Ok(()) => {
                        memory.records.insert(session.clone(), record);
                        state.set_recorded(&session, RecordedSession { last, closed: true });
                    }
                    Err(error) => add_history_error(&mut summary, error),
                }
            }
        }
    }

    // A session that left the running list whose transcript entry is already
    // pruned still gets one closing record, with the values of its last record.
    if running.skipped.is_empty() {
        let gone: Vec<String> = memory
            .records
            .keys()
            .filter(|session| {
                !running_ids.contains(*session)
                    && !state.sessions.contains_key(*session)
                    && !memory.closed.contains(*session)
            })
            .cloned()
            .collect();
        for session in gone {
            let Some(last) = memory.records.get(&session).cloned() else {
                continue;
            };
            if last.closed {
                memory.closed.insert(session);
                continue;
            }
            let mut closed = last;
            closed.t = now;
            closed.closed = true;
            if let Some(recorder) = recorder {
                match recorder.session(&closed) {
                    Ok(()) => {
                        memory.records.insert(session.clone(), closed);
                        memory.closed.insert(session);
                    }
                    Err(error) => add_history_error(&mut summary, error),
                }
            }
        }
    }

    // A session that is not running and whose last answer is more than an hour
    // old leaves the state; its files' offsets stay, so its transcript is not
    // read again. As with the closed records: when a session file could not be
    // read this pass, the thread cannot tell which session ended, so it forgets
    // none.
    if running.skipped.is_empty() {
        state.forget_closed(&running_ids, now);
    }

    // A session that left the list is reported at once and its closed rows
    // are read again; otherwise both follow their own "is it time" rules.
    let closed_now = running.skipped.is_empty()
        && memory
            .running_ids
            .iter()
            .any(|session| !running_ids.contains(session));

    // The rows of this host, and the report the other hosts read. The session
    // files alone are enough to build them: a history that cannot be read gives
    // the projects' default names and says why the values are missing, so a
    // waiting session still notifies.
    let mut currency = None;
    let rows = match recorder {
        Some(recorder) => {
            let mut context = memory.rows_context.take().unwrap_or_default();
            let rows = {
                let (projects, pricing, settings_currency) =
                    prepare_rows_context(&mut context, app, recorder.dir(), now, &mut summary);
                currency = settings_currency;
                live_rows(
                    &running.sessions,
                    state,
                    claude,
                    memory,
                    projects,
                    &pricing,
                    recorder.dir().host(),
                    now,
                )
            };
            memory.rows_context = Some(context);
            Some(rows)
        }
        None => {
            let pricing = live_sessions::Pricing::Unknown {
                reason: memory.history_error.clone().unwrap_or_else(|| {
                    "the history is not open, so the values are not known".to_string()
                }),
            };
            Some(live_rows(
                &running.sessions,
                state,
                claude,
                memory,
                None,
                &pricing,
                "",
                now,
            ))
        }
    };
    if let Some(rows) = &rows {
        for warning in &rows.warnings {
            add_warning(&mut summary, warning.clone());
        }
        for session in rows
            .sessions
            .iter()
            .filter(|session| session.status == LiveStatus::Waiting)
        {
            summary.waiting.push(Waiting {
                session: session.session.clone(),
                label: session.label.clone(),
                project: session.project.clone(),
                since: session.since,
                host: None,
            });
        }
    }

    // The report follows the rows at most every
    // [`live_sessions::REPORT_EVERY_SECS`], with a heartbeat so another host
    // does not see a quiet host as stale.
    if running.skipped.is_empty() {
        if let (Some(recorder), Some(rows)) = (recorder, rows.as_ref()) {
            write_report_if_due(
                recorder,
                &rows.sessions,
                now,
                closed_now,
                memory,
                &mut summary,
            );
            memory.running_ids = running_ids.clone();
        }
    }

    read_other_hosts(recorder, memory, now, &mut summary);

    // What the view is made of, kept for the commands and the pushes.
    memory.rows = rows;
    memory.this_host = recorder.map(|recorder| recorder.dir().host().to_string());
    memory.at = Some(now);
    memory.currency = currency;
    memory.thinking = running
        .sessions
        .iter()
        .filter_map(|session| {
            state
                .sessions
                .get(&session.session_id)
                .map(|seen| (session.session_id.clone(), seen.thinking_since_user))
        })
        .collect();
    refresh_closed_rows(app, recorder, memory, now, closed_now, &mut summary);
    memory.unavailable = summary.unavailable.clone();
    memory.transcript_folders = if memory.subscription.is_some() {
        transcript_folders(state, &running.sessions, &claude.projects_dir())
    } else {
        BTreeSet::new()
    };
    publish_view(memory, now);

    let models = models_in_use(&running.sessions, state);
    let (_, tied_busy) = tied_sessions(&running.sessions, state);
    PassOutcome {
        summary,
        models,
        tied_busy,
        wake_soon: !scan_complete,
    }
}

/// Read the other hosts' reports when due, and add their warnings and waiting
/// sessions to the summary. Their reports are read at most every
/// [`live_sessions::REPORT_EVERY_SECS`], like this host writes its own; the
/// last read is used on every pass in between, so a wait and a warning stay put
/// until the next read. A read that fails replaces it with an empty one and its
/// error.
fn read_other_hosts(
    recorder: Option<&Recorder>,
    memory: &mut SessionMemory,
    now: DateTime<Utc>,
    summary: &mut SessionsSummary,
) {
    let Some(recorder) = recorder else {
        return;
    };
    if is_due(
        memory.last_other_hosts_at,
        now,
        live_sessions::REPORT_EVERY_SECS,
    ) {
        memory.last_other_hosts_at = Some(now);
        memory.other_hosts = Some(match live_sessions::other_hosts(recorder.dir(), now) {
            Ok(other) => other,
            Err(error) => live_sessions::OtherHosts {
                hosts: Vec::new(),
                warnings: vec![format!("the other hosts' reports cannot be read: {error}")],
            },
        });
    }
    let Some(other) = &memory.other_hosts else {
        return;
    };
    for warning in &other.warnings {
        add_warning(summary, warning.clone());
    }
    for host in &other.hosts {
        if host.stale {
            continue;
        }
        for session in &host.sessions {
            if session.status != LiveStatus::Waiting {
                continue;
            }
            summary.waiting.push(Waiting {
                session: session.session.clone(),
                label: session.label.clone(),
                project: session.project.clone(),
                since: session.since,
                host: Some(host.host.clone()),
            });
        }
    }
}

/// A pass that cannot read the running sessions still leaves a view, not "no
/// pass yet": this host's running rows are empty (they are not known, never the
/// old ones), while the other hosts' reports and the closed rows are kept. The
/// summary's `unavailable` text is the view's, and the pass time is recorded.
fn pass_without_running(
    app: &AppPaths,
    recorder: Option<&Recorder>,
    memory: &mut SessionMemory,
    now: DateTime<Utc>,
    mut summary: SessionsSummary,
) -> PassOutcome {
    memory.rows = Some(no_rows());
    memory.thinking.clear();
    memory.this_host = recorder.map(|recorder| recorder.dir().host().to_string());
    memory.at = Some(now);
    memory.currency = match recorder {
        Some(recorder) => {
            let mut context = memory.rows_context.take().unwrap_or_default();
            // The settings' currency, the projects and the pricing the closed
            // rows need; the running rows are not built.
            let currency = {
                let (_, _, currency) =
                    prepare_rows_context(&mut context, app, recorder.dir(), now, &mut summary);
                currency
            };
            memory.rows_context = Some(context);
            currency
        }
        None => None,
    };
    read_other_hosts(recorder, memory, now, &mut summary);
    refresh_closed_rows(app, recorder, memory, now, false, &mut summary);
    memory.unavailable = summary.unavailable.clone();
    // No running session is known, so there is no transcript folder to watch.
    memory.transcript_folders.clear();
    publish_view(memory, now);
    PassOutcome {
        summary,
        models: None,
        tied_busy: 0,
        wake_soon: false,
    }
}

/// The view a `View` command answers: from what the thread holds, without a
/// pass. Its options are the command's own; the subscription's are not touched.
/// Another closed choice reads that choice's closed rows once and keeps them.
fn answer_view(
    memory: &mut SessionMemory,
    options: SessionsOptions,
    app: &AppPaths,
    recorder: Option<&Recorder>,
) -> Result<SessionsView, String> {
    let at = memory
        .at
        .ok_or_else(|| "the sessions thread has not finished a pass yet".to_string())?;
    if let Some(recorder) = recorder {
        read_closed_rows(memory, app, recorder, options.closed, at, false);
    }
    memory
        .view_for(&options)
        .ok_or_else(|| "the sessions thread has not finished a pass yet".to_string())
}

/// Refresh the closed rows of the subscription's choice after a pass, and drop
/// a choice no command has asked for in [`CLOSED_KEEP_SECS`]. `ClosedSince::None`
/// reads nothing.
fn refresh_closed_rows(
    app: &AppPaths,
    recorder: Option<&Recorder>,
    memory: &mut SessionMemory,
    now: DateTime<Utc>,
    closed_now: bool,
    summary: &mut SessionsSummary,
) {
    let choice = memory.subscription.as_ref().map(|options| options.closed);
    let Some(recorder) = recorder else {
        memory.closed_rows.clear();
        return;
    };
    if let Some(choice) = choice {
        read_closed_rows(memory, app, recorder, choice, now, closed_now);
    }
    memory.closed_rows.retain(|_, read| {
        now.signed_duration_since(read.asked_at).num_seconds() < CLOSED_KEEP_SECS
    });
    // The closed rows belong to the window: their failure is the view's
    // `unavailable` (see `SessionMemory::view_for`), not the pop-up's. The
    // history read's warnings are passed on like every other history warning.
    if let Some(read) = choice.and_then(|choice| memory.closed_rows.get(&choice)) {
        for warning in &read.rows.warnings {
            add_warning(summary, warning.clone());
        }
    }
}

/// Read `choice`'s closed rows when they are not held, a running session of this
/// host left the list (`closed_now`), or [`CLOSED_EVERY_SECS`] passed. The
/// result is kept with that choice until its next read or its last ask is
/// [`CLOSED_KEEP_SECS`] old.
fn read_closed_rows(
    memory: &mut SessionMemory,
    app: &AppPaths,
    recorder: &Recorder,
    choice: ClosedSince,
    now: DateTime<Utc>,
    closed_now: bool,
) {
    if choice == ClosedSince::None {
        memory.closed_rows.remove(&ClosedSince::None);
        return;
    }
    let due = match memory.closed_rows.get(&choice) {
        None => true,
        Some(read) => closed_now || is_due(Some(read.read_at), now, CLOSED_EVERY_SECS),
    };
    if !due {
        if let Some(read) = memory.closed_rows.get_mut(&choice) {
            read.asked_at = now;
        }
        return;
    }
    let Some(since) = choice.since(now) else {
        return;
    };
    let projects = memory
        .rows_context
        .as_ref()
        .and_then(|context| context.projects.as_ref())
        .map(|built| built.projects.clone());
    let (rows, failure) = closed_rows_for(app, recorder.dir(), since, now, projects.as_ref());
    memory.closed_rows.insert(
        choice,
        ClosedRows {
            rows,
            read_at: now,
            asked_at: now,
            failure,
        },
    );
}

/// The closed rows over `[since, now]`, with a valuer built for exactly that
/// range. A missing part is `None` / `Pricing::Unknown`, as for the running
/// rows; the error text is the summary's `unavailable`.
fn closed_rows_for(
    app: &AppPaths,
    history: &HistoryDir,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    projects: Option<&projects::Projects>,
) -> (Rows, Option<String>) {
    let range = Range {
        from: since,
        to: now,
    };
    let file = match store::load_accounts(app) {
        Ok(file) => file,
        Err(error) => {
            let text = format!("the accounts cannot be read: {error}");
            return (no_rows(), Some(text));
        }
    };
    let (failure, rows) = match build_pricing(app, history, &range, &file) {
        Ok((valuer, settings)) => (
            None,
            live_sessions::closed(
                history,
                since,
                now,
                projects,
                &live_sessions::Pricing::Known {
                    valuer: &valuer,
                    settings: &settings,
                },
            ),
        ),
        Err(text) => (
            Some(text.clone()),
            live_sessions::closed(
                history,
                since,
                now,
                projects,
                &live_sessions::Pricing::Unknown { reason: text },
            ),
        ),
    };
    match rows {
        Ok(rows) => (rows, failure),
        Err(error) => {
            let text = format!("the closed sessions cannot be read: {error}");
            (no_rows(), Some(text))
        }
    }
}

/// No closed rows: the reason is returned beside them, never as a warning.
fn no_rows() -> Rows {
    Rows {
        sessions: Vec::new(),
        warnings: Vec::new(),
    }
}

/// The transcript folders of the running sessions and their agents, from the
/// keys the `ScanState` already knows: `<folder>/<session>.jsonl` and
/// `<folder>/<session>/subagents/agent-<id>.jsonl`.
fn transcript_folders(
    state: &ScanState,
    running: &[SessionFile],
    projects_dir: &Path,
) -> BTreeSet<PathBuf> {
    let runs: HashMap<&str, &str> = running
        .iter()
        .filter_map(|session| {
            state
                .sessions
                .get(&session.session_id)
                .map(|seen| (session.session_id.as_str(), seen.folder.as_str()))
        })
        .collect();
    let mut folders = BTreeSet::new();
    // The main transcript's folder is the project folder the session sees.
    for folder in runs.values() {
        if !folder.is_empty() {
            folders.insert(projects_dir.join(folder));
        }
    }
    // The agents' folders are the deeper keys of the same session.
    for key in state.files.keys() {
        let Some((folder, rest)) = key.split_once('/') else {
            continue;
        };
        let session = rest.split(['/', '.']).next().unwrap_or_default();
        if runs
            .get(session)
            .is_some_and(|expected| *expected == folder)
        {
            if let Some(parent) = Path::new(key).parent() {
                if !parent.as_os_str().is_empty() {
                    folders.insert(projects_dir.join(parent));
                }
            }
        }
    }
    folders
}

/// Push the view after a pass while subscribed: at most one per
/// [`VIEW_EVERY_MS`], with a trailing one, and the minute pass even when
/// nothing changed. Without a subscription nothing is sent.
fn publish_view(memory: &mut SessionMemory, now: DateTime<Utc>) {
    if memory.subscription.is_none() {
        return;
    }
    let Some(view) = memory.view() else {
        return;
    };
    let changed = memory
        .last_pushed
        .as_ref()
        .is_none_or(|last| !same_pushed(last, &view));
    if !memory.throttle.due(now, changed) {
        return;
    }
    memory.last_pushed = Some(view.clone());
    if let Some(ui) = &memory.ui {
        let _ = ui.send(UiEvent::SessionsView(view));
    }
}

/// Whether two pushed views hold the same facts. The times move with every
/// pass and are not a change (the minute pass covers them).
fn same_pushed(first: &SessionsView, second: &SessionsView) -> bool {
    first.this_host == second.this_host
        && first.sessions == second.sessions
        && first.switch_effect == second.switch_effect
        && first.currency == second.currency
        && first.unavailable == second.unavailable
        && first.in_use == second.in_use
        && first.hosts.len() == second.hosts.len()
        && first
            .hosts
            .iter()
            .zip(&second.hosts)
            .all(|(a, b)| a.host == b.host && a.stale == b.stale && a.this == b.this)
}

/// The rows of this host from the session files, with the project look-up the
/// thread caches and the pricing that is known.
#[allow(clippy::too_many_arguments)]
fn live_rows(
    files: &[SessionFile],
    state: &ScanState,
    claude: &ClaudePaths,
    memory: &mut SessionMemory,
    projects: Option<&projects::Projects>,
    pricing: &live_sessions::Pricing,
    host: &str,
    now: DateTime<Utc>,
) -> live_sessions::Rows {
    let metas = |session: &str| {
        state
            .sessions
            .get(session)
            .map(|seen| transcripts::agents_of(claude, session, &seen.spawns))
            .unwrap_or_default()
    };
    let project_of = {
        let memory = RefCell::new(memory);
        move |cwd: &str| memory.borrow_mut().project_for(cwd)
    };
    live_sessions::running(
        files,
        state,
        &metas,
        projects,
        pricing,
        host,
        now,
        &project_of,
    )
}

fn session_record(
    claude: &ClaudePaths,
    session: &str,
    seen: &SessionSeen,
    entrypoint: Option<String>,
    closed: bool,
    memory: &mut SessionMemory,
    now: DateTime<Utc>,
) -> Result<SessionRecord, String> {
    let agents = transcripts::agents_of(claude, session, &seen.spawns);
    let started = count_u32(
        agents.len(),
        &format!("the number of agents started in session {session}"),
    )?;
    let finished = count_u32(
        agents.iter().filter(|agent| agent.finished).count(),
        &format!("the number of agents finished in session {session}"),
    )?;
    Ok(SessionRecord {
        t: now,
        session: session.to_string(),
        cwd: seen.cwd.clone(),
        project: memory.project_for(&seen.cwd),
        entrypoint,
        first: seen.first,
        last: seen.last,
        agents_started: started,
        agents_finished: finished,
        closed,
    })
}

/// A count that does not fit in 32 bits is an error, never `u32::MAX`.
fn count_u32(count: usize, what: &str) -> Result<u32, String> {
    u32::try_from(count)
        .map_err(|_| format!("{what} ({count}) does not fit in 32 bits; it is not written"))
}

/// A record is written when none exists yet, or when the summary differs from
/// the last one written and that line is at least 10 minutes old. `t` is not
/// part of the comparison.
fn record_is_due(memory: &SessionMemory, record: &SessionRecord, now: DateTime<Utc>) -> bool {
    match memory.records.get(&record.session) {
        None => true,
        Some(last) => {
            !same_summary(last, record)
                && now.signed_duration_since(last.t).num_seconds() >= RECORD_EVERY_SECS
        }
    }
}

fn same_summary(first: &SessionRecord, second: &SessionRecord) -> bool {
    first.session == second.session
        && first.cwd == second.cwd
        && first.project == second.project
        && first.entrypoint == second.entrypoint
        && first.first == second.first
        && first.last == second.last
        && first.agents_started == second.agents_started
        && first.agents_finished == second.agents_finished
        && first.closed == second.closed
}

/// The models of the running sessions that have one (lower-case ids). A session
/// without an answer yet adds nothing; only a pass that cannot read the session
/// files has no set at all.
fn models_in_use(running: &[SessionFile], state: &ScanState) -> Option<BTreeSet<String>> {
    let mut models = BTreeSet::new();
    for session in running {
        let Some(model) = state
            .sessions
            .get(&session.session_id)
            .and_then(|seen| seen.last_model.as_deref())
        else {
            continue;
        };
        models.insert(model.to_lowercase());
    }
    Some(models)
}

/// The sessions whose model ties its thinking to the account, and of those the
/// ones busy now (spec §2.11).
fn tied_sessions(running: &[SessionFile], state: &ScanState) -> (u32, u32) {
    let mut tied = 0u32;
    let mut busy = 0u32;
    for session in running {
        let Some(seen) = state.sessions.get(&session.session_id) else {
            continue;
        };
        if !seen
            .last_model
            .as_deref()
            .is_some_and(seatswap_core::burn::tied::thinking_tied)
        {
            continue;
        }
        tied += 1;
        if session.status == SessionStatus::Busy && seen.thinking_since_user > 0 {
            busy += 1;
        }
    }
    (tied, busy)
}

/// Write this host's report when the rows changed and the last one is at least
/// [`live_sessions::REPORT_EVERY_SECS`] old, when a session just closed, or when
/// the last report is older than [`REPORT_HEARTBEAT_SECS`] even though nothing
/// changed. The time is passed in, so tests drive it without sleeping.
fn write_report_if_due(
    recorder: &Recorder,
    sessions: &[LiveSession],
    now: DateTime<Utc>,
    closed_now: bool,
    memory: &mut SessionMemory,
    summary: &mut SessionsSummary,
) {
    let changed = memory.last_rows.as_deref() != Some(sessions);
    let due = closed_now
        || is_due(memory.last_report_at, now, REPORT_HEARTBEAT_SECS)
        || (changed && is_due(memory.last_report_at, now, live_sessions::REPORT_EVERY_SECS));
    if !due {
        return;
    }
    match live_sessions::write_report(recorder.dir(), sessions, now) {
        Ok(_) => {
            memory.last_report_at = Some(now);
            memory.last_rows = Some(sessions.to_vec());
        }
        Err(error) => add_history_error(summary, error),
    }
}

fn add_unavailable(summary: &mut SessionsSummary, text: String) {
    summary.unavailable = Some(match summary.unavailable.take() {
        Some(previous) => format!("{previous}; {text}"),
        None => text,
    });
}

fn add_warning(summary: &mut SessionsSummary, text: String) {
    summary.warning = Some(match summary.warning.take() {
        Some(previous) => format!("{previous}; {text}"),
        None => text,
    });
}

/// Put a warning before the ones already on the summary: the history error is
/// shown before the state error.
fn prepend_warning(summary: &mut SessionsSummary, text: String) {
    summary.warning = Some(match summary.warning.take() {
        Some(next) => format!("{text}; {next}"),
        None => text,
    });
}

fn add_history_error(summary: &mut SessionsSummary, error: seatswap_core::history::HistoryError) {
    add_warning(summary, ops::history_unwritable(&error));
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use seatswap_core::history::{Event, Hosts, Range};
    use seatswap_core::store::Settings;
    use tempfile::TempDir;

    const SESSION: &str = "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34";

    struct FakeProbe(HashMap<u32, String>);

    impl Liveness for FakeProbe {
        fn alive(&self, file: &SessionFile) -> bool {
            self.0.get(&file.pid) == Some(&file.proc_start)
        }
    }

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn fixture() -> (TempDir, ClaudePaths, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("claude");
        fs::create_dir_all(&config).unwrap();
        let claude = ClaudePaths::for_config_dir(&config, tmp.path());
        let app = AppPaths::at(&tmp.path().join("data"));
        fs::create_dir_all(&app.data_dir).unwrap();
        fs::create_dir_all(claude.sessions_dir()).unwrap();
        (tmp, claude, app)
    }

    fn session_json(
        pid: u32,
        session_id: &str,
        cwd: &str,
        name: Option<&str>,
        status: &str,
        status_updated_at: i64,
    ) -> String {
        let mut value = serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
            "cwd": cwd,
            "startedAt": 1_794_612_960_232_i64,
            "procStart": format!("proc-{pid}"),
            "version": "2.1.322",
            "kind": "interactive",
            "entrypoint": "cli",
            "updatedAt": status_updated_at,
            "status": status,
            "statusUpdatedAt": status_updated_at,
        });
        if let Some(name) = name {
            value["name"] = serde_json::json!(name);
        }
        value.to_string()
    }

    fn transcript_line(
        session: &str,
        id: &str,
        at: DateTime<Utc>,
        cwd: &str,
        model: &str,
        usage: (u64, u64, u64, u64),
    ) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": "0b6f4c1a-2d3e-4f50-8a9b-0c1d2e3f4a5b",
            "parentUuid": "91c2a7b3-4c5d-4e6f-8a90-1b2c3d4e5f60",
            "sessionId": session,
            "timestamp": at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "cwd": cwd,
            "gitBranch": "main",
            "isSidechain": false,
            "requestId": format!("req_{id}"),
            "version": "2.1.322",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [{"type": "text", "text": "fix the failing tests"}],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": usage.0,
                    "output_tokens": usage.1,
                    "cache_creation_input_tokens": usage.2,
                    "cache_read_input_tokens": usage.3,
                    "service_tier": "standard",
                }
            }
        })
        .to_string()
    }

    fn write_session(claude: &ClaudePaths, pid: u32, text: &str) {
        fs::write(claude.sessions_dir().join(format!("{pid}.json")), text).unwrap();
    }

    fn probe(pids: &[u32]) -> FakeProbe {
        FakeProbe(
            pids.iter()
                .map(|pid| (*pid, format!("proc-{pid}")))
                .collect(),
        )
    }

    fn history(root: &Path) -> seatswap_core::history::HistoryDir {
        seatswap_core::history::HistoryDir::at(root, "atlas")
    }

    fn repo(home: &Path, name: &str) -> std::path::PathBuf {
        let root = home.join("projects").join(name);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(".git").join("config"),
            format!("[remote \"origin\"]\n\turl = https://github.com/sam/{name}.git\n"),
        )
        .unwrap();
        root
    }

    #[test]
    fn summary_counts_running_busy_and_waiting() {
        let (tmp, claude, app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(history);
        let waiting_since = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            100,
            &session_json(
                100,
                "11111111-1111-4111-8111-111111111111",
                "/home/sam/projects/atlas",
                Some("atlas-busy"),
                "busy",
                waiting_since.timestamp_millis(),
            ),
        );
        write_session(
            &claude,
            101,
            &session_json(
                101,
                "22222222-2222-4222-8222-222222222222",
                "/home/sam/projects/ledger",
                Some("ledger-idle"),
                "idle",
                waiting_since.timestamp_millis(),
            ),
        );
        write_session(
            &claude,
            102,
            &session_json(
                102,
                "33333333-3333-4333-8333-333333333333",
                "/home/sam/projects/atlas",
                Some("atlas-waiting"),
                "waiting",
                waiting_since.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let outcome = run_pass(
            &app,
            &claude,
            &probe(&[100, 101, 102]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
        );
        assert_eq!(outcome.summary.running, 3);
        assert_eq!(outcome.summary.busy, 1);
        assert_eq!(outcome.summary.waiting.len(), 1);
        assert_eq!(outcome.summary.waiting[0].since, waiting_since);
    }

    #[test]
    fn a_session_without_a_name_gets_project_and_two_hex() {
        let (tmp, claude, app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(history);
        let cwd = repo(tmp.path(), "atlas");
        let status_at = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            200,
            &session_json(
                200,
                SESSION,
                cwd.to_str().unwrap(),
                None,
                "waiting",
                status_at.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let outcome = run_pass(
            &app,
            &claude,
            &probe(&[200]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
        );
        assert_eq!(outcome.summary.waiting.len(), 1);
        assert_eq!(outcome.summary.waiting[0].label, "atlas-7c");
        assert_eq!(outcome.summary.waiting[0].project, "atlas");
    }

    #[test]
    fn a_wait_that_ends_is_recorded() {
        let (tmp, claude, _app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(seatswap_core::history::HistoryDir::at(
            history.root(),
            "atlas",
        ));
        let since = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            300,
            &session_json(
                300,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "waiting",
                since.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        run_pass(
            &_app,
            &claude,
            &probe(&[300]),
            Some(&recorder),
            &mut state,
            &mut memory,
            since,
            None,
        );

        let later = time("2026-11-14T10:06:00Z");
        write_session(
            &claude,
            300,
            &session_json(
                300,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                later.timestamp_millis(),
            ),
        );
        run_pass(
            &_app,
            &claude,
            &probe(&[300]),
            Some(&recorder),
            &mut state,
            &mut memory,
            later,
            None,
        );

        let read = history
            .read::<Event>(
                &Range {
                    from: time("2026-11-01T00:00:00Z"),
                    to: time("2026-12-01T00:00:00Z"),
                },
                &Hosts::One("atlas".to_string()),
            )
            .unwrap();
        let waited: Vec<&EventKind> = read
            .records
            .iter()
            .map(|hosted| &hosted.record.kind)
            .filter(|kind| matches!(kind, EventKind::Waited { .. }))
            .collect();
        assert_eq!(waited.len(), 1);
        match waited[0] {
            EventKind::Waited { secs, .. } => assert_eq!(*secs, 600),
            other => panic!("expected a waited event, got {other:?}"),
        }
    }

    /// A session file that cannot be parsed (Claude Code rewrote it and the
    /// thread read it half-written) must not look like a session that ended:
    /// the thread cannot tell, so it must not write a closing record.
    #[test]
    fn a_session_file_that_cannot_be_read_does_not_close_the_session() {
        let (tmp, claude, _app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(seatswap_core::history::HistoryDir::at(
            history.root(),
            "atlas",
        ));
        let first = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            500,
            &session_json(
                500,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                first.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        state.sessions.insert(
            SESSION.to_string(),
            SessionSeen {
                folder: "-home-sam-projects-atlas".to_string(),
                cwd: "/home/sam/projects/atlas".to_string(),
                first,
                last: first,
                last_model: Some("claude-opus-5-5".to_string()),
                last_user_turn: None,
                thinking_since_user: 0,
                totals: Default::default(),
                by_model: Default::default(),
                agents: Default::default(),
                spawns: Default::default(),
                recent: Default::default(),
                recorded: None,
            },
        );
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        run_pass(
            &_app,
            &claude,
            &probe(&[500]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first,
            None,
        );

        // The file is half-written: it does not parse any more.
        fs::write(claude.sessions_dir().join("500.json"), "{\"pid\":500,").unwrap();
        let second = time("2026-11-14T10:00:00Z");
        run_pass(
            &_app,
            &claude,
            &probe(&[500]),
            Some(&recorder),
            &mut state,
            &mut memory,
            second,
            None,
        );

        let file = tmp
            .path()
            .join("history")
            .join("atlas")
            .join("sessions-2026-11.jsonl");
        let text = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "a file that cannot be read must not close the session: {text}"
        );
    }

    /// The wall clock moved backwards between the start of the wait and its
    /// end. The elapsed time is negative and does not fit the event's `secs`;
    /// the thread must not quietly write "0 seconds".
    #[test]
    fn a_wait_that_ends_after_the_clock_moved_backwards_is_not_written_as_zero() {
        let (tmp, claude, _app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(seatswap_core::history::HistoryDir::at(
            history.root(),
            "atlas",
        ));
        let since = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            600,
            &session_json(
                600,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "waiting",
                since.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        run_pass(
            &_app,
            &claude,
            &probe(&[600]),
            Some(&recorder),
            &mut state,
            &mut memory,
            since,
            None,
        );

        // The clock jumps six minutes back; the session is busy again.
        let earlier = time("2026-11-14T09:50:00Z");
        write_session(
            &claude,
            600,
            &session_json(
                600,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                earlier.timestamp_millis(),
            ),
        );
        run_pass(
            &_app,
            &claude,
            &probe(&[600]),
            Some(&recorder),
            &mut state,
            &mut memory,
            earlier,
            None,
        );

        let read = history
            .read::<Event>(
                &Range {
                    from: time("2026-11-01T00:00:00Z"),
                    to: time("2026-12-01T00:00:00Z"),
                },
                &Hosts::One("atlas".to_string()),
            )
            .unwrap();
        for hosted in &read.records {
            if let EventKind::Waited { secs, .. } = &hosted.record.kind {
                assert_ne!(
                    *secs, 0,
                    "a wait that ended after the clock moved backwards must not be written as 0 seconds"
                );
            }
        }
    }

    #[test]
    fn a_closed_session_gets_one_closing_record() {
        let (tmp, claude, _app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(seatswap_core::history::HistoryDir::at(
            history.root(),
            "atlas",
        ));
        let first = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            400,
            &session_json(
                400,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                first.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        state.sessions.insert(
            SESSION.to_string(),
            SessionSeen {
                folder: "-home-sam-projects-atlas".to_string(),
                cwd: "/home/sam/projects/atlas".to_string(),
                first,
                last: first,
                last_model: Some("claude-opus-5-5".to_string()),
                last_user_turn: None,
                thinking_since_user: 0,
                totals: Default::default(),
                by_model: Default::default(),
                agents: Default::default(),
                spawns: Default::default(),
                recent: Default::default(),
                recorded: None,
            },
        );
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        run_pass(
            &_app,
            &claude,
            &probe(&[400]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first,
            None,
        );

        fs::remove_file(claude.sessions_dir().join("400.json")).unwrap();
        let second = time("2026-11-14T10:00:00Z");
        run_pass(
            &_app,
            &claude,
            &probe(&[400]),
            Some(&recorder),
            &mut state,
            &mut memory,
            second,
            None,
        );
        let third = time("2026-11-14T10:02:00Z");
        run_pass(
            &_app,
            &claude,
            &probe(&[400]),
            Some(&recorder),
            &mut state,
            &mut memory,
            third,
            None,
        );

        let file = tmp
            .path()
            .join("history")
            .join("atlas")
            .join("sessions-2026-11.jsonl");
        let text = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let last: SessionRecord = serde_json::from_str(lines[1]).unwrap();
        assert!(last.closed);
    }

    /// The order of the pop-up's messages: the history error comes before the
    /// state that cannot be written.
    #[test]
    fn a_history_error_comes_before_a_state_that_cannot_be_written() {
        let (tmp, claude, app) = fixture();
        let folder = claude.projects_dir().join("-home-sam-projects-atlas");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{SESSION}.jsonl")),
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "user",
                    "timestamp": "2026-11-14T09:56:00Z",
                    "sessionId": SESSION,
                    "cwd": "/home/sam/projects/atlas",
                    "message": {"role": "user", "content": "fix the failing tests"},
                })
            ),
        )
        .unwrap();
        // The state loads while its folder is still a folder. The user line
        // above changes the state and holds no tokens, so the save tries to
        // write that folder without a recorder.
        let (mut state, state_error) = load_state(&app);
        assert_eq!(state_error, None);
        fs::write(app.transcripts_state_dir(), "not a folder").unwrap();

        // A history folder that is a file cannot be opened.
        let history_file = tmp.path().join("history-is-a-file");
        fs::write(&history_file, "").unwrap();
        let settings = Settings {
            host_name: Some("atlas".to_string()),
            history_dir: Some(history_file),
            ..Settings::default()
        };
        let (recorder, history_error) = match ops::open_history(&app, &settings) {
            Ok(recorder) => (Some(recorder), None),
            Err(error) => (None, Some(error)),
        };
        assert!(recorder.is_none(), "the history folder is a file");
        let history_error = history_error.expect("a history folder that is a file is an error");

        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        let outcome = one_pass(
            &app,
            &claude,
            &probe(&[]),
            recorder.as_ref(),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            state_error.as_deref(),
            Some(history_error.as_str()),
        );
        let warning = outcome.summary.warning.expect("both errors are reported");
        assert!(
            warning.starts_with(&format!(
                "{history_error}; the transcript state cannot be written: "
            )),
            "{warning}"
        );
        assert_eq!(
            outcome.summary.unavailable, None,
            "the counts stay visible: only unreadable session files are unavailable"
        );
    }

    /// The first start reads a projects folder it has never seen and writes one
    /// token line per session, hour and model; a second start, from the saved
    /// state, adds none. The data is made here: three sessions, two days, about
    /// forty answers, two models, several answers in one hour.
    #[test]
    fn a_backfill_writes_each_hour_once() {
        use chrono::Timelike;
        use std::collections::BTreeMap;

        const LEDGER: &str = "2b7d9c1e-5a4f-4c3b-9e8d-7f6a5b4c3d2e";
        const ACME_KB: &str = "4f8a1c3e-6b2d-4a5c-9e7f-1a3b5c7d9e20";
        const OPUS: &str = "claude-opus-5-5";
        const SONNET: &str = "claude-sonnet-5-5";

        #[derive(Debug, Default, Clone, PartialEq, Eq)]
        struct Totals {
            input: u64,
            output: u64,
            cache_write: u64,
            cache_read: u64,
            answers: u32,
        }

        impl Totals {
            fn add(&mut self, usage: (u64, u64, u64, u64)) {
                self.input += usage.0;
                self.output += usage.1;
                self.cache_write += usage.2;
                self.cache_read += usage.3;
                self.answers += 1;
            }
        }

        fn start_of_hour(at: DateTime<Utc>) -> DateTime<Utc> {
            at - chrono::Duration::minutes(i64::from(at.minute()))
                - chrono::Duration::seconds(i64::from(at.second()))
                - chrono::Duration::nanoseconds(i64::from(at.nanosecond()))
        }

        fn project_folder(cwd: &Path) -> String {
            cwd.to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect()
        }

        fn files_under(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
            let mut files = BTreeMap::new();
            let mut folders = vec![root.to_path_buf()];
            while let Some(folder) = folders.pop() {
                let entries = match fs::read_dir(&folder) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => panic!("{}: {error}", folder.display()),
                };
                for entry in entries {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        folders.push(path);
                    } else if path.file_name().and_then(|name| name.to_str())
                        != Some("sessions-now.json")
                    {
                        // The live report is not a history line: it is rewritten
                        // each time this host's rows are reported.
                        files.insert(path.clone(), fs::read(&path).unwrap());
                    }
                }
            }
            files
        }

        fn token_lines(root: &Path) -> usize {
            files_under(root)
                .iter()
                .filter(|(path, _)| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("tokens-") && name.ends_with(".jsonl"))
                })
                .map(|(_, bytes)| bytes.iter().filter(|byte| **byte == b'\n').count())
                .sum()
        }

        let (tmp, claude, app) = fixture();
        let mut cwds = HashMap::new();
        for name in ["atlas", "ledger", "acme-kb"] {
            cwds.insert(name, repo(tmp.path(), name));
        }

        // (project, session, first answer, model, answers in that hour)
        let groups: [(&str, &str, &str, &str, u32); 15] = [
            ("atlas", SESSION, "2026-11-12T09:10:00Z", OPUS, 3),
            ("atlas", SESSION, "2026-11-12T09:30:00Z", SONNET, 2),
            ("atlas", SESSION, "2026-11-12T10:05:00Z", OPUS, 2),
            ("atlas", SESSION, "2026-11-12T14:20:00Z", SONNET, 3),
            ("atlas", SESSION, "2026-11-13T08:10:00Z", OPUS, 4),
            ("atlas", SESSION, "2026-11-13T09:15:00Z", SONNET, 3),
            ("atlas", SESSION, "2026-11-13T12:30:00Z", OPUS, 3),
            ("ledger", LEDGER, "2026-11-12T11:05:00Z", OPUS, 4),
            ("ledger", LEDGER, "2026-11-12T13:20:00Z", SONNET, 3),
            ("ledger", LEDGER, "2026-11-13T10:10:00Z", SONNET, 3),
            ("ledger", LEDGER, "2026-11-13T11:40:00Z", OPUS, 4),
            ("acme-kb", ACME_KB, "2026-11-12T09:05:00Z", SONNET, 2),
            ("acme-kb", ACME_KB, "2026-11-12T16:20:00Z", OPUS, 3),
            ("acme-kb", ACME_KB, "2026-11-13T08:40:00Z", OPUS, 4),
            ("acme-kb", ACME_KB, "2026-11-13T09:50:00Z", SONNET, 3),
        ];

        let mut lines: HashMap<&str, Vec<String>> = HashMap::new();
        let mut expected: BTreeMap<(String, DateTime<Utc>, String), Totals> = BTreeMap::new();
        let mut per_session: BTreeMap<String, Totals> = BTreeMap::new();
        let mut sequence = 0u32;
        for (project, session, start, model, count) in groups {
            let cwd = cwds[project].to_str().unwrap().to_string();
            for answer in 0..count {
                let at = time(start) + chrono::Duration::minutes(i64::from(answer) * 3);
                // Input in the thousands, cache reads in the tens of thousands,
                // output in the hundreds.
                let usage = (
                    2_400 + u64::from(sequence) * 91,
                    260 + u64::from(sequence) * 7,
                    17_000 + u64::from(sequence) * 173,
                    31_000 + u64::from(sequence) * 257,
                );
                expected
                    .entry((session.to_string(), start_of_hour(at), model.to_string()))
                    .or_default()
                    .add(usage);
                per_session
                    .entry(session.to_string())
                    .or_default()
                    .add(usage);
                lines.entry(session).or_default().push(transcript_line(
                    session,
                    &format!("msg_{sequence:04}"),
                    at,
                    &cwd,
                    model,
                    usage,
                ));
                sequence += 1;
            }
        }
        assert_eq!(sequence, 46, "the backfill has about forty answers");

        for (session, session_lines) in &lines {
            let cwd = groups
                .iter()
                .find(|group| group.1 == *session)
                .map(|group| &cwds[group.0])
                .unwrap();
            let folder = claude.projects_dir().join(project_folder(cwd));
            fs::create_dir_all(&folder).unwrap();
            fs::write(
                folder.join(format!("{session}.jsonl")),
                format!("{}\n", session_lines.join("\n")),
            )
            .unwrap();
        }

        let history_root = tmp.path().join("history");
        let history_dir = history(&history_root);
        let first_now = time("2026-11-13T13:00:00Z");
        let (mut state, state_error) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        let recorder = Recorder::new(history(&history_root));
        let outcome = one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first_now,
            state_error.as_deref(),
            None,
        );
        assert_eq!(outcome.summary.unavailable, None);
        assert_eq!(outcome.summary.warning, None);
        assert_eq!(outcome.summary.running, 0);
        assert_eq!(
            token_lines(&history_root),
            expected.len(),
            "one token line per session, hour and model"
        );

        let read = history_dir
            .read::<seatswap_core::history::TokenRecord>(
                &Range {
                    from: time("2026-11-01T00:00:00Z"),
                    to: time("2026-12-01T00:00:00Z"),
                },
                &Hosts::One("atlas".to_string()),
            )
            .unwrap();
        assert_eq!(read.warnings, Vec::<String>::new());
        assert_eq!(read.records.len(), expected.len());
        let mut seen = HashSet::new();
        let mut got_per_session: BTreeMap<String, Totals> = BTreeMap::new();
        for hosted in &read.records {
            let record = &hosted.record;
            let key = (record.session.clone(), record.t, record.model.clone());
            let want = expected
                .get(&key)
                .unwrap_or_else(|| panic!("a token line for {key:?} that has no answers"));
            assert!(seen.insert(key.clone()), "two token lines for {key:?}");
            assert_eq!(record.input, want.input, "{key:?} input");
            assert_eq!(record.out, want.output, "{key:?} output");
            assert_eq!(record.cw, want.cache_write, "{key:?} cache write");
            assert_eq!(record.cr, want.cache_read, "{key:?} cache read");
            assert_eq!(record.answers, want.answers, "{key:?} answers");
            assert_eq!(record.agent_answers, 0);
            let total = got_per_session.entry(record.session.clone()).or_default();
            total.input += record.input;
            total.output += record.out;
            total.cache_write += record.cw;
            total.cache_read += record.cr;
            total.answers += record.answers;
        }
        assert_eq!(seen.len(), expected.len());
        assert_eq!(got_per_session, per_session);

        let before = files_under(&history_root);
        let (mut state, state_error) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        let recorder = Recorder::new(history(&history_root));
        let outcome = one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-13T14:00:00Z"),
            state_error.as_deref(),
            None,
        );
        assert_eq!(outcome.summary.unavailable, None);
        assert_eq!(
            files_under(&history_root),
            before,
            "a second start adds no line to any history file"
        );
    }

    #[test]
    fn tokens_read_while_the_history_is_closed_are_not_lost() {
        let (tmp, claude, app) = fixture();
        let folder = claude.projects_dir().join("-home-sam-projects-atlas");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{SESSION}.jsonl")),
            format!(
                "{}\n",
                transcript_line(
                    SESSION,
                    "msg_01QmB7xT",
                    time("2026-11-14T09:41:07Z"),
                    "/home/sam/projects/atlas",
                    "claude-opus-5-5",
                    (2, 166, 26_173, 28_126),
                )
            ),
        )
        .unwrap();

        // First start: the history cannot be opened (say HostTaken), so there
        // is no recorder; the pass still runs and saves the scan state.
        let (mut state, _) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            None,
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
            Some("the history cannot be written: host atlas is taken"),
        );

        // The owner fixes hostName and restarts: now there is a recorder.
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let (mut state, _) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:05:00Z"),
            None,
            None,
        );

        let read = recorder
            .dir()
            .read::<seatswap_core::history::TokenRecord>(
                &Range {
                    from: time("2026-11-01T00:00:00Z"),
                    to: time("2026-12-01T00:00:00Z"),
                },
                &Hosts::One("atlas".to_string()),
            )
            .unwrap();
        assert_eq!(
            read.records.len(),
            1,
            "the answer read while the history was closed must still reach tokens-…"
        );
    }

    #[test]
    fn a_history_error_keeps_the_counts_visible() {
        let (tmp, claude, app) = fixture();
        write_session(
            &claude,
            100,
            &session_json(
                100,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                time("2026-11-14T09:56:00Z").timestamp_millis(),
            ),
        );
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let (mut state, _) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        let outcome = one_pass(
            &app,
            &claude,
            &probe(&[100]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
            Some("the history cannot be written: host atlas is taken"),
        );
        assert_eq!(outcome.summary.running, 1);
        // panel.js renderFooter shows `unavailable` *instead of* the counts.
        assert!(
            outcome.summary.unavailable.is_none(),
            "unavailable hides `1 sessions · 1 busy` in the footer: {:?}",
            outcome.summary.unavailable
        );
        assert_eq!(
            outcome.summary.warning.as_deref(),
            Some("the history cannot be written: host atlas is taken"),
            "the history error is a warning beside the counts"
        );
    }

    #[test]
    fn a_state_that_cannot_be_read_is_a_warning_beside_the_counts() {
        let (_tmp, claude, app) = fixture();
        write_session(
            &claude,
            100,
            &session_json(
                100,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                time("2026-11-14T09:56:00Z").timestamp_millis(),
            ),
        );
        let (mut state, _) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        let state_error = "the transcript state cannot be read: not a state file";
        let outcome = one_pass(
            &app,
            &claude,
            &probe(&[100]),
            None,
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            Some(state_error),
            None,
        );
        assert_eq!(outcome.summary.running, 1);
        assert_eq!(
            outcome.summary.unavailable, None,
            "the state is not the session files; the counts stay visible"
        );
        assert_eq!(outcome.summary.warning.as_deref(), Some(state_error));
    }

    #[test]
    fn a_short_session_gets_a_session_record() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let (mut state, _) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        // 10:00:00.5 a `claude -p` session has just started: no answer yet.
        write_session(
            &claude,
            100,
            &session_json(
                100,
                SESSION,
                "/home/sam/projects/atlas",
                None,
                "busy",
                time("2026-11-14T10:00:00Z").timestamp_millis(),
            ),
        );
        one_pass(
            &app,
            &claude,
            &probe(&[100]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:01Z"),
            None,
            None,
        );
        // It answers and ends cleanly (its file is removed) before the next wake.
        let folder = claude.projects_dir().join("-home-sam-projects-atlas");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{SESSION}.jsonl")),
            format!(
                "{}\n",
                transcript_line(
                    SESSION,
                    "msg_01QmB7xT",
                    time("2026-11-14T10:00:09Z"),
                    "/home/sam/projects/atlas",
                    "claude-opus-5-5",
                    (2, 166, 26_173, 28_126)
                )
            ),
        )
        .unwrap();
        fs::remove_file(claude.sessions_dir().join("100.json")).unwrap();
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:20Z"),
            None,
            None,
        );
        let read = recorder
            .dir()
            .read::<SessionRecord>(
                &Range {
                    from: time("2026-11-01T00:00:00Z"),
                    to: time("2026-12-01T00:00:00Z"),
                },
                &Hosts::One("atlas".to_string()),
            )
            .unwrap();
        assert!(
            !read.records.is_empty(),
            "the session used tokens but has no sessions-… line"
        );
    }

    /// A session without an answer yet must not turn the whole set of models
    /// into "not known": that would switch a per-model limit to the server's
    /// `is_active` view for one wake.
    #[test]
    fn models_in_use_keeps_the_models_of_the_sessions_that_have_one() {
        const SECOND: &str = "2b7d9c1e-5a4f-4c3b-9e8d-7f6a5b4c3d2e";
        let (_tmp, claude, _app) = fixture();
        let at = time("2026-11-14T10:00:00Z");
        write_session(
            &claude,
            700,
            &session_json(
                700,
                SESSION,
                "/home/sam/projects/atlas",
                None,
                "busy",
                at.timestamp_millis(),
            ),
        );
        write_session(
            &claude,
            701,
            &session_json(
                701,
                SECOND,
                "/home/sam/projects/ledger",
                None,
                "busy",
                at.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        state.sessions.insert(
            SESSION.to_string(),
            SessionSeen {
                folder: "-home-sam-projects-atlas".to_string(),
                cwd: "/home/sam/projects/atlas".to_string(),
                first: at,
                last: at,
                last_model: Some("claude-opus-5-5".to_string()),
                last_user_turn: None,
                thinking_since_user: 0,
                totals: Default::default(),
                by_model: Default::default(),
                agents: Default::default(),
                spawns: Default::default(),
                recent: Default::default(),
                recorded: None,
            },
        );
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        let outcome = run_pass(
            &_app,
            &claude,
            &probe(&[700, 701]),
            None,
            &mut state,
            &mut memory,
            at,
            None,
        );
        assert_eq!(
            outcome.models,
            Some(BTreeSet::from(["claude-opus-5-5".to_string()])),
            "the session without an answer adds nothing and does not clear the set"
        );
    }

    /// A closed session that answers again gets one new closed record; an
    /// unchanged one gets none.
    #[test]
    fn a_closed_session_that_answers_again_gets_a_new_closed_record() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let folder = claude.projects_dir().join("-home-sam-projects-atlas");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{SESSION}.jsonl"));
        fs::write(
            &path,
            format!(
                "{}\n",
                transcript_line(
                    SESSION,
                    "msg_01",
                    time("2026-11-14T09:41:00Z"),
                    "/home/sam/projects/atlas",
                    "claude-opus-5-5",
                    (2, 166, 26_173, 28_126),
                )
            ),
        )
        .unwrap();
        let (mut state, state_error) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            state_error.as_deref(),
            None,
        );

        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str(&format!(
            "{}\n",
            transcript_line(
                SESSION,
                "msg_02",
                time("2026-11-14T09:42:00Z"),
                "/home/sam/projects/atlas",
                "claude-opus-5-5",
                (2, 170, 26_201, 28_150),
            )
        ));
        fs::write(&path, text).unwrap();
        let (mut state, state_error) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        let outcome = one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:01:00Z"),
            state_error.as_deref(),
            None,
        );
        assert_eq!(outcome.summary.unavailable, None);
        // No new answer: no third record.
        let (mut state, state_error) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:02:00Z"),
            state_error.as_deref(),
            None,
        );

        let read = recorder
            .dir()
            .read::<SessionRecord>(
                &Range {
                    from: time("2026-11-01T00:00:00Z"),
                    to: time("2026-12-01T00:00:00Z"),
                },
                &Hosts::One("atlas".to_string()),
            )
            .unwrap();
        assert_eq!(
            read.records.len(),
            2,
            "one closed record per batch of answers"
        );
        assert!(read.records.iter().all(|hosted| hosted.record.closed));
        assert_eq!(read.records[0].record.last, time("2026-11-14T09:41:00Z"));
        assert_eq!(read.records[1].record.last, time("2026-11-14T09:42:00Z"));
    }

    /// A session that closed gets one closing record, and no second one after
    /// a restart. `SessionSeen.recorded` is what remembers the closing record,
    /// so it has to reach the state file: the project folder of a session that
    /// only ends (its transcript does not grow) is not written by the scan.
    #[test]
    fn a_closed_session_gets_no_second_record_after_a_restart() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let folder = claude.projects_dir().join("-home-sam-projects-atlas");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{SESSION}.jsonl")),
            format!(
                "{}\n",
                transcript_line(
                    SESSION,
                    "msg_01QmB7xT",
                    time("2026-11-14T09:41:07Z"),
                    "/home/sam/projects/atlas",
                    "claude-opus-5-5",
                    (2, 166, 26_173, 28_126),
                )
            ),
        )
        .unwrap();

        // 10:00 the session runs and has answered.
        write_session(
            &claude,
            100,
            &session_json(
                100,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                time("2026-11-14T10:00:00Z").timestamp_millis(),
            ),
        );
        let (mut state, _) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[100]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
            None,
        );

        // 10:05 it ends: its session file is gone and its transcript did not
        // grow, so the scan writes nothing and the project folder is not
        // touched.
        fs::remove_file(claude.sessions_dir().join("100.json")).unwrap();
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:05:00Z"),
            None,
            None,
        );

        // The app restarts: the state comes from the file, the memory is empty.
        let (mut state, state_error) = load_state(&app);
        assert_eq!(state_error, None);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:10:00Z"),
            None,
            None,
        );

        let file = tmp
            .path()
            .join("history")
            .join("atlas")
            .join("sessions-2026-11.jsonl");
        let text = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "one open and one closing record, not a second closing one: {text}"
        );
    }

    /// One answer is counted once. The state that remembers it is written
    /// after the token lines, so a state that cannot be written leaves the
    /// offsets on the disk where they were: the next start reads the same
    /// answer again and counts it a second time.
    #[test]
    #[cfg(unix)]
    fn tokens_written_while_the_state_cannot_be_saved_are_not_counted_twice() {
        use std::os::unix::fs::PermissionsExt;

        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let folder = claude.projects_dir().join("-home-sam-projects-atlas");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(format!("{SESSION}.jsonl")),
            format!(
                "{}\n",
                transcript_line(
                    SESSION,
                    "msg_01QmB7xT",
                    time("2026-11-14T09:41:07Z"),
                    "/home/sam/projects/atlas",
                    "claude-opus-5-5",
                    (2, 166, 26_173, 28_126),
                )
            ),
        )
        .unwrap();

        // The state folder cannot be written: the offsets stay on the disk.
        let state_dir = app.transcripts_state_dir();
        fs::create_dir_all(&state_dir).unwrap();
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let (mut state, _) = load_state(&app);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
            None,
        );

        // The state folder works again, and the app restarts: the state on the
        // disk knows nothing about the answer that was read while the save
        // failed.
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let (mut state, state_error) = load_state(&app);
        assert_eq!(state_error, None);
        let mut memory = SessionMemory::new(claude.home().to_path_buf());
        one_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:05:00Z"),
            None,
            None,
        );

        let read = recorder
            .dir()
            .read::<seatswap_core::history::TokenRecord>(
                &Range {
                    from: time("2026-11-01T00:00:00Z"),
                    to: time("2026-12-01T00:00:00Z"),
                },
                &Hosts::One("atlas".to_string()),
            )
            .unwrap();
        assert_eq!(
            read.records.len(),
            1,
            "one answer is counted once, not once per start"
        );
    }

    /// A session file that cannot be parsed leaves the thread unable to tell
    /// which sessions are still running: it must not forget one of them, the
    /// same way it must not close one of them.
    #[test]
    fn a_session_file_that_cannot_be_read_does_not_forget_the_session() {
        let (tmp, claude, _app) = fixture();
        let first = time("2026-11-14T09:56:00Z");
        // The file is half-written: it does not parse any more, so the thread
        // cannot tell which session is still running.
        fs::write(claude.sessions_dir().join("500.json"), "{\"pid\":500,").unwrap();
        let mut state = ScanState::default();
        state.sessions.insert(
            SESSION.to_string(),
            SessionSeen {
                folder: "-home-sam-projects-atlas".to_string(),
                cwd: "/home/sam/projects/atlas".to_string(),
                first,
                last: first,
                last_model: Some("claude-opus-5-5".to_string()),
                last_user_turn: None,
                thinking_since_user: 0,
                totals: Default::default(),
                by_model: Default::default(),
                agents: Default::default(),
                spawns: Default::default(),
                recent: Default::default(),
                recorded: None,
            },
        );
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        run_pass(
            &_app,
            &claude,
            &probe(&[]),
            None,
            &mut state,
            &mut memory,
            first + chrono::Duration::hours(2),
            None,
        );

        assert!(
            state.sessions.contains_key(SESSION),
            "a session file that cannot be read must not make the thread forget a session"
        );
    }

    // ---- this host's report and the other hosts' reports (spec §3.8) ----

    fn report_of(root: &Path, host: &str) -> live_sessions::HostReport {
        let path = history(root).sessions_now_file(host);
        let text =
            fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&text).expect("a report as write_report writes it")
    }

    #[test]
    fn the_report_is_written_at_most_every_thirty_seconds() {
        let (tmp, claude, app) = fixture();
        let root = tmp.path().join("history");
        let recorder = Recorder::new(history(&root));
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let first = time("2026-11-14T10:00:00Z");
        write_session(
            &claude,
            400,
            &session_json(
                400,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                first.timestamp_millis(),
            ),
        );

        run_pass(
            &app,
            &claude,
            &probe(&[400]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first,
            None,
        );
        let report = report_of(&root, "atlas");
        assert_eq!(report.at, first);
        assert_eq!(report.sessions.len(), 1);

        // The rows changed 10 s later: the report waits for the 30 s.
        let changed = first + chrono::Duration::seconds(10);
        write_session(
            &claude,
            400,
            &session_json(
                400,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "idle",
                changed.timestamp_millis(),
            ),
        );
        run_pass(
            &app,
            &claude,
            &probe(&[400]),
            Some(&recorder),
            &mut state,
            &mut memory,
            changed,
            None,
        );
        assert_eq!(report_of(&root, "atlas").at, first);

        // At 30 s the change is reported.
        let due = first + chrono::Duration::seconds(30);
        run_pass(
            &app,
            &claude,
            &probe(&[400]),
            Some(&recorder),
            &mut state,
            &mut memory,
            due,
            None,
        );
        let report = report_of(&root, "atlas");
        assert_eq!(report.at, due);
        assert_eq!(report.sessions[0].status, live_sessions::LiveStatus::Idle);
    }

    #[test]
    fn a_quiet_host_still_reports_every_five_minutes() {
        let (tmp, claude, app) = fixture();
        let root = tmp.path().join("history");
        let recorder = Recorder::new(history(&root));
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let first = time("2026-11-14T10:00:00Z");

        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first,
            None,
        );
        assert!(report_of(&root, "atlas").sessions.is_empty());
        assert_eq!(report_of(&root, "atlas").at, first);

        // Nothing changed and four minutes have not passed: no new report.
        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first + chrono::Duration::seconds(299),
            None,
        );
        assert_eq!(report_of(&root, "atlas").at, first);

        // The heartbeat writes the same empty rows again after five minutes.
        let heartbeat = first + chrono::Duration::seconds(301);
        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            heartbeat,
            None,
        );
        let report = report_of(&root, "atlas");
        assert_eq!(report.at, heartbeat);
        assert!(report.sessions.is_empty(), "an empty list is a report too");
    }

    #[test]
    fn a_closed_session_is_reported_at_once() {
        let (tmp, claude, app) = fixture();
        let root = tmp.path().join("history");
        let recorder = Recorder::new(history(&root));
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let first = time("2026-11-14T10:00:00Z");
        write_session(
            &claude,
            500,
            &session_json(
                500,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                "busy",
                first.timestamp_millis(),
            ),
        );
        run_pass(
            &app,
            &claude,
            &probe(&[500]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first,
            None,
        );
        assert_eq!(report_of(&root, "atlas").sessions.len(), 1);

        fs::remove_file(claude.sessions_dir().join("500.json")).unwrap();
        let closed = first + chrono::Duration::seconds(10);
        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            closed,
            None,
        );
        let report = report_of(&root, "atlas");
        assert_eq!(report.at, closed, "a close is reported at once");
        assert!(report.sessions.is_empty());
    }

    // ---- proof tests of the C2-3 review ----

    fn vega_waiting_row(since: DateTime<Utc>) -> LiveSession {
        LiveSession {
            session: "dd5e0c1a-3b7f-4c2d-9e8a-6f1b2c3d4e5f".to_string(),
            host: "vega".to_string(),
            label: "acme-kb-dd".to_string(),
            project: "acme-kb".to_string(),
            org: None,
            cwd: "/home/sam/projects/acme-kb".to_string(),
            provider: "Claude Code".to_string(),
            model: Some("claude-opus-5-5".to_string()),
            tied: false,
            status: LiveStatus::Waiting,
            since,
            started: since,
            agents_running: 0,
            agents_done: 0,
            agents: Vec::new(),
            tokens: Default::default(),
            last_hour: Default::default(),
            values: live_sessions::Values {
                api: None,
                api_currency: None,
                api_missing: None,
                subscription: None,
                currency: None,
                subscription_missing: None,
            },
        }
    }

    #[test]
    fn review_other_hosts_waiting_session_stays_on_the_passes_between_two_reads() {
        let (tmp, claude, app) = fixture();
        let root = tmp.path().join("history");
        let recorder = Recorder::new(history(&root));
        let first = time("2026-11-14T10:00:00Z");
        let vega = seatswap_core::history::HistoryDir::at(&root, "vega");
        fs::create_dir_all(root.join("vega")).unwrap();
        fs::write(
            root.join("vega").join("host.json"),
            "{\"machineId\":\"made-up\"}",
        )
        .unwrap();
        let since = first - chrono::Duration::minutes(2);
        live_sessions::write_report(&vega, &[vega_waiting_row(since)], first).unwrap();
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());

        let one = run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first,
            None,
        );
        assert_eq!(
            one.summary.waiting.len(),
            1,
            "the read pass lists vega's wait"
        );
        let two = run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first + chrono::Duration::seconds(5),
            None,
        );
        assert_eq!(
            two.summary.waiting.len(),
            1,
            "a pass between two reads drops vega's wait, so notify.rs withdraws the notification and sends it again at the next read"
        );
    }

    #[test]
    fn review_this_hosts_waiting_session_does_not_need_the_history() {
        let (tmp, claude, app) = fixture();
        let status_at = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            600,
            &session_json(
                600,
                SESSION,
                "/home/sam/projects/acme-kb",
                Some("acme-kb-dd"),
                "waiting",
                status_at.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let outcome = run_pass(
            &app,
            &claude,
            &probe(&[600]),
            None,
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
        );
        assert_eq!(
            outcome.summary.waiting.len(),
            1,
            "before C2-3 a waiting session notified without a history folder"
        );
    }

    #[test]
    fn review_this_hosts_waiting_session_survives_a_broken_prices_file() {
        let (tmp, claude, app) = fixture();
        let root = tmp.path().join("history");
        let recorder = Recorder::new(history(&root));
        fs::write(app.prices_file(), "{not json").unwrap();
        let status_at = time("2026-11-14T09:56:00Z");
        write_session(
            &claude,
            601,
            &session_json(
                601,
                SESSION,
                "/home/sam/projects/acme-kb",
                Some("acme-kb-dd"),
                "waiting",
                status_at.timestamp_millis(),
            ),
        );
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let outcome = run_pass(
            &app,
            &claude,
            &probe(&[601]),
            Some(&recorder),
            &mut state,
            &mut memory,
            time("2026-11-14T10:00:00Z"),
            None,
        );
        assert!(outcome.summary.unavailable.is_some(), "the context fails");
        assert_eq!(
            outcome.summary.waiting.len(),
            1,
            "a broken prices file must not switch off this host's waiting notifications"
        );
    }

    #[test]
    fn review_a_clock_that_moved_back_does_not_silence_the_report() {
        let (tmp, claude, app) = fixture();
        let root = tmp.path().join("history");
        let recorder = Recorder::new(history(&root));
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        let first = time("2026-11-14T10:00:00Z");
        let file = |status: &str, at: DateTime<Utc>| {
            session_json(
                700,
                SESSION,
                "/home/sam/projects/atlas",
                Some("atlas-a6"),
                status,
                at.timestamp_millis(),
            )
        };
        write_session(&claude, 700, &file("busy", first));
        run_pass(
            &app,
            &claude,
            &probe(&[700]),
            Some(&recorder),
            &mut state,
            &mut memory,
            first,
            None,
        );

        let back = first - chrono::Duration::hours(1);
        write_session(&claude, 700, &file("idle", back));
        run_pass(
            &app,
            &claude,
            &probe(&[700]),
            Some(&recorder),
            &mut state,
            &mut memory,
            back,
            None,
        );
        assert_eq!(
            report_of(&root, "atlas").at,
            back,
            "the rows changed and the last report is far away from now: it is due"
        );
    }

    fn transcript_folder(claude: &ClaudePaths, folder: &str) -> PathBuf {
        let path = claude.projects_dir().join(folder);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn append_transcript(claude: &ClaudePaths, folder: &str, session: &str, line: &str) {
        use std::io::Write;
        let dir = transcript_folder(claude, folder);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{session}.jsonl")))
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }

    fn views(received: &Receiver<UiEvent>) -> Vec<SessionsView> {
        let mut out = Vec::new();
        while let Ok(event) = received.try_recv() {
            if let UiEvent::SessionsView(view) = event {
                out.push(view);
            }
        }
        out
    }

    #[test]
    fn nothing_is_emitted_without_a_subscription() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let now = time("2026-11-14T10:00:00Z");
        write_session(
            &claude,
            800,
            &session_json(
                800,
                SESSION,
                "/home/sam/projects/acme-kb",
                Some("acme-kb-7c"),
                "busy",
                now.timestamp_millis(),
            ),
        );
        append_transcript(
            &claude,
            "-home-sam-projects-acme-kb",
            SESSION,
            &transcript_line(
                SESSION,
                "msg_01",
                now,
                "/home/sam/projects/acme-kb",
                "claude-opus-5-5",
                (2, 100, 26_173, 28_126),
            ),
        );
        let (sent, received) = mpsc::channel();
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        memory.ui = Some(sent);
        run_pass(
            &app,
            &claude,
            &probe(&[800]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now,
            None,
        );
        assert!(
            views(&received).is_empty(),
            "a pass without a subscription sends no view"
        );
    }

    #[test]
    fn a_transcript_change_emits_at_most_two_views_a_second() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let now = time("2026-11-14T10:00:00Z");
        write_session(
            &claude,
            801,
            &session_json(
                801,
                SESSION,
                "/home/sam/projects/acme-kb",
                Some("acme-kb-7c"),
                "busy",
                now.timestamp_millis(),
            ),
        );
        append_transcript(
            &claude,
            "-home-sam-projects-acme-kb",
            SESSION,
            &transcript_line(
                SESSION,
                "msg_01",
                now,
                "/home/sam/projects/acme-kb",
                "claude-opus-5-5",
                (2, 1, 26_173, 28_126),
            ),
        );
        let (sent, received) = mpsc::channel();
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        memory.ui = Some(sent);
        memory.subscribe(SessionsOptions::default());
        run_pass(
            &app,
            &claude,
            &probe(&[801]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now,
            None,
        );
        let mut emitted = views(&received);
        assert_eq!(emitted.len(), 1, "the first pass sends one view");

        for index in 1..=10 {
            let at = now + chrono::Duration::milliseconds(index * 90);
            append_transcript(
                &claude,
                "-home-sam-projects-acme-kb",
                SESSION,
                &transcript_line(
                    SESSION,
                    &format!("msg_{:02}", index + 1),
                    at,
                    "/home/sam/projects/acme-kb",
                    "claude-opus-5-5",
                    (2, 100, 26_173, 28_126),
                ),
            );
            run_pass(
                &app,
                &claude,
                &probe(&[801]),
                Some(&recorder),
                &mut state,
                &mut memory,
                at,
                None,
            );
            emitted.extend(views(&received));
        }
        let within_one_second = emitted
            .iter()
            .filter(|view| view.at < now + chrono::Duration::seconds(1))
            .count();
        assert!(
            within_one_second <= 2,
            "{within_one_second} views within one second: {emitted:?}"
        );

        let trailing = now + chrono::Duration::milliseconds(1_040);
        run_pass(
            &app,
            &claude,
            &probe(&[801]),
            Some(&recorder),
            &mut state,
            &mut memory,
            trailing,
            None,
        );
        emitted.extend(views(&received));
        let last = emitted.last().expect("the trailing view");
        assert_eq!(
            last.sessions[0].tokens.out, 1_001,
            "the last view holds the final numbers"
        );
        assert_eq!(last.at, trailing);
    }

    #[test]
    fn a_broken_prices_file_is_said_once_in_the_view_and_in_the_pop_up() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        fs::write(app.prices_file(), "{not json").unwrap();
        let now = time("2026-11-14T10:00:00Z");
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        memory.subscription = Some(SessionsOptions {
            closed: ClosedSince::Day,
            host: None,
        });

        let outcome = run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now,
            None,
        );
        let pop_up = outcome.summary.unavailable.expect("the pop-up says why");
        assert_eq!(
            pop_up.matches("the prices cannot be read").count(),
            1,
            "{pop_up}"
        );
        let view = memory.view().expect("a view after a pass");
        let window = view.unavailable.expect("the window says why");
        assert_eq!(
            window.matches("the prices cannot be read").count(),
            1,
            "{window}"
        );
    }

    #[test]
    fn closed_rows_are_read_once_per_choice() {
        let (tmp, claude, app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(history.clone());
        let now = time("2026-11-14T10:00:00Z");
        let closed_at = now - chrono::Duration::hours(3);
        history
            .append(&SessionRecord {
                t: closed_at,
                session: "11a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b".to_string(),
                cwd: "/home/sam/projects/acme-kb".to_string(),
                project: ProjectKey::Repo("github.com/sam/acme-kb".to_string()),
                entrypoint: Some("cli".to_string()),
                first: closed_at - chrono::Duration::hours(1),
                last: closed_at,
                agents_started: 0,
                agents_finished: 0,
                closed: true,
            })
            .unwrap();
        history
            .append(&seatswap_core::history::TokenRecord {
                t: closed_at - chrono::Duration::minutes(10),
                session: "11a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b".to_string(),
                cwd: "/home/sam/projects/acme-kb".to_string(),
                project: ProjectKey::Repo("github.com/sam/acme-kb".to_string()),
                model: "claude-opus-5-5".to_string(),
                input: 0,
                out: 4_000_000,
                cw: 0,
                cr: 0,
                answers: 1,
                agent_answers: 0,
            })
            .unwrap();
        let (sent, _received) = mpsc::channel();
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        memory.ui = Some(sent);
        memory.subscription = Some(SessionsOptions {
            closed: ClosedSince::Day,
            host: None,
        });

        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now,
            None,
        );
        assert_eq!(
            memory
                .closed_rows
                .get(&ClosedSince::Day)
                .map(|read| read.read_at),
            Some(now)
        );
        assert_eq!(
            memory
                .closed_rows
                .get(&ClosedSince::Day)
                .map(|read| read.rows.sessions.len()),
            Some(1)
        );

        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now + chrono::Duration::seconds(10),
            None,
        );
        assert_eq!(
            memory
                .closed_rows
                .get(&ClosedSince::Day)
                .map(|read| read.read_at),
            Some(now),
            "a later pass with the same choice does not read the closed rows again"
        );

        memory.subscription = Some(SessionsOptions {
            closed: ClosedSince::Week,
            host: None,
        });
        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now + chrono::Duration::seconds(20),
            None,
        );
        assert_eq!(
            memory
                .closed_rows
                .get(&ClosedSince::Week)
                .map(|read| read.read_at),
            Some(now + chrono::Duration::seconds(20)),
            "a changed choice reads again"
        );
        assert_eq!(
            memory
                .closed_rows
                .get(&ClosedSince::Week)
                .map(|read| read.rows.sessions.len()),
            Some(1)
        );
    }

    /// The pushed view is the view of the *subscription* (the plan: the event
    /// is "the view for the subscribed options"). A `View` command with other
    /// options answers its own question; it must not take the subscription's
    /// place.
    #[test]
    fn a_view_command_with_other_options_does_not_change_the_subscription() {
        let (tmp, claude, app) = fixture();
        let history = history(&tmp.path().join("history"));
        let recorder = Recorder::new(history.clone());
        let now = time("2026-11-14T10:00:00Z");
        let closed_at = now - chrono::Duration::hours(3);
        history
            .append(&SessionRecord {
                t: closed_at,
                session: "11a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b".to_string(),
                cwd: "/home/sam/projects/acme-kb".to_string(),
                project: ProjectKey::Repo("github.com/sam/acme-kb".to_string()),
                entrypoint: Some("cli".to_string()),
                first: closed_at - chrono::Duration::hours(1),
                last: closed_at,
                agents_started: 0,
                agents_finished: 0,
                closed: true,
            })
            .unwrap();
        write_session(
            &claude,
            900,
            &session_json(
                900,
                SESSION,
                "/home/sam/projects/acme-kb",
                Some("acme-kb-7c"),
                "busy",
                now.timestamp_millis(),
            ),
        );
        let (sent, received) = mpsc::channel();
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        memory.ui = Some(sent);
        // The subscription wants no closed rows.
        memory.subscribe(SessionsOptions::default());
        run_pass(
            &app,
            &claude,
            &probe(&[900]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now,
            None,
        );

        // The window asks once for the closed rows.
        let answer = answer_view(
            &mut memory,
            SessionsOptions {
                closed: ClosedSince::Day,
                host: None,
            },
            &app,
            Some(&recorder),
        )
        .expect("the command's answer");
        assert!(
            answer
                .sessions
                .iter()
                .any(|row| row.status == LiveStatus::Closed),
            "the command's answer holds the closed rows"
        );

        // The next pushed view is still the subscription's view.
        run_pass(
            &app,
            &claude,
            &probe(&[900]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now + chrono::Duration::seconds(61),
            None,
        );
        let pushed = views(&received);
        let last = pushed.last().expect("a pushed view");
        assert!(
            last.sessions
                .iter()
                .all(|row| row.status != LiveStatus::Closed),
            "the pushed view is the subscription's view (closed: none), but it holds {} rows",
            last.sessions.len()
        );
    }

    /// A pass that cannot read the session files must say so in the window's
    /// view, not only in the pop-up: the window would otherwise keep showing
    /// the old rows and never learn why they are stale.
    #[test]
    fn a_pass_that_cannot_read_the_session_files_says_why_in_the_view() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        let now = time("2026-11-14T10:00:00Z");
        // The sessions folder is a file: it cannot be read.
        fs::remove_dir_all(claude.sessions_dir()).unwrap();
        fs::write(claude.sessions_dir(), "not a folder").unwrap();
        let (sent, _received) = mpsc::channel();
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        memory.ui = Some(sent);
        memory.subscribe(SessionsOptions::default());

        let outcome = run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now,
            None,
        );
        assert!(
            outcome
                .summary
                .unavailable
                .as_deref()
                .unwrap_or_default()
                .contains("running sessions cannot be read"),
            "the pop-up says why: {:?}",
            outcome.summary.unavailable
        );
        let view = memory
            .view()
            .expect("the window's view says why too, not 'no pass yet'");
        assert!(
            view.unavailable
                .as_deref()
                .unwrap_or_default()
                .contains("running sessions cannot be read"),
            "the window says why: {:?}",
            view.unavailable
        );
    }

    /// The view carries the settings' currency, not the currency of a valuer
    /// that could be built: the settings are read even when the prices cannot
    /// be read.
    #[test]
    fn the_view_has_the_settings_currency_when_the_prices_cannot_be_read() {
        let (tmp, claude, app) = fixture();
        let recorder = Recorder::new(history(&tmp.path().join("history")));
        fs::write(
            app.accounts_file(),
            serde_json::json!({
                "version": 1,
                "settings": { "currency": "EUR" },
            })
            .to_string(),
        )
        .unwrap();
        fs::write(app.prices_file(), "{not json").unwrap();
        let now = time("2026-11-14T10:00:00Z");
        let mut state = ScanState::default();
        let mut memory = SessionMemory::new(tmp.path().to_path_buf());
        memory.subscribe(SessionsOptions::default());

        run_pass(
            &app,
            &claude,
            &probe(&[]),
            Some(&recorder),
            &mut state,
            &mut memory,
            now,
            None,
        );
        let view = memory.view().expect("a view after the pass");
        assert_eq!(
            view.currency.as_deref(),
            Some("EUR"),
            "the view carries the settings' currency"
        );
    }

    /// The commands must not starve the thread: after a command the thread has
    /// to wait out the *rest* of its window, not a whole window again. With a
    /// window that is over and over again, the passes (and with them the
    /// pending view) would never come.
    #[test]
    fn the_commands_do_not_starve_the_passes() {
        use std::time::Instant;
        let (tmp, claude, app) = fixture();
        // The projects folder cannot be read: the scan fails, so the thread
        // asks for the next pass in a second.
        fs::write(claude.projects_dir(), "not a folder").unwrap();
        let (ui, received) = mpsc::channel();
        let (engine, _engine_rx) = mpsc::channel();
        let mut handle = spawn(
            app,
            claude,
            Ok(history(&tmp.path().join("history"))),
            ui,
            engine,
        )
        .expect("the sessions thread");
        let client = handle.client();
        let first = received
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the first pass");
        assert!(
            matches!(first, UiEvent::Sessions(_)),
            "the first pass sends its summary"
        );

        // Now the window asks for a view again and again, as a page that polls
        // would.
        let start = Instant::now();
        let mut passes = 0;
        while start.elapsed() < std::time::Duration::from_secs(3) {
            let _ = client.view(SessionsOptions::default());
            std::thread::sleep(std::time::Duration::from_millis(10));
            while let Ok(event) = received.try_recv() {
                if matches!(event, UiEvent::Sessions(_)) {
                    passes += 1;
                }
            }
        }
        handle.stop();
        assert!(
            passes >= 2,
            "the thread must not starve its passes: {passes} passes in 3 s"
        );
    }
}
