//! The single-instance socket (docs/implementation.md "Single instance and
//! `seatswap panel`").
//!
//! The tray app holds `<data>/gui.lock` for its whole life and listens on
//! `<data>/gui.sock`. `seatswap panel` connects and sends one line. The
//! listener never touches Tauri: it only calls the handler with the view name.

#[cfg(feature = "gui")]
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(all(unix, feature = "gui"))]
use std::os::unix::fs::PermissionsExt;
#[cfg(all(unix, feature = "gui"))]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
#[cfg(feature = "gui")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "gui")]
use std::sync::Arc;
#[cfg(feature = "gui")]
use std::thread::{self, JoinHandle};
use std::time::Duration;

// The same Unix socket on Windows; `uds_windows` until std::os::windows::net is
// stable (rust-lang/rust#150487).
#[cfg(all(windows, feature = "gui"))]
use uds_windows::UnixListener;
#[cfg(windows)]
use uds_windows::UnixStream;

#[cfg(feature = "gui")]
use seatswap_core::locks::{self, FileLock, LockError};
use seatswap_core::paths::{self, AppPaths, PathsError};

/// How often the listener checks its stop flag while accepting.
#[cfg(feature = "gui")]
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// A silent client must not hold a connection, the listener or app exit forever.
const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub(crate) enum IpcError {
    #[error("seatswap is not running")]
    NotRunning,
    #[cfg(feature = "gui")]
    #[error("another seatswap is already running")]
    AlreadyRunning,
    #[error("gui socket {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[cfg(feature = "gui")]
    #[error("gui lock: {0}")]
    Lock(#[from] LockError),
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error("the tray app answered: {0}")]
    Reply(String),
    #[error("the running tray app is older than this seatswap; restart it")]
    OlderTray,
}

/// Tell the running tray app to open `view` (`main`, `add`, `settings`).
pub(crate) fn send_open_panel(app: &AppPaths, view: &str) -> Result<(), IpcError> {
    let socket = socket_for(app)?;
    send_open_panel_at(&socket, view)
}

/// Tell the running tray app that the status line has written a fresh feed.
/// No tray app running is not an error: the file waits for the next start.
pub(crate) fn send_feed(app: &AppPaths) -> Result<(), IpcError> {
    let socket = match socket_for(app) {
        Ok(socket) => socket,
        Err(IpcError::NotRunning) => return Ok(()),
        Err(error) => return Err(error),
    };
    send_feed_at(&socket)
}

/// The socket half of [`send_feed`], split out so tests can name the path.
fn send_feed_at(socket: &std::path::Path) -> Result<(), IpcError> {
    match send_at(socket, "feed\n") {
        Err(IpcError::NotRunning) => Ok(()),
        // The previous tray app knows no `feed` command. Saying it is old is
        // the only useful answer; every other reply keeps its own error.
        Err(IpcError::Reply(reply)) if reply == "error unknown command" => Err(IpcError::OlderTray),
        result => result,
    }
}

/// The socket of a running tray app, or the reason no tray app can hold one.
fn socket_for(app: &AppPaths) -> Result<PathBuf, IpcError> {
    match app.gui_socket(paths::runtime_dir().as_deref()) {
        Ok(socket) => Ok(socket),
        // No data folder: no tray app can hold its lock there.
        Err(PathsError::DataDir { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Err(IpcError::NotRunning)
        }
        Err(error) => Err(error.into()),
    }
}

/// The socket half of [`send_open_panel`], split out so tests can name the path.
pub(crate) fn send_open_panel_at(socket: &std::path::Path, view: &str) -> Result<(), IpcError> {
    send_at(socket, &format!("open-panel {view}\n"))
}

/// One command line to the tray app, then its one-line answer.
fn send_at(socket: &std::path::Path, line: &str) -> Result<(), IpcError> {
    // A failed connect is the documented "no tray app is running" case.
    let stream = UnixStream::connect(socket).map_err(|_| IpcError::NotRunning)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|source| IpcError::Io {
            path: socket.to_path_buf(),
            source,
        })?;
    let mut writer = stream;
    writer
        .write_all(line.as_bytes())
        .map_err(|source| IpcError::Io {
            path: socket.to_path_buf(),
            source,
        })?;
    let mut reader = BufReader::new(writer.try_clone().map_err(|source| IpcError::Io {
        path: socket.to_path_buf(),
        source,
    })?);
    let mut reply = String::new();
    reader
        .read_line(&mut reply)
        .map_err(|source| IpcError::Io {
            path: socket.to_path_buf(),
            source,
        })?;
    let reply = reply.trim();
    if reply == "ok" {
        Ok(())
    } else {
        Err(IpcError::Reply(reply.to_string()))
    }
}

/// One parsed line from a client: open a panel view, or take a fresh feed.
#[cfg(feature = "gui")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IpcCommand {
    OpenPanel(String),
    Feed,
}

/// A running listener. Dropping it stops the thread, removes the socket and
/// releases `gui.lock`.
#[cfg(feature = "gui")]
pub(crate) struct Listener {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _lock: FileLock,
}

#[cfg(feature = "gui")]
impl Listener {
    /// Take `gui.lock` and listen on `gui.sock`. A held lock means another
    /// instance is running and is reported as [`IpcError::AlreadyRunning`].
    pub(crate) fn start(
        app: &AppPaths,
        on_command: impl Fn(IpcCommand) + Send + Sync + 'static,
    ) -> Result<Listener, IpcError> {
        Self::start_in(app, paths::runtime_dir().as_deref(), on_command)
    }

    /// [`start`](Self::start) with the runtime folder given (`None`: the socket
    /// goes into the data folder), so tests never write into the machine's
    /// runtime folder.
    pub(crate) fn start_in(
        app: &AppPaths,
        runtime_dir: Option<&std::path::Path>,
        on_command: impl Fn(IpcCommand) + Send + Sync + 'static,
    ) -> Result<Listener, IpcError> {
        let on_command: Arc<dyn Fn(IpcCommand) + Send + Sync> = Arc::new(on_command);
        let lock = match locks::acquire_file_lock(&app.gui_lock(), Duration::ZERO) {
            Ok(lock) => lock,
            Err(LockError::Busy { .. }) => return Err(IpcError::AlreadyRunning),
            Err(error) => return Err(IpcError::Lock(error)),
        };
        // Only now: taking the lock creates the data folder on first start, and
        // the socket's name needs the folder's real path.
        let socket = app.gui_socket(runtime_dir)?;

        // A socket file left behind by a crash must not keep the app from starting.
        match fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(IpcError::Io {
                    path: socket,
                    source,
                })
            }
        }
        let listener = UnixListener::bind(&socket).map_err(|source| IpcError::Io {
            path: socket.clone(),
            source,
        })?;
        // Windows has no modes: the socket file has the data folder's permissions.
        #[cfg(unix)]
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).map_err(|source| {
            IpcError::Io {
                path: socket.clone(),
                source,
            }
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|source| IpcError::Io {
                path: socket.clone(),
                source,
            })?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let spawned = thread::Builder::new()
            .name("seatswap-ipc".to_string())
            .spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // A silent client gets its own short-lived thread, so it
                            // never delays the next connection or the app's exit.
                            let on_command = Arc::clone(&on_command);
                            if let Err(error) = thread::Builder::new()
                                .name("seatswap-ipc-connection".to_string())
                                .spawn(move || handle_connection(stream, on_command.as_ref()))
                            {
                                eprintln!("seatswap: ipc connection: {error}");
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(ACCEPT_POLL);
                        }
                        // A connection that went away between accept and read is not fatal.
                        Err(_) => thread::sleep(ACCEPT_POLL),
                    }
                }
            });
        let thread = match spawned {
            Ok(thread) => thread,
            Err(source) => {
                return Err(IpcError::Io {
                    path: socket,
                    source,
                })
            }
        };

        Ok(Listener {
            socket,
            stop,
            thread: Some(thread),
            _lock: lock,
        })
    }
}

#[cfg(feature = "gui")]
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}

#[cfg(feature = "gui")]
fn handle_connection(mut stream: UnixStream, on_command: &dyn Fn(IpcCommand)) {
    if stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .is_err()
    {
        return;
    }
    let Ok(read_side) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_side);
    let mut line = String::new();
    let reply = match reader.read_line(&mut line) {
        Ok(_) => match parse_command(line.trim()) {
            Some(command) => {
                on_command(command);
                "ok\n"
            }
            None => "error unknown command\n",
        },
        Err(_) => "error unknown command\n",
    };
    let _ = stream.write_all(reply.as_bytes());
}

/// `open-panel <view>`, with the three views the panel has, or `feed`.
/// Anything else is an unknown command and never panics.
#[cfg(feature = "gui")]
fn parse_command(line: &str) -> Option<IpcCommand> {
    if line == "feed" {
        return Some(IpcCommand::Feed);
    }
    let mut parts = line.split_whitespace();
    if parts.next() != Some("open-panel") {
        return None;
    }
    let view = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    match view {
        "main" | "add" | "settings" => Some(IpcCommand::OpenPanel(view.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "gui")]
    use std::sync::mpsc;
    #[cfg(feature = "gui")]
    use std::time::{Duration, Instant};

    #[cfg(feature = "gui")]
    fn app(tmp: &tempfile::TempDir) -> AppPaths {
        AppPaths::at(&tmp.path().join("data"))
    }

    #[cfg(feature = "gui")]
    fn ask(socket: &std::path::Path, line: &str) -> String {
        let mut stream = UnixStream::connect(socket).expect("connect to the listener");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        stream.write_all(line.as_bytes()).expect("send the command");
        let mut reply = String::new();
        BufReader::new(stream)
            .read_line(&mut reply)
            .expect("read the reply");
        reply
    }

    #[cfg(feature = "gui")]
    #[test]
    fn open_panel_calls_the_handler_and_replies_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = app(&tmp);
        let (opened, received) = mpsc::channel();
        let socket = app.gui_socket(None).expect("socket path");
        let listener = Listener::start_in(&app, None, move |command| {
            let _ = opened.send(command);
        })
        .expect("listener");

        assert_eq!(ask(&socket, "open-panel add\n"), "ok\n");
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)),
            Ok(IpcCommand::OpenPanel("add".to_string()))
        );
        drop(listener);
        assert!(!socket.exists(), "drop removes the socket");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn garbage_gets_an_error_and_no_handler_call() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = app(&tmp);
        let (opened, received) = mpsc::channel();
        let _listener = Listener::start_in(&app, None, move |command| {
            let _ = opened.send(command);
        })
        .expect("listener");

        assert_eq!(
            ask(
                &app.gui_socket(None).expect("socket path"),
                "please open the panel\n"
            ),
            "error unknown command\n"
        );
        assert_eq!(
            ask(
                &app.gui_socket(None).expect("socket path"),
                "open-panel nowhere\n"
            ),
            "error unknown command\n"
        );
        assert!(
            received.recv_timeout(Duration::from_millis(200)).is_err(),
            "an unknown command must not call the handler"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_silent_client_does_not_block_the_next_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = app(&tmp);
        let socket = app.gui_socket(None).expect("socket path");
        let _listener = Listener::start_in(&app, None, |_| {}).expect("listener");

        // Connect and send nothing: this used to hold the listener's only thread.
        let silent = UnixStream::connect(&socket).expect("silent client connects");
        std::thread::sleep(Duration::from_millis(100));

        let started = Instant::now();
        assert_eq!(ask(&socket, "open-panel main\n"), "ok\n");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the silent client delayed the listener by {:?}",
            started.elapsed()
        );
        drop(silent);
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_second_listener_on_a_held_lock_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = app(&tmp);
        let first = Listener::start_in(&app, None, |_| {}).expect("first listener");
        match Listener::start_in(&app, None, |_| {}) {
            Err(IpcError::AlreadyRunning) => {}
            Err(error) => panic!("expected AlreadyRunning, got {error}"),
            Ok(_) => panic!("a second listener must be refused"),
        }
        drop(first);
    }

    /// First start: the data folder does not exist until the lock creates it,
    /// and the socket in the runtime folder is named after its real path, so a
    /// relative spelling of the same folder reaches it.
    #[cfg(feature = "gui")]
    #[test]
    fn a_first_start_in_a_runtime_folder_is_found_by_any_spelling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runtime = tmp.path().join("run");
        fs::create_dir(&runtime).expect("runtime folder");
        let data = tmp.path().join("data");
        let _listener = Listener::start_in(&AppPaths::at(&data), Some(&runtime), |_| {})
            .expect("listener on a fresh data folder");

        let spelled = AppPaths::at(&tmp.path().join("run/../data/"));
        let socket = spelled.gui_socket(Some(&runtime)).expect("socket path");
        assert_eq!(ask(&socket, "open-panel main\n"), "ok\n");
    }

    /// A socket file left behind by a crash does not stop the next start, and
    /// `seatswap panel` reaches the new listener.
    #[cfg(feature = "gui")]
    #[test]
    fn a_leftover_socket_file_does_not_stop_the_start() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = app(&tmp);
        fs::create_dir(&app.data_dir).expect("data folder");
        let socket = app.gui_socket(None).expect("socket path");
        drop(UnixListener::bind(&socket).expect("a crashed app's socket"));
        assert!(
            socket.exists(),
            "dropping a listener leaves its socket file"
        );

        let (opened, received) = mpsc::channel();
        let _listener = Listener::start_in(&app, None, move |command| {
            let _ = opened.send(command);
        })
        .expect("listener despite the leftover socket");
        send_open_panel_at(&socket, "settings").expect("the new listener answers");
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)),
            Ok(IpcCommand::OpenPanel("settings".to_string()))
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn feed_is_a_known_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = app(&tmp);
        let socket = app.gui_socket(None).expect("socket path");
        let (opened, received) = mpsc::channel();
        let _listener = Listener::start_in(&app, None, move |command| {
            let _ = opened.send(command);
        })
        .expect("listener");

        assert_eq!(ask(&socket, "feed\n"), "ok\n");
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)),
            Ok(IpcCommand::Feed)
        );
        assert_eq!(parse_command("feed extra"), None);
    }

    #[test]
    fn send_feed_without_a_socket_is_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = seatswap_core::paths::AppPaths::at(&tmp.path().join("data"));
        match send_feed(&app) {
            Ok(()) => {}
            Err(error) => panic!("no tray app is running, so a feed is not an error: {error}"),
        }
    }

    /// A tray app of the previous version answers `feed` with
    /// `error unknown command`; the line must say what to do about it.
    #[cfg(unix)]
    #[test]
    fn a_tray_app_that_does_not_know_the_feed_is_reported_as_older() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("gui.sock");
        let listener =
            std::os::unix::net::UnixListener::bind(&socket).expect("the fake tray app's socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the feed connection");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("the read side"))
                .read_line(&mut line)
                .expect("the command line");
            assert_eq!(line, "feed\n");
            stream
                .write_all(b"error unknown command\n")
                .expect("the old app's reply");
        });

        match send_feed_at(&socket) {
            Err(IpcError::OlderTray) => {}
            Err(error) => panic!("expected the older-tray error, got {error}"),
            Ok(()) => panic!("an unknown command is an error, not success"),
        }
        server.join().expect("the fake tray app");
    }

    #[test]
    fn no_app_running_is_reported_as_not_running() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = seatswap_core::paths::AppPaths::at(&tmp.path().join("data"));
        match send_open_panel(&app, "main") {
            Err(IpcError::NotRunning) => {}
            Err(error) => panic!("expected NotRunning, got {error}"),
            Ok(()) => panic!("no app is listening"),
        }
    }
}
