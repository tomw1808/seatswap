//! Notices the changes Claude Code makes to the live login (spec §6.4).
//!
//! The watcher looks only at file names and at Claude Code's lock folders; it never
//! reads or writes the login files. After a quiet period it calls `on_change` on its
//! own thread, and only while Claude Code holds none of its locks.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode, EventKind};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};

use crate::locks;
use crate::paths::ClaudePaths;

/// How long the event stream has to stay quiet before `on_change` is considered.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// A burst in a watched folder ([`watch_folder`]) fires at the latest this long
/// after its first change, even while changes keep arriving more often than
/// [`DEBOUNCE`]. The login watch ([`watch`]) has no such bound: a login written
/// many times while Claude Code starts is one change.
pub const DEBOUNCE_MAX: Duration = Duration::from_secs(1);

/// How often a busy Claude Code lock is re-checked.
pub const BUSY_RECHECK: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("file watch: {0}")]
    Notify(#[from] notify::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Whether the login watch sees Claude Code's folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchState {
    /// Every folder that holds a login file exists and is watched.
    Watching,
    /// A folder that holds a login file does not exist yet: Claude Code has not
    /// been set up on this machine. The nearest existing parent is watched for
    /// it, so the folder and a later login are noticed without a restart.
    NotSetUp,
    /// The folders changed in a way the watch cannot follow (a path that is not
    /// a folder, one that cannot be read, a watch the system refused). The text
    /// says which path and why; a later change that fixes it ends this state.
    Failed(String),
}

/// Notices changes to the live login. Dropping it stops the watcher thread and
/// joins it, so no callback runs after `drop` returned.
pub struct Watcher {
    /// Shared with the thread, which moves the watches when a folder appears
    /// or goes away; `None` once dropped.
    backend: Arc<Mutex<Option<RecommendedWatcher>>>,
    state: Arc<Mutex<WatchState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Whether the login folders exist and are watched (see [`WatchState`]).
    pub fn state(&self) -> WatchState {
        lock(&self.state).clone()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Dropping the backend closes the channel, so an idle thread wakes at once.
        let backend = lock(&self.backend).take();
        drop(backend);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Watch the folders that hold the credentials file and the identity file.
/// Both are watched non-recursively (the files are replaced by rename); a folder
/// that is the same for both files is watched once. Events for other files, and
/// pure read-access events, are ignored.
///
/// A missing folder is not an error: Claude Code has not been set up yet
/// ([`WatchState::NotSetUp`]). Its nearest existing parent is watched instead,
/// and when the folder appears the watch moves to it and `on_change` is called
/// (files written before the new watch took hold are then read by the
/// callback). A folder that goes away moves the watch back up the same way: the
/// parent of every existing folder is watched as well, because on Windows the
/// watch of a removed folder ends without an event. No folder is created. A path that exists but is not a folder, or that
/// cannot be read, is an error.
pub fn watch(
    claude: &ClaudePaths,
    on_change: impl Fn() + Send + 'static,
) -> Result<Watcher, WatchError> {
    let names = WatchedNames::of(claude)?;
    let mut folders: Vec<PathBuf> = Vec::new();
    for file in [&claude.credentials_file, &claude.identity_file] {
        let folder = file.parent().ok_or_else(|| WatchError::Io {
            path: file.clone(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "the file has no parent folder"),
        })?;
        if !folders.iter().any(|seen| seen == folder) {
            folders.push(folder.to_path_buf());
        }
    }

    let (events, received) = mpsc::channel();
    let mut backend = RecommendedWatcher::new(events, notify::Config::default())?;
    let mut watched = Vec::new();
    let set_up = follow_folders(&mut backend, &mut watched, &folders)?;
    let state = Arc::new(Mutex::new(if set_up {
        WatchState::Watching
    } else {
        WatchState::NotSetUp
    }));
    let backend = Arc::new(Mutex::new(Some(backend)));

    let stop = Arc::new(AtomicBool::new(false));
    let layout = Layout {
        folders,
        watched,
        backend: Arc::clone(&backend),
        state: Arc::clone(&state),
    };
    let thread_stop = Arc::clone(&stop);
    let thread_paths = claude.clone();
    let spawned = thread::Builder::new()
        .name("seatswap-watcher".to_string())
        .spawn(move || {
            run(
                received,
                thread_stop,
                thread_paths,
                names,
                layout,
                on_change,
            )
        });
    let thread = match spawned {
        Ok(thread) => thread,
        Err(source) => {
            return Err(WatchError::Io {
                path: claude.config_dir.clone(),
                source,
            });
        }
    };

    Ok(Watcher {
        backend,
        state,
        stop,
        thread: Some(thread),
    })
}

/// Where the login watch looks: the folders that hold the login files, the
/// paths watched for them now (each folder, or its nearest existing parent while
/// it is missing), and the state the GUI reads.
struct Layout {
    folders: Vec<PathBuf>,
    watched: Vec<PathBuf>,
    backend: Arc<Mutex<Option<RecommendedWatcher>>>,
    state: Arc<Mutex<WatchState>>,
}

impl Layout {
    /// An event about one of the login folders or a parent of one may have
    /// created, removed or renamed it.
    fn touches(&self, event: &Event) -> bool {
        event
            .paths
            .iter()
            .any(|path| self.folders.iter().any(|folder| folder.starts_with(path)))
    }

    /// Plan the watches again after such an event. Returns true when the
    /// watched paths or the state changed: the login may have changed with them.
    fn follow(&mut self) -> bool {
        let before = self.watched.clone();
        let state = {
            let mut backend = lock(&self.backend);
            let Some(backend) = backend.as_mut() else {
                return false;
            };
            match follow_folders(backend, &mut self.watched, &self.folders) {
                Ok(true) => WatchState::Watching,
                Ok(false) => WatchState::NotSetUp,
                Err(error) => WatchState::Failed(error.to_string()),
            }
        };
        let mut current = lock(&self.state);
        let changed = *current != state || !same_paths(&before, &self.watched);
        *current = state;
        changed
    }
}

/// The path to watch for `folder`: the folder itself when it exists, else its
/// nearest existing parent. The flag says whether the folder itself exists. A
/// path on the way that exists but is not a folder, or cannot be read, is an
/// error.
fn watch_point(folder: &Path) -> Result<(PathBuf, bool), WatchError> {
    let mut candidate = folder;
    loop {
        match fs::metadata(candidate) {
            Ok(metadata) if metadata.is_dir() => {
                return Ok((candidate.to_path_buf(), candidate == folder));
            }
            Ok(_) => {
                return Err(WatchError::Io {
                    path: candidate.to_path_buf(),
                    source: io::Error::other("not a folder"),
                });
            }
            // "Not a directory" means a parent is a file: walking up finds it
            // and names it.
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                candidate = match candidate.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => parent,
                    _ => {
                        return Err(WatchError::Io {
                            path: folder.to_path_buf(),
                            source,
                        })
                    }
                };
            }
            Err(source) => {
                return Err(WatchError::Io {
                    path: candidate.to_path_buf(),
                    source,
                });
            }
        }
    }
}

/// The same paths, in any order (neither list holds a path twice).
fn same_paths(a: &[PathBuf], b: &[PathBuf]) -> bool {
    a.len() == b.len() && a.iter().all(|path| b.contains(path))
}

/// Bring the backend's watches in line with the folders as they are now, and
/// say whether every folder exists. An existing folder is watched together with
/// its parent, where its own removal shows up. New watches are added before old ones are
/// removed, and the plan is checked again afterwards, so a folder that appears
/// in between is still followed.
fn follow_folders(
    backend: &mut RecommendedWatcher,
    watched: &mut Vec<PathBuf>,
    folders: &[PathBuf],
) -> Result<bool, WatchError> {
    loop {
        let mut wanted: Vec<PathBuf> = Vec::new();
        let mut set_up = true;
        for folder in folders {
            let (point, present) = watch_point(folder)?;
            set_up &= present;
            let parent = folder.parent().filter(|_| present);
            for path in std::iter::once(point).chain(parent.map(Path::to_path_buf)) {
                if !wanted.contains(&path) {
                    wanted.push(path);
                }
            }
        }
        if same_paths(&wanted, watched) {
            return Ok(set_up);
        }
        for path in &wanted {
            if watched.contains(path) {
                continue;
            }
            if let Err(error) = backend.watch(path, RecursiveMode::NonRecursive) {
                // The folder went away between the check and the watch: plan
                // again. Any other refusal is the error.
                if path.is_dir() {
                    return Err(error.into());
                }
                continue;
            }
            watched.push(path.clone());
        }
        let gone: Vec<PathBuf> = watched
            .iter()
            .filter(|path| !wanted.contains(path))
            .cloned()
            .collect();
        for path in gone {
            // A folder that was removed has lost its watch already, so the
            // system's answer here says nothing new.
            let _ = backend.unwatch(&path);
            watched.retain(|kept| *kept != path);
        }
    }
}

fn ensure_folder(path: &Path) -> Result<(), WatchError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(WatchError::Io {
            path: path.to_path_buf(),
            source: io::Error::other("not a folder"),
        }),
        Err(source) => Err(WatchError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The two files an event can be about. A watched parent folder may hold
/// another file with the same name (`~/.claude.json` beside a `CLAUDE_CONFIG_DIR`
/// folder), so the whole path counts, not the name.
struct WatchedNames {
    credentials: PathBuf,
    identity: PathBuf,
}

impl WatchedNames {
    fn of(claude: &ClaudePaths) -> Result<WatchedNames, WatchError> {
        file_name(&claude.credentials_file)?;
        file_name(&claude.identity_file)?;
        Ok(WatchedNames {
            credentials: claude.credentials_file.clone(),
            identity: claude.identity_file.clone(),
        })
    }

    /// An event counts when it may have changed one of the watched files. Read
    /// access is not a change; counting it would make the callback (which reads
    /// the files) trigger itself.
    fn is_change(&self, event: &Event) -> bool {
        if is_read_access(&event.kind) {
            return false;
        }
        event.paths.iter().any(|path| self.is_watched(path))
    }

    fn is_watched(&self, path: &Path) -> bool {
        path == self.credentials || path == self.identity
    }
}

fn file_name(path: &Path) -> Result<&OsStr, WatchError> {
    path.file_name().ok_or_else(|| WatchError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "the file has no name"),
    })
}

fn is_read_access(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Access(AccessKind::Open(_))
            | EventKind::Access(AccessKind::Close(AccessMode::Read))
    )
}

#[derive(Clone, Copy)]
enum Phase {
    /// No relevant event seen yet.
    Idle,
    /// A relevant event was seen; wait until it has been quiet for [`DEBOUNCE`],
    /// but never past `deadline` (the burst's first change plus the loop's
    /// maximum wait, when it has one).
    Quiet {
        until: Instant,
        deadline: Option<Instant>,
    },
    /// Quiet period over; wait for Claude Code's locks to be free.
    WaitingForLocks,
}

/// How long the next wait may last. Idle has no timeout: the thread blocks on the
/// event channel until an event arrives or the backend is dropped (which closes
/// the channel and wakes it).
fn wait_timeout(phase: Phase) -> Option<Duration> {
    match phase {
        Phase::Quiet { until, deadline } => {
            Some(fires_at(until, deadline).saturating_duration_since(Instant::now()))
        }
        Phase::WaitingForLocks => Some(BUSY_RECHECK),
        Phase::Idle => None,
    }
}

/// The moment a quiet phase ends: after the quiet time, or at the deadline if
/// that comes first.
fn fires_at(until: Instant, deadline: Option<Instant>) -> Instant {
    deadline.map_or(until, |deadline| until.min(deadline))
}

fn run(
    events: Receiver<notify::Result<Event>>,
    stop: Arc<AtomicBool>,
    claude: ClaudePaths,
    names: WatchedNames,
    mut layout: Layout,
    on_change: impl Fn() + Send + 'static,
) {
    run_debounced(
        events,
        stop,
        |event| {
            // A login folder that appeared or went away counts as a change:
            // login files written before the new watch took hold are read by
            // the callback.
            let moved = !is_read_access(&event.kind) && layout.touches(event) && layout.follow();
            moved || names.is_change(event)
        },
        || !locks::claude_locks_busy(&claude),
        None,
        on_change,
    );
}

/// One folder, watched non-recursively. Dropping it stops the watcher thread and
/// joins it, so no callback runs after `drop` returned.
pub struct FolderWatch {
    backend: Option<RecommendedWatcher>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for FolderWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        drop(self.backend.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Watch one folder non-recursively and call `on_change` on its own thread after
/// the event stream has been quiet for [`DEBOUNCE`], or at the latest
/// [`DEBOUNCE_MAX`] after the first change of a burst. Read access is ignored: the
/// callback reads the folder, which would otherwise make it trigger itself.
pub fn watch_folder(
    folder: &Path,
    on_change: impl Fn() + Send + 'static,
) -> Result<FolderWatch, WatchError> {
    ensure_folder(folder)?;
    let (events, received) = mpsc::channel();
    let mut backend = RecommendedWatcher::new(events, notify::Config::default())?;
    backend.watch(folder, RecursiveMode::NonRecursive)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let spawned = thread::Builder::new()
        .name("seatswap-folder-watch".to_string())
        .spawn(move || {
            run_debounced(
                received,
                thread_stop,
                |event| !is_read_access(&event.kind),
                || true,
                Some(DEBOUNCE_MAX),
                on_change,
            );
        });
    let thread = match spawned {
        Ok(thread) => thread,
        Err(source) => {
            return Err(WatchError::Io {
                path: folder.to_path_buf(),
                source,
            });
        }
    };
    Ok(FolderWatch {
        backend: Some(backend),
        stop,
        thread: Some(thread),
    })
}

/// The debounce loop shared by [`watch`] and [`watch_folder`]: wait for an event
/// that `is_change` accepts, then after [`DEBOUNCE`] quiet (and at the latest
/// `max_wait` after the burst's first change, when given) and while `ready`
/// holds, call `on_change` once.
fn run_debounced(
    events: Receiver<notify::Result<Event>>,
    stop: Arc<AtomicBool>,
    mut is_change: impl FnMut(&Event) -> bool,
    ready: impl Fn() -> bool,
    max_wait: Option<Duration>,
    on_change: impl Fn(),
) {
    let mut phase = Phase::Idle;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if matches!(phase, Phase::WaitingForLocks) && ready() {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            on_change();
            phase = Phase::Idle;
            continue;
        }
        let received = match wait_timeout(phase) {
            Some(timeout) => events.recv_timeout(timeout),
            None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(Ok(event)) => {
                if is_change(&event) {
                    let now = Instant::now();
                    let deadline = match phase {
                        Phase::Quiet { deadline, .. } => deadline,
                        Phase::Idle | Phase::WaitingForLocks => {
                            max_wait.map(|max_wait| now + max_wait)
                        }
                    };
                    phase = Phase::Quiet {
                        until: now + DEBOUNCE,
                        deadline,
                    };
                }
            }
            // An error inside the stream is not fatal: the next event still works.
            Ok(Err(_)) => {}
            Err(RecvTimeoutError::Timeout) => {
                if let Phase::Quiet { until, deadline } = phase {
                    if Instant::now() >= fires_at(until, deadline) {
                        phase = Phase::WaitingForLocks;
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atomic;
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;

    fn config_paths(tmp: &TempDir) -> ClaudePaths {
        let config = tmp.path().join("cfg");
        fs::create_dir(&config).unwrap();
        ClaudePaths::for_config_dir(&config, tmp.path())
    }

    fn home_paths(tmp: &TempDir) -> ClaudePaths {
        fs::create_dir(tmp.path().join(".claude")).unwrap();
        ClaudePaths::for_config_dir(&tmp.path().join(".claude"), tmp.path())
    }

    fn watched(paths: &ClaudePaths) -> (Watcher, Arc<AtomicUsize>, Receiver<()>) {
        let count = Arc::new(AtomicUsize::new(0));
        let (calls, received) = mpsc::channel();
        let counted = Arc::clone(&count);
        let watcher = watch(paths, move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = calls.send(());
        })
        .unwrap();
        (watcher, count, received)
    }

    /// Wait for the single expected call, then make sure no second one follows.
    fn expect_one(received: &Receiver<()>, count: &AtomicUsize) {
        received
            .recv_timeout(Duration::from_secs(3))
            .expect("on_change was not called");
        assert!(
            received.recv_timeout(Duration::from_millis(1200)).is_err(),
            "on_change was called more than once"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn atomic_credentials_write_calls_once() {
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let (watcher, count, received) = watched(&paths);

        atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
        expect_one(&received, &count);
        drop(watcher);
    }

    #[test]
    fn burst_of_writes_calls_once() {
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let (watcher, count, received) = watched(&paths);

        // Claude Code rewrites the file about that often while it starts.
        for _ in 0..20 {
            atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
            thread::sleep(Duration::from_millis(50));
        }
        expect_one(&received, &count);
        drop(watcher);
    }

    #[test]
    fn a_burst_longer_than_the_quiet_time_still_wakes() {
        let tmp = TempDir::new().unwrap();
        let folder = tmp.path().join("subagents");
        fs::create_dir(&folder).unwrap();
        let (calls, received) = mpsc::channel();
        let watch = watch_folder(&folder, move || {
            let _ = calls.send(());
        })
        .unwrap();

        // An agent's transcript grows more often than DEBOUNCE, so a pure
        // trailing debounce would never fire while it writes.
        let transcript = folder.join("agent-a875.jsonl");
        let mut wakes = 0;
        let started = Instant::now();
        let mut line = 0;
        while started.elapsed() < Duration::from_secs(3) {
            line += 1;
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&transcript)
                .unwrap();
            writeln!(file, "{{\"line\":{line}}}").unwrap();
            drop(file);
            thread::sleep(Duration::from_millis(200));
            while received.try_recv().is_ok() {
                wakes += 1;
            }
        }
        assert!(
            wakes >= 2,
            "a 3 s burst every 200 ms woke on_change {wakes} times before it stopped"
        );
        drop(watch);
    }

    #[test]
    fn identity_change_in_the_home_layout_calls_once() {
        let tmp = TempDir::new().unwrap();
        let paths = home_paths(&tmp);
        assert_ne!(
            paths.credentials_file.parent(),
            paths.identity_file.parent(),
            "the home layout has two watched folders"
        );
        let (watcher, count, received) = watched(&paths);

        atomic::write_atomic(&paths.identity_file, b"{}", atomic::Access::Private).unwrap();
        expect_one(&received, &count);
        drop(watcher);
    }

    #[test]
    fn unrelated_files_are_ignored() {
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let (watcher, count, received) = watched(&paths);

        atomic::write_atomic(
            &paths.config_dir.join("settings.json"),
            b"{}",
            atomic::Access::Private,
        )
        .unwrap();
        atomic::write_atomic(
            &paths.config_dir.join("history.jsonl"),
            b"{}\n",
            atomic::Access::Private,
        )
        .unwrap();
        assert!(
            received.recv_timeout(Duration::from_millis(1500)).is_err(),
            "an unrelated file must not call on_change"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(watcher);
    }

    #[test]
    fn a_login_file_name_in_the_watched_parent_is_not_the_login() {
        // The config folder's parent is watched for the folder's removal; the
        // default `~/.claude.json` there is not this config folder's identity.
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let (watcher, count, received) = watched(&paths);
        for name in [".claude.json", ".credentials.json"] {
            atomic::write_atomic(&tmp.path().join(name), b"{}", atomic::Access::Private).unwrap();
        }
        assert!(
            received.recv_timeout(Duration::from_millis(1500)).is_err(),
            "a file of the same name beside the config folder must not call on_change"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(watcher);
    }

    #[test]
    fn busy_storage_lock_delays_the_call() {
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let (watcher, count, received) = watched(&paths);

        let lock = paths.storage_write_lock();
        fs::create_dir(&lock).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let heartbeat_stop = Arc::clone(&stop);
        let heartbeat = thread::spawn(move || {
            while !heartbeat_stop.load(Ordering::SeqCst) {
                let _ = filetime::set_file_mtime(&lock, filetime::FileTime::now());
                thread::sleep(Duration::from_millis(100));
            }
        });

        atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
        assert!(
            received.recv_timeout(Duration::from_millis(1500)).is_err(),
            "on_change must not run while Claude Code's storage lock is fresh"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);

        stop.store(true, Ordering::SeqCst);
        heartbeat.join().unwrap();
        fs::remove_dir(paths.storage_write_lock()).unwrap();

        received
            .recv_timeout(Duration::from_secs(2))
            .expect("on_change must run after the lock is gone");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(watcher);
    }

    /// Wait for at least one call, then for the burst to end.
    fn expect_some(received: &Receiver<()>, what: &str) {
        received
            .recv_timeout(Duration::from_secs(3))
            .unwrap_or_else(|_| panic!("on_change was not called after {what}"));
        while received.recv_timeout(Duration::from_millis(1200)).is_ok() {}
    }

    #[test]
    fn a_missing_config_folder_is_not_set_up_and_noticed_when_it_appears() {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("cfg");
        let paths = ClaudePaths::for_config_dir(&config, tmp.path());
        let (watcher, _count, received) = watched(&paths);
        assert_eq!(watcher.state(), WatchState::NotSetUp);

        fs::create_dir(&config).unwrap();
        expect_some(&received, "the folder appeared");
        assert_eq!(watcher.state(), WatchState::Watching);

        atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
        expect_some(&received, "the first login was written");
        drop(watcher);
    }

    #[test]
    fn claude_codes_first_start_in_the_home_layout_is_noticed() {
        let tmp = TempDir::new().unwrap();
        let paths = ClaudePaths::for_config_dir(&tmp.path().join(".claude"), tmp.path());
        let (watcher, _count, received) = watched(&paths);
        assert_eq!(watcher.state(), WatchState::NotSetUp);

        // Claude Code creates the folder and writes the login right after:
        // the file may land before the new watch holds, and is still noticed.
        fs::create_dir(&paths.config_dir).unwrap();
        atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
        atomic::write_atomic(&paths.identity_file, b"{}", atomic::Access::Private).unwrap();
        expect_some(&received, "the folder and the login appeared");
        assert_eq!(watcher.state(), WatchState::Watching);
        drop(watcher);
    }

    #[test]
    fn a_folder_several_levels_down_is_followed() {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("work").join("claude").join("cfg");
        let paths = ClaudePaths::for_config_dir(&config, tmp.path());
        let (watcher, _count, received) = watched(&paths);
        assert_eq!(watcher.state(), WatchState::NotSetUp);

        fs::create_dir_all(&config).unwrap();
        expect_some(&received, "the folders appeared");
        assert_eq!(watcher.state(), WatchState::Watching);

        atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
        expect_some(&received, "the login was written");
        drop(watcher);
    }

    #[test]
    fn a_removed_folder_is_followed_back_up_and_down() {
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let (watcher, _count, received) = watched(&paths);
        assert_eq!(watcher.state(), WatchState::Watching);

        fs::remove_dir(&paths.config_dir).unwrap();
        expect_some(&received, "the folder was removed");
        assert_eq!(watcher.state(), WatchState::NotSetUp);

        fs::create_dir(&paths.config_dir).unwrap();
        expect_some(&received, "the folder came back");
        assert_eq!(watcher.state(), WatchState::Watching);
        atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
        expect_some(&received, "the login was written again");
        drop(watcher);
    }

    #[test]
    fn a_config_path_that_is_a_file_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("cfg");
        fs::write(&config, b"").unwrap();
        let paths = ClaudePaths::for_config_dir(&config, tmp.path());
        let error = watch(&paths, || {})
            .err()
            .expect("a file is not a config folder");
        assert_eq!(
            error.to_string(),
            format!("{}: not a folder", config.display())
        );

        // A file on the way to the folder can never become it either.
        let below = ClaudePaths::for_config_dir(&config.join("inner"), tmp.path());
        let error = watch(&below, || {})
            .err()
            .expect("a file is not a parent folder");
        assert_eq!(
            error.to_string(),
            format!("{}: not a folder", config.display())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_config_folder_that_cannot_be_read_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let closed = tmp.path().join("closed");
        fs::create_dir(&closed).unwrap();
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
        let config = closed.join("cfg");
        let paths = ClaudePaths::for_config_dir(&config, tmp.path());
        let result = watch(&paths, || {});
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
        let error = result
            .err()
            .expect("a folder behind a closed one cannot be checked");
        assert!(
            error
                .to_string()
                .starts_with(&format!("{}: ", config.display())),
            "the error names the path: {error}"
        );
        assert!(error.to_string().contains("ermission denied"), "{error}");
    }

    #[test]
    fn idle_waits_without_a_timeout() {
        assert_eq!(wait_timeout(Phase::Idle), None);
        assert_eq!(wait_timeout(Phase::WaitingForLocks), Some(BUSY_RECHECK));
    }

    #[test]
    fn drop_while_idle_returns_within_a_second() {
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let watcher = watch(&paths, || {}).unwrap();

        let started = Instant::now();
        drop(watcher);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "dropping an idle watcher took {elapsed:?}"
        );
    }

    #[test]
    fn no_call_after_drop() {
        let tmp = TempDir::new().unwrap();
        let paths = config_paths(&tmp);
        let count = Arc::new(AtomicUsize::new(0));
        let (calls, received) = mpsc::channel();
        // Keep the channel connected after the callback (and its sender) is gone.
        let keep = calls.clone();
        let counted = Arc::clone(&count);
        let watcher = watch(&paths, move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = calls.send(());
        })
        .unwrap();
        drop(watcher);

        atomic::write_atomic(&paths.credentials_file, b"{}", atomic::Access::Private).unwrap();
        assert!(
            received.recv_timeout(Duration::from_millis(1500)).is_err(),
            "no callback may run after the watcher is dropped"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(keep);
    }
}
