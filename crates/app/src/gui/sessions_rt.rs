//! The sessions thread: Claude Code's running sessions and the transcript
//! scanner (spec §1.6).
//!
//! It runs only when `readSessions` is on. Every wake it asks which sessions are
//! alive, reads the new parts of their transcripts, writes the tokens and the
//! session records to the history, then hands a [`SessionsSummary`] to the main
//! thread and the models in use to the engine thread.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{DateTime, Utc};

use seatswap_core::history::{EventKind, HistoryDir, Recorder, SessionRecord};
use seatswap_core::paths::{AppPaths, ClaudePaths};
use seatswap_core::project::{self, ProjectKey};
use seatswap_core::projects;
use seatswap_core::sessions::{self, Liveness, SessionFile, SessionStatus, SystemProbe};
use seatswap_core::transcripts::{self, RecordedSession, ScanState, SessionSeen};
use seatswap_core::watcher::{watch_folder, FolderWatch, WatchError};

use super::worker::{EngineCommand, SessionsSummary, UiEvent, Waiting};
use crate::ops;

/// The regular wake, so a session folder that cannot be watched still works.
const WAKE: Duration = Duration::from_secs(60);
/// After a scan that did not finish, read the next piece soon.
const AFTER_INCOMPLETE: Duration = Duration::from_secs(1);
/// One transcript scan reads at most this much.
const SCAN_BUDGET: u64 = 64 * 1024 * 1024;
/// A session record is rewritten at most this often while its summary changes.
const RECORD_EVERY_SECS: i64 = 10 * 60;

/// What one pass found, for the thread and the tests.
#[derive(Debug, Clone)]
pub(crate) struct PassOutcome {
    pub summary: SessionsSummary,
    pub models: Option<BTreeSet<String>>,
    pub tied_busy: u32,
    pub wake_soon: bool,
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
}

impl SessionMemory {
    pub(crate) fn new(home: std::path::PathBuf) -> SessionMemory {
        SessionMemory {
            home,
            projects: HashMap::new(),
            waiting: HashMap::new(),
            records: HashMap::new(),
            closed: HashSet::new(),
        }
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

struct WaitingMemory {
    project: ProjectKey,
    since: DateTime<Utc>,
}

/// The handle the tray keeps. Dropping it, or calling [`stop`](Self::stop),
/// stops and joins the thread.
pub(crate) struct SessionsHandle {
    stop: Arc<AtomicBool>,
    wake: Sender<()>,
    thread: Option<JoinHandle<()>>,
    latest: Arc<Mutex<Option<SessionsSummary>>>,
}

impl SessionsHandle {
    /// The summary of the last pass, shared with the action thread so a switch
    /// can record how many sessions it interrupted.
    pub(crate) fn latest(&self) -> Arc<Mutex<Option<SessionsSummary>>> {
        Arc::clone(&self.latest)
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
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
    let (wake, wake_rx) = mpsc::channel::<()>();
    let latest = Arc::new(Mutex::new(None));
    let thread_stop = Arc::clone(&stop);
    let thread_latest = Arc::clone(&latest);
    let watch_wake = wake.clone();
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
                wake_rx,
                watch_wake,
                thread_latest,
            )
        })
        .map_err(|error| format!("cannot start the sessions thread: {error}"))?;
    Ok(SessionsHandle {
        stop,
        wake,
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
    wake_rx: Receiver<()>,
    watch_wake: Sender<()>,
    latest: Arc<Mutex<Option<SessionsSummary>>>,
) {
    let (recorder, history_error) = match history {
        Ok(history) => (Some(Recorder::new(history)), None),
        Err(error) => (None, Some(error)),
    };
    let (mut state, state_error) = load_state(&app);
    let mut memory = SessionMemory::new(claude.home().to_path_buf());
    let mut watch: Option<FolderWatch> = None;
    let mut last_watch_error: Option<String> = None;

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if watch.is_none() {
            let wake = watch_wake.clone();
            match watch_folder(&claude.sessions_dir(), move || {
                let _ = wake.send(());
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
                PassOutcome {
                    summary,
                    models: None,
                    tied_busy: 0,
                    wake_soon: false,
                }
            }
        };

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

        let wait = if outcome.wake_soon {
            AFTER_INCOMPLETE
        } else {
            WAKE
        };
        match wake_rx.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
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
            return PassOutcome {
                summary,
                models: None,
                tied_busy: 0,
                wake_soon: false,
            };
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

    // Waiting sessions, and the waits that just ended.
    let mut still_waiting: HashSet<String> = HashSet::new();
    for session in &running.sessions {
        if session.status != SessionStatus::Waiting {
            continue;
        }
        let Some(since) = DateTime::<Utc>::from_timestamp_millis(session.status_updated_at) else {
            add_warning(
                &mut summary,
                format!(
                    "session {} has an invalid statusUpdatedAt {}",
                    session.session_id, session.status_updated_at
                ),
            );
            continue;
        };
        let project = memory.project_for(&session.cwd);
        summary.waiting.push(Waiting {
            session: session.session_id.clone(),
            label: label_of(session, &project),
            project: projects::default_name(&project),
            since,
        });
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

    let models = models_in_use(&running.sessions, state);
    let (_, tied_busy) = tied_sessions(&running.sessions, state);
    PassOutcome {
        summary,
        models,
        tied_busy,
        wake_soon: !scan_complete,
    }
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

fn label_of(session: &SessionFile, project: &ProjectKey) -> String {
    if let Some(name) = &session.name {
        return name.clone();
    }
    let prefix: String = session.session_id.chars().take(2).collect();
    format!("{}-{prefix}", projects::default_name(project))
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
        let (tmp, claude, _app) = fixture();
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
            &_app,
            &claude,
            &probe(&[100, 101, 102]),
            None,
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
        let (tmp, claude, _app) = fixture();
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
            &_app,
            &claude,
            &probe(&[200]),
            None,
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
                    } else {
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
}
