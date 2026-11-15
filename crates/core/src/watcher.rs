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
use std::sync::Arc;
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

/// Notices changes to the live login. Dropping it stops the watcher thread and
/// joins it, so no callback runs after `drop` returned.
pub struct Watcher {
    backend: Option<RecommendedWatcher>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Dropping the backend closes the channel, so an idle thread wakes at once.
        drop(self.backend.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Watch the folders that hold the credentials file and the identity file.
/// Both are watched non-recursively (the files are replaced by rename); a folder
/// that is the same for both files is watched once. Events for other file names,
/// and pure read-access events, are ignored. A missing folder is an error; no
/// folder is created.
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
    for folder in &folders {
        ensure_folder(folder)?;
    }

    let (events, received) = mpsc::channel();
    let mut backend = RecommendedWatcher::new(events, notify::Config::default())?;
    for folder in &folders {
        backend.watch(folder, RecursiveMode::NonRecursive)?;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_paths = claude.clone();
    let spawned = thread::Builder::new()
        .name("seatswap-watcher".to_string())
        .spawn(move || run(received, thread_stop, thread_paths, names, on_change));
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
        backend: Some(backend),
        stop,
        thread: Some(thread),
    })
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

/// The two file names an event can be about.
struct WatchedNames {
    credentials: std::ffi::OsString,
    identity: std::ffi::OsString,
}

impl WatchedNames {
    fn of(claude: &ClaudePaths) -> Result<WatchedNames, WatchError> {
        Ok(WatchedNames {
            credentials: file_name(&claude.credentials_file)?.to_os_string(),
            identity: file_name(&claude.identity_file)?.to_os_string(),
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
        path.file_name().is_some_and(|name| {
            name == self.credentials.as_os_str() || name == self.identity.as_os_str()
        })
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
    on_change: impl Fn() + Send + 'static,
) {
    run_debounced(
        events,
        stop,
        |event| names.is_change(event),
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
    is_change: impl Fn(&Event) -> bool,
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

    #[test]
    fn missing_config_folder_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("missing");
        let paths = ClaudePaths::for_config_dir(&missing, tmp.path());
        assert!(watch(&paths, || {}).is_err());
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
