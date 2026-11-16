//! DeepSeek Harness sessions: flash workers and their jobs (spec §3.8, task H-1
//! of the 1.2.0 plan).
//!
//! A Harness session is one folder below `<sessions>/<cwd-key>/<id>` with a
//! zstd-compressed JSON-lines file and an empty `session.lock` beside it. The
//! header names the session, its parent and its delegation depth; the events
//! give its turns, tokens, model and title. Only numbers and names leave this
//! module: a title is kept in memory for the live rows and is never written.
//!
//! A job started by a flash tool is linked to the Claude Code session whose
//! transcript names it. On Linux a running job that no transcript names yet is
//! found through the process that holds its `session.lock` open, walking up its
//! parents to a pid with a Claude Code session file. Elsewhere a running job
//! appears once its result is collected in a transcript.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Deserialize;

use crate::paths::PathsError;
use crate::transcripts::{ClosedJob, SessionSeen, Tokens};

/// The file that holds a Harness session's events.
const SESSION_FILE: &str = "session.v4.jsonl.zstd";
/// The lock a running session holds open.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SESSION_LOCK: &str = "session.lock";
/// The report of one flash job, below `<reports>/<key>/patches/`.
const REPORT_SUFFIX: &str = ".report.json";

/// An unfinished Harness session whose last event is this many minutes old is
/// only running when a process still holds its lock. A cancelled or killed job
/// leaves an unfinished file behind; it stops being shown as running once it
/// has been quiet for this long. C-1 uses the same rule for Codex sessions.
pub const HARNESS_QUIET_MINUTES: i64 = 30;

/// Where DeepSeek Harness and flash-agents keep their state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessPaths {
    /// `$DSH_HOME/sessions` or `~/.dsh/sessions`.
    pub sessions: PathBuf,
    /// `$XDG_STATE_HOME/flash-agents` or `~/.local/state/flash-agents`.
    pub reports: PathBuf,
}

impl HarnessPaths {
    /// Resolve from the real environment. Only the app calls this; tests build
    /// their own folders with [`HarnessPaths::at`].
    pub fn resolve() -> Result<HarnessPaths, PathsError> {
        let home = dirs::home_dir().ok_or(PathsError::NoHome)?;
        let dsh = env_folder("DSH_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".dsh"));
        let state = env_folder("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"));
        Ok(HarnessPaths {
            sessions: dsh.join("sessions"),
            reports: state.join("flash-agents"),
        })
    }

    /// The folders of a test, never read from the environment.
    pub fn at(sessions: PathBuf, reports: PathBuf) -> HarnessPaths {
        HarnessPaths { sessions, reports }
    }
}

/// A non-empty environment folder, if the variable is set.
fn env_folder(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// One Harness session, as its file gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSession {
    /// `flash-task-<uuid>` for a job, a UUID for a worker.
    pub id: String,
    /// The session's file below the Harness sessions folder, so the recorded
    /// totals can be kept for as long as the file exists.
    pub file: PathBuf,
    /// The session that started it; `None` for a top-level job.
    pub parent: Option<String>,
    /// The header's `delegationDepth`: 0 for a job, 1 or more for a worker.
    pub depth: u32,
    pub cwd: PathBuf,
    pub created: DateTime<Utc>,
    /// The newest event time; the creation time when it has no event yet.
    pub last: DateTime<Utc>,
    /// The newest `assistant/message` time; the creation time when it has no
    /// message yet.
    pub last_message: DateTime<Utc>,
    /// The model of the last `assistant/message`.
    pub model: Option<String>,
    /// The input, output and cache-read tokens of every `assistant/message`.
    pub tokens: Tokens,
    /// How many `assistant/message` events the file holds.
    pub messages: u32,
    /// Every `assistant/message`, oldest first: its time, model and usage. A
    /// message without `usage` is an answer with no tokens.
    pub messages_detail: Vec<HarnessMessage>,
    /// A `turn/end` after the last `turn/start`.
    pub finished: bool,
    /// The last `session/title`; kept in memory only.
    pub title: Option<String>,
}

/// One `assistant/message` of a Harness session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessMessage {
    /// The message's `time`, which names the hour its tokens belong to.
    pub at: DateTime<Utc>,
    /// The `data.message.source.model`; `None` when the event does not name one.
    pub model: Option<String>,
    /// The event's `data.usage`; `None` when the message has no usage.
    pub usage: Option<Tokens>,
}

/// What one read of the Harness store found, and what it could not read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HarnessRead {
    pub sessions: Vec<HarnessSession>,
    pub warnings: Vec<String>,
}

/// The Claude Code sessions the transcripts link a Harness id to, and the job
/// reports that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HarnessLinks {
    /// Harness session id → Claude Code session id.
    pub owners: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

/// Every Harness session whose file was written at or after `since`. A store
/// that does not exist is no session and no warning. A file still being written
/// ends mid-frame: the bytes decoded so far are used up to their last whole
/// line, and that is not a warning.
pub fn sessions(paths: &HarnessPaths, since: DateTime<Utc>) -> HarnessRead {
    let mut read = HarnessRead::default();
    let keys = match fs::read_dir(&paths.sessions) {
        Ok(keys) => keys,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return read,
        Err(source) => {
            read.warnings
                .push(format!("{}: {source}", paths.sessions.display()));
            return read;
        }
    };
    for key in keys {
        let key = match key {
            Ok(key) => key,
            Err(source) => {
                read.warnings
                    .push(format!("{}: {source}", paths.sessions.display()));
                continue;
            }
        };
        if !key.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let entries = match fs::read_dir(key.path()) {
            Ok(entries) => entries,
            Err(source) => {
                read.warnings
                    .push(format!("{}: {source}", key.path().display()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    read.warnings
                        .push(format!("{}: {source}", key.path().display()));
                    continue;
                }
            };
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let file = entry.path().join(SESSION_FILE);
            let metadata = match fs::metadata(&file) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    read.warnings.push(format!("{}: {source}", file.display()));
                    continue;
                }
            };
            match metadata.modified() {
                Ok(modified) => {
                    let modified: DateTime<Utc> = modified.into();
                    // The file was last written before the range: no event in
                    // it can belong to the range, so it is not decompressed.
                    if modified < since {
                        continue;
                    }
                }
                Err(source) => {
                    read.warnings.push(format!(
                        "{}: the modification time cannot be read: {source}",
                        file.display()
                    ));
                }
            }
            match read_one(&file) {
                Ok(Some(mut session)) => {
                    // Keep the file relative to the store, so a saved total
                    // finds it again also when the store moves.
                    if let Ok(relative) = file.strip_prefix(&paths.sessions) {
                        session.file = relative.to_path_buf();
                    }
                    read.sessions.push(session);
                }
                Ok(None) => {}
                Err(warning) => read.warnings.push(warning),
            }
        }
    }
    read.sessions
        .sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    read
}

/// One session file: `Ok(None)` when it has no whole line yet, `Err` with a
/// warning naming the file when its first line is not a header or a whole line
/// is not JSON.
fn read_one(file: &Path) -> Result<Option<HarnessSession>, String> {
    let bytes = read_decoded(file).map_err(|source| format!("{}: {source}", file.display()))?;
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return Ok(None);
    };
    let text = match std::str::from_utf8(&bytes[..end]) {
        Ok(text) => text,
        Err(_) => {
            return Err(format!(
                "{}: a whole line is not valid UTF-8",
                file.display()
            ))
        }
    };
    let mut lines = text.split('\n');
    let Some(header) = lines.next() else {
        return Ok(None);
    };
    let header: Header = match serde_json::from_str::<Header>(header) {
        Ok(header) if header.kind == "session" => header,
        Ok(_) => {
            return Err(format!(
                "{}: the first line is not a session header",
                file.display()
            ))
        }
        Err(_) => {
            return Err(format!(
                "{}: the first line is not a session header",
                file.display()
            ))
        }
    };
    let Some(created) = Utc.timestamp_millis_opt(header.created_at).single() else {
        return Err(format!(
            "{}: createdAt {} is not a valid time",
            file.display(),
            header.created_at
        ));
    };
    let mut session = HarnessSession {
        id: header.id,
        file: file.to_path_buf(),
        parent: header.parent_session,
        depth: header.delegation_depth,
        cwd: PathBuf::from(header.cwd),
        created,
        last: created,
        last_message: created,
        model: None,
        tokens: Tokens::default(),
        messages: 0,
        messages_detail: Vec::new(),
        finished: false,
        title: None,
    };
    for (number, line) in lines.enumerate() {
        let event: EventLine = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(_) => {
                return Err(format!(
                    "{}: line {} is not JSON",
                    file.display(),
                    number + 2
                ))
            }
        };
        let Some(at) = event
            .time
            .and_then(|time| Utc.timestamp_millis_opt(time).single())
        else {
            return Err(format!(
                "{}: line {} has no valid time",
                file.display(),
                number + 2
            ));
        };
        if at > session.last {
            session.last = at;
        }
        match event.kind.as_str() {
            "turn/start" => session.finished = false,
            "turn/end" => session.finished = true,
            "assistant/message" => {
                session.messages = session.messages.saturating_add(1);
                if at > session.last_message {
                    session.last_message = at;
                }
                let data = event.data.unwrap_or_default();
                let model = data
                    .message
                    .as_ref()
                    .and_then(|message| message.source.as_ref())
                    .and_then(|source| source.model.clone());
                if let Some(model) = &model {
                    session.model = Some(model.clone());
                }
                let usage = match data.usage {
                    Some(usage) => {
                        let (Some(input), Some(output)) = (usage.input_tokens, usage.output_tokens)
                        else {
                            return Err(format!(
                                "{}: line {} has a usage without inputTokens or outputTokens",
                                file.display(),
                                number + 2
                            ));
                        };
                        Some(Tokens {
                            input,
                            out: output,
                            cw: 0,
                            cr: usage.cache_read_tokens.unwrap_or(0),
                        })
                    }
                    None => None,
                };
                if let Some(tokens) = usage {
                    session.tokens += tokens;
                }
                session
                    .messages_detail
                    .push(HarnessMessage { at, model, usage });
            }
            "session/title" => {
                if let Some(title) = event.data.and_then(|data| data.title) {
                    session.title = Some(title);
                }
            }
            // Every other event type is skipped unread except for its time.
            _ => {}
        }
    }
    Ok(Some(session))
}

/// Every byte the decoder gives before its first error, or before the end of
/// the file. A stream still being written ends mid-frame: that is where it
/// stops, never an error of the read.
fn read_decoded(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|source| source.to_string())?;
    let mut decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|source| format!("it is not a zstd stream: {source}"))?;
    let mut out = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match decoder.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => out.extend_from_slice(&buffer[..read]),
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            // A torn frame after some good bytes: use what the decoder gave
            // before it. A stream whose first frame gives nothing at all is
            // corrupt, not a file that is still being written.
            Err(source) if out.is_empty() => {
                return Err(format!("the zstd stream cannot be decoded: {source}"))
            }
            Err(_) => break,
        }
    }
    Ok(out)
}

/// The header line of a session file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    cwd: String,
    created_at: i64,
    parent_session: Option<String>,
    delegation_depth: u32,
}

/// One event line, as far as this module looks at it.
#[derive(Deserialize)]
struct EventLine {
    #[serde(rename = "type")]
    kind: String,
    time: Option<i64>,
    data: Option<EventData>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct EventData {
    message: Option<Message>,
    usage: Option<Usage>,
    title: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    source: Option<MessageSource>,
}

#[derive(Deserialize)]
struct MessageSource {
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
}

/// The `sessionId` of one job report.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    session_id: Option<String>,
}

/// The Claude Code sessions whose transcripts named a Harness job, and the jobs
/// of Claude Code sessions that have left the scan state. Every
/// `flash-task-<uuid>` is taken as it is; every `job-<8 hex>` id is turned into
/// its `flash-task-…` id through `<reports>/<key>/patches/job-…report.json`.
pub fn links(
    paths: &HarnessPaths,
    seen: &BTreeMap<String, SessionSeen>,
    closed: &BTreeMap<String, ClosedJob>,
) -> HarnessLinks {
    let mut links = HarnessLinks::default();
    // A resumed session carries the old transcript's tool results, so the same
    // job can be named by two sessions: the later session `last` owns it.
    let mut owner_at: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    for (session, seen) in seen {
        for id in &seen.flash_jobs {
            name_job(paths, id, session, seen.last, &mut links, &mut owner_at);
        }
    }
    for (id, link) in closed {
        name_job(
            paths,
            id,
            &link.session,
            link.last,
            &mut links,
            &mut owner_at,
        );
    }
    links
}

/// Put one job id under the Claude Code session that named it, unless an owner
/// with a later `last` is already known.
fn name_job(
    paths: &HarnessPaths,
    id: &str,
    session: &str,
    last: DateTime<Utc>,
    links: &mut HarnessLinks,
    owner_at: &mut BTreeMap<String, DateTime<Utc>>,
) {
    let job = if id.starts_with("flash-task-") {
        Some(id.to_string())
    } else if id.starts_with("job-") {
        report_session(paths, id, &mut links.warnings)
    } else {
        None
    };
    let Some(job) = job else {
        return;
    };
    match owner_at.get(&job) {
        Some(previous) if *previous >= last => {}
        _ => {
            owner_at.insert(job.clone(), last);
            links.owners.insert(job, session.to_string());
        }
    }
}

/// The `sessionId` of `job-<8 hex>` in any `<reports>/<key>/patches/` folder.
fn report_session(paths: &HarnessPaths, job: &str, warnings: &mut Vec<String>) -> Option<String> {
    let keys = match fs::read_dir(&paths.reports) {
        Ok(keys) => keys,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return None,
        Err(source) => {
            warnings.push(format!("{}: {source}", paths.reports.display()));
            return None;
        }
    };
    for key in keys {
        let key = match key {
            Ok(key) => key,
            Err(source) => {
                warnings.push(format!("{}: {source}", paths.reports.display()));
                continue;
            }
        };
        if !key.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let path = key
            .path()
            .join("patches")
            .join(format!("{job}{REPORT_SUFFIX}"));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                warnings.push(format!("{}: {source}", path.display()));
                continue;
            }
        };
        let report: Report = match serde_json::from_slice(&bytes) {
            Ok(report) => report,
            Err(error) => {
                warnings.push(format!(
                    "{}: not valid JSON at line {} column {}",
                    path.display(),
                    error.line(),
                    error.column()
                ));
                return None;
            }
        };
        match report.session_id {
            Some(session) => return Some(session),
            None => {
                warnings.push(format!("{}: no sessionId", path.display()));
                return None;
            }
        }
    }
    None
}

/// The processes a [`running_owners`] walk can read. The app uses [`RealProc`];
/// a test builds its own table.
pub trait ProcSource {
    /// Every pid the system lists.
    fn pids(&self) -> Vec<u32>;
    /// The files `pid` holds open.
    fn open_files(&self, pid: u32) -> Vec<PathBuf>;
    /// The pid's parent, when the system says.
    fn parent(&self, pid: u32) -> Option<u32>;
}

/// The real `/proc` on Linux; elsewhere it has no processes to walk, and
/// [`running_owners`] returns no link (the documented rule: on Windows a
/// running job appears once its result is collected in a transcript).
#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone, Copy)]
pub struct RealProc;

#[cfg(target_os = "linux")]
impl ProcSource for RealProc {
    fn pids(&self) -> Vec<u32> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut pids = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Ok(pid) = name.parse() {
                pids.push(pid);
            }
        }
        pids
    }

    fn open_files(&self, pid: u32) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            return Vec::new();
        };
        let mut files = Vec::new();
        for entry in entries.flatten() {
            if let Ok(path) = fs::read_link(entry.path()) {
                files.push(path);
            }
        }
        files
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        crate::sessions::proc_parent(pid)
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct RealProc;

#[cfg(not(target_os = "linux"))]
impl ProcSource for RealProc {
    fn pids(&self) -> Vec<u32> {
        Vec::new()
    }

    fn open_files(&self, _pid: u32) -> Vec<PathBuf> {
        Vec::new()
    }

    fn parent(&self, _pid: u32) -> Option<u32> {
        None
    }
}

/// What one walk of the process table found: the locks held and the Claude Code
/// sessions their holders hang under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcLinks {
    /// Harness session id → Claude Code session id.
    pub owners: BTreeMap<String, String>,
    /// The Harness sessions whose lock a process holds.
    pub locks: BTreeSet<String>,
}

/// Walk the process table once: every Harness lock held, and the Claude Code
/// session of a lock holder's parent when there is one. The sessions folder is
/// resolved once, because the kernel reports an open file by its resolved path;
/// a folder that cannot be resolved (it does not exist) has no lock to find.
#[cfg(target_os = "linux")]
pub fn proc_links(
    paths: &HarnessPaths,
    procs: &dyn ProcSource,
    claude_pids: &BTreeMap<u32, String>,
) -> ProcLinks {
    let mut links = ProcLinks::default();
    let Ok(sessions) = fs::canonicalize(&paths.sessions) else {
        return links;
    };
    for pid in procs.pids() {
        let ids: BTreeSet<String> = procs
            .open_files(pid)
            .iter()
            .filter_map(|file| harness_id_of(&sessions, file))
            .collect();
        for id in ids {
            links.locks.insert(id.clone());
            if let Some(session) = walk_to_claude(procs, claude_pids, pid) {
                links.owners.entry(id).or_insert(session);
            }
        }
    }
    links
}

#[cfg(not(target_os = "linux"))]
pub fn proc_links(
    _paths: &HarnessPaths,
    _procs: &dyn ProcSource,
    _claude_pids: &BTreeMap<u32, String>,
) -> ProcLinks {
    ProcLinks::default()
}

/// The Harness sessions whose `session.lock` a process holds open, linked to
/// the Claude Code session of a parent process (`pid → session id`). On a
/// system without `/proc` the map is empty.
pub fn running_owners(
    paths: &HarnessPaths,
    procs: &dyn ProcSource,
    claude_pids: &BTreeMap<u32, String>,
) -> BTreeMap<String, String> {
    proc_links(paths, procs, claude_pids).owners
}

/// Whether a Harness session is working now: it has no `turn/end` after its
/// last `turn/start`, and either a process holds its lock or its last event is
/// less than [`HARNESS_QUIET_MINUTES`] old. An unfinished file that has been
/// quiet for longer and has no lock holder is a cancelled or killed job, shown
/// as done.
pub fn is_running(session: &HarnessSession, lock_held: bool, now: DateTime<Utc>) -> bool {
    !session.finished
        && (lock_held
            || now.signed_duration_since(session.last) < Duration::minutes(HARNESS_QUIET_MINUTES))
}

/// The Harness session id of `<sessions>/<key>/<id>/session.lock`, if this is
/// that file. `sessions` is the canonical sessions folder.
#[cfg(target_os = "linux")]
fn harness_id_of(sessions: &Path, file: &Path) -> Option<String> {
    if file.file_name()? != SESSION_LOCK {
        return None;
    }
    let id = file.parent()?.file_name()?.to_str()?.to_string();
    let key = file.parent()?.parent()?;
    if key.parent()? != sessions {
        return None;
    }
    Some(id)
}

/// The Claude Code session of the first pid in `pid`'s parent chain that has a
/// session file. A cycle or a chain without one gives `None`.
#[cfg(target_os = "linux")]
fn walk_to_claude(
    procs: &dyn ProcSource,
    claude_pids: &BTreeMap<u32, String>,
    pid: u32,
) -> Option<String> {
    let mut seen = BTreeSet::new();
    let mut current = pid;
    while seen.insert(current) {
        if let Some(session) = claude_pids.get(&current) {
            return Some(session.clone());
        }
        current = procs.parent(current)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeMap, BTreeSet};

    use tempfile::TempDir;

    const JOB: &str = "flash-task-5c974a53-178a-4254-93b2-b804a111414a";
    const WORKER: &str = "a25abffd-ed93-4296-868f-dbd77aca32ac";
    const SESSION: &str = "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34";
    const KEY: &str = "--home-sam-fm-23490eb0ea8f-slots-10--";
    const CWD: &str = "/home/sam/fm/23490eb0ea8f/slots/10";
    const MODEL: &str = "deepseek-v4.1-flash:cloud";

    fn store() -> (TempDir, HarnessPaths) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = HarnessPaths::at(tmp.path().join("sessions"), tmp.path().join("flash-agents"));
        (tmp, paths)
    }

    fn header(id: &str, parent: Option<&str>, depth: u32, created: i64) -> String {
        let mut value = serde_json::json!({
            "type": "session",
            "version": 4,
            "id": id,
            "createdAt": created,
            "cwd": CWD,
            "isSeeded": false,
            "delegationDepth": depth,
        });
        if let Some(parent) = parent {
            value["parentSession"] = serde_json::json!(parent);
            value["origin"] = serde_json::json!("subagent");
        }
        value.to_string()
    }

    fn event(kind: &str, seq: u32, time: i64, data: serde_json::Value) -> String {
        serde_json::json!({"type": kind, "seq": seq, "time": time, "data": data}).to_string()
    }

    fn assistant(time: i64, model: &str, input: u64, output: u64) -> String {
        event(
            "assistant/message",
            9,
            time,
            serde_json::json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "role": "assistant",
                    "source": {"kind": "model", "provider": "ollama", "model": model},
                    "id": "m1"
                },
                "usage": {
                    "inputTokens": input,
                    "outputTokens": output,
                    "totalTokens": input + output
                }
            }),
        )
    }

    fn job_lines() -> Vec<String> {
        vec![
            header(JOB, None, 0, 1794696054070),
            event(
                "turn/start",
                3,
                1794696054100,
                serde_json::json!({"turn": 1}),
            ),
            assistant(1794696060000, MODEL, 10516, 3585),
            event(
                "session/title",
                4,
                1794696054200,
                serde_json::json!({"title": "atlas link test"}),
            ),
            event(
                "turn/end",
                20,
                1794696062000,
                serde_json::json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ]
    }

    fn worker_lines() -> Vec<String> {
        vec![
            header(WORKER, Some(JOB), 1, 1794696066829),
            event(
                "turn/start",
                3,
                1794696066900,
                serde_json::json!({"turn": 1}),
            ),
            assistant(1794696070000, MODEL, 2200, 1100),
        ]
    }

    fn write_lines(paths: &HarnessPaths, id: &str, lines: &[String]) -> PathBuf {
        let dir = paths.sessions.join(KEY).join(id);
        fs::create_dir_all(&dir).expect("a session folder");
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        let bytes = zstd::encode_all(text.as_bytes(), 3).expect("zstd");
        let file = dir.join(SESSION_FILE);
        fs::write(&file, bytes).expect("a session file");
        fs::write(dir.join(SESSION_LOCK), []).expect("a lock");
        file
    }

    fn write_report(paths: &HarnessPaths, job: &str, session: &str) {
        let dir = paths.reports.join("acme").join("patches");
        fs::create_dir_all(&dir).expect("a report folder");
        let report = serde_json::json!({
            "createdAt": "2026-11-14T09:40:54.070Z",
            "jobId": job,
            "key": "acme",
            "kind": "task",
            "label": "atlas link test",
            "report": {},
            "sessionId": session,
            "status": "done",
            "stopReason": null,
            "timing": {},
        });
        fs::write(dir.join(format!("{job}.report.json")), report.to_string()).expect("a report");
    }

    fn since() -> DateTime<Utc> {
        "2026-11-01T00:00:00Z".parse().expect("a time")
    }

    #[test]
    fn a_job_and_its_workers_are_read() {
        let (_tmp, paths) = store();
        write_lines(&paths, JOB, &job_lines());
        write_lines(&paths, WORKER, &worker_lines());

        let read = sessions(&paths, since());

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(read.sessions.len(), 2);
        let job = read.sessions.iter().find(|s| s.id == JOB).expect("the job");
        assert_eq!(job.parent, None);
        assert_eq!(job.depth, 0);
        assert_eq!(job.cwd, PathBuf::from(CWD));
        assert_eq!(
            job.created,
            Utc.timestamp_millis_opt(1794696054070).single().unwrap()
        );
        assert_eq!(
            job.last,
            Utc.timestamp_millis_opt(1794696062000).single().unwrap()
        );
        assert_eq!(job.model.as_deref(), Some(MODEL));
        assert_eq!(job.tokens.input, 10516);
        assert_eq!(job.tokens.out, 3585);
        assert!(job.finished);
        assert_eq!(job.title.as_deref(), Some("atlas link test"));

        let worker = read
            .sessions
            .iter()
            .find(|s| s.id == WORKER)
            .expect("the worker");
        assert_eq!(worker.parent.as_deref(), Some(JOB));
        assert_eq!(worker.depth, 1);
        assert_eq!(worker.tokens.input, 2200);
        assert_eq!(worker.tokens.out, 1100);
        assert!(!worker.finished);
    }

    #[test]
    fn a_growing_session_file_reads_up_to_its_last_whole_line() {
        let (_tmp, paths) = store();
        let dir = paths.sessions.join(KEY).join(JOB);
        fs::create_dir_all(&dir).expect("a session folder");
        // Three whole frames, then a fourth cut mid-frame, as a writer that is
        // still appending leaves the file.
        let mut bytes = Vec::new();
        let lines = job_lines();
        for line in &lines[..3] {
            bytes.extend(zstd::encode_all(format!("{line}\n").as_bytes(), 3).expect("zstd"));
        }
        let last = zstd::encode_all(format!("{}\n", lines[4]).as_bytes(), 3).expect("zstd");
        bytes.extend(&last[..last.len() / 3]);
        let file = dir.join(SESSION_FILE);
        fs::write(&file, bytes).expect("a session file");

        let read = sessions(&paths, since());

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(read.sessions.len(), 1);
        let job = &read.sessions[0];
        assert_eq!(job.tokens.input, 10516);
        assert_eq!(job.tokens.out, 3585);
        assert!(!job.finished, "the cut turn/end is not read");
    }

    #[test]
    fn a_store_that_does_not_exist_is_no_rows_and_no_warning() {
        let (_tmp, paths) = store();

        let read = sessions(&paths, since());

        assert!(read.sessions.is_empty());
        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
    }

    struct FakeProc {
        processes: BTreeMap<u32, (Vec<PathBuf>, Option<u32>)>,
    }

    impl ProcSource for FakeProc {
        fn pids(&self) -> Vec<u32> {
            self.processes.keys().copied().collect()
        }

        fn open_files(&self, pid: u32) -> Vec<PathBuf> {
            self.processes
                .get(&pid)
                .map(|(files, _)| files.clone())
                .unwrap_or_default()
        }

        fn parent(&self, pid: u32) -> Option<u32> {
            self.processes.get(&pid).and_then(|(_, parent)| *parent)
        }
    }

    #[test]
    fn a_running_job_is_linked_through_its_lock_holder() {
        let (_tmp, paths) = store();
        let lock = paths.sessions.join(KEY).join(JOB).join(SESSION_LOCK);
        fs::create_dir_all(lock.parent().expect("a session folder")).expect("a session folder");
        let procs = FakeProc {
            processes: BTreeMap::from([
                (500, (vec![lock], Some(450))),
                (450, (Vec::new(), Some(400))),
                (400, (Vec::new(), Some(1))),
                (1, (Vec::new(), None)),
            ]),
        };
        let claude_pids = BTreeMap::from([(400, SESSION.to_string())]);

        let owners = running_owners(&paths, &procs, &claude_pids);

        assert_eq!(owners.get(JOB).map(String::as_str), Some(SESSION));
    }

    #[test]
    fn a_lock_holder_without_a_claude_parent_is_unlinked() {
        let (_tmp, paths) = store();
        let lock = paths.sessions.join(KEY).join(JOB).join(SESSION_LOCK);
        fs::create_dir_all(lock.parent().expect("a session folder")).expect("a session folder");
        let procs = FakeProc {
            processes: BTreeMap::from([
                (500, (vec![lock], Some(450))),
                (450, (Vec::new(), Some(1))),
                (1, (Vec::new(), None)),
            ]),
        };
        let claude_pids = BTreeMap::from([(400, SESSION.to_string())]);

        let owners = running_owners(&paths, &procs, &claude_pids);

        assert!(owners.is_empty(), "{owners:?}");
    }

    #[test]
    fn a_job_report_gives_its_flash_task_id() {
        let (_tmp, paths) = store();
        write_report(&paths, "job-3e32c2fc", JOB);
        let mut seen = BTreeMap::new();
        let mut session = test_seen();
        session.flash_jobs = BTreeSet::from(["job-3e32c2fc".to_string()]);
        seen.insert(SESSION.to_string(), session);

        let links = links(&paths, &seen, &BTreeMap::new());

        assert!(links.warnings.is_empty(), "{:?}", links.warnings);
        assert_eq!(links.owners.get(JOB).map(String::as_str), Some(SESSION));
    }

    fn test_seen() -> SessionSeen {
        let time: DateTime<Utc> = "2026-11-14T09:40:00Z".parse().expect("a time");
        SessionSeen {
            folder: "home-sam-projects-atlas".to_string(),
            cwd: CWD.to_string(),
            first: time,
            last: time,
            last_model: None,
            last_user_turn: None,
            thinking: Vec::new(),
            totals: Tokens::default(),
            by_model: BTreeMap::new(),
            agents: BTreeMap::new(),
            spawns: BTreeMap::new(),
            recent: Vec::new(),
            recorded: None,
            flash_jobs: BTreeSet::new(),
        }
    }

    #[test]
    fn proof_a_corrupt_session_file_is_not_silently_absent() {
        let (_tmp, paths) = store();
        let dir = paths.sessions.join(KEY).join(JOB);
        fs::create_dir_all(&dir).expect("a session folder");
        let file = dir.join(SESSION_FILE);
        fs::write(&file, vec![b'x'; 4096]).expect("a corrupt file");

        let read = sessions(&paths, since());

        assert!(read.sessions.is_empty());
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        assert!(
            read.warnings[0].contains(file.to_str().expect("a text path")),
            "the warning must name the file: {}",
            read.warnings[0]
        );
    }

    #[test]
    fn a_first_frame_that_cannot_be_decoded_is_named() {
        let (_tmp, paths) = store();
        let dir = paths.sessions.join(KEY).join(JOB);
        fs::create_dir_all(&dir).expect("a session folder");
        // The zstd magic, then bytes that are not a frame.
        let mut bytes = vec![0x28, 0xb5, 0x2f, 0xfd];
        bytes.extend([0u8; 128]);
        let file = dir.join(SESSION_FILE);
        fs::write(&file, bytes).expect("a corrupt file");

        let read = sessions(&paths, since());

        assert!(read.sessions.is_empty());
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        assert!(
            read.warnings[0].contains(file.to_str().expect("a text path")),
            "the warning must name the file: {}",
            read.warnings[0]
        );
    }

    #[test]
    fn proof_a_job_named_by_two_sessions_goes_to_the_newer_one() {
        let (_tmp, paths) = store();
        let mut older = test_seen();
        older.flash_jobs = BTreeSet::from([JOB.to_string()]);
        older.last = "2026-11-14T09:00:00Z".parse().expect("a time");
        let mut newer = test_seen();
        newer.flash_jobs = BTreeSet::from([JOB.to_string()]);
        newer.last = "2026-11-14T11:00:00Z".parse().expect("a time");
        // The older session sorts first by id, as a resumed session's copy of
        // the transcript can.
        let seen = BTreeMap::from([
            ("00000000-0000-4000-8000-000000000001".to_string(), older),
            ("ffffffff-0000-4000-8000-000000000002".to_string(), newer),
        ]);

        let links = links(&paths, &seen, &BTreeMap::new());

        assert_eq!(
            links.owners.get(JOB).map(String::as_str),
            Some("ffffffff-0000-4000-8000-000000000002")
        );
    }

    #[test]
    fn a_closed_sessions_job_link_is_read_and_a_newer_session_wins() {
        let (_tmp, paths) = store();
        let closed = BTreeMap::from([(
            JOB.to_string(),
            ClosedJob {
                session: SESSION.to_string(),
                last: "2026-11-14T09:00:00Z".parse().expect("a time"),
            },
        )]);

        let found = links(&paths, &BTreeMap::new(), &closed);
        assert_eq!(found.owners.get(JOB).map(String::as_str), Some(SESSION));

        let mut newer = test_seen();
        newer.flash_jobs = BTreeSet::from([JOB.to_string()]);
        newer.last = "2026-11-14T11:00:00Z".parse().expect("a time");
        let seen = BTreeMap::from([("ffffffff-0000-4000-8000-000000000002".to_string(), newer)]);
        let found = links(&paths, &seen, &closed);
        assert_eq!(
            found.owners.get(JOB).map(String::as_str),
            Some("ffffffff-0000-4000-8000-000000000002"),
            "a remembered session with a later last owns the job"
        );
    }

    /// The kernel reports an open file by its resolved path: a sessions folder
    /// reached through a symlink must still find its lock holder.
    #[test]
    #[cfg(target_os = "linux")]
    fn proof_a_lock_under_a_symlinked_sessions_folder_is_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join(KEY).join(JOB)).expect("folders");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a symlink");
        let paths = HarnessPaths::at(link, tmp.path().join("flash-agents"));
        let resolved = std::fs::canonicalize(&real).expect("resolved");
        let lock = resolved.join(KEY).join(JOB).join(SESSION_LOCK);
        let procs = FakeProc {
            processes: BTreeMap::from([(500, (vec![lock], Some(400))), (400, (Vec::new(), None))]),
        };
        let claude_pids = BTreeMap::from([(400, SESSION.to_string())]);

        let owners = running_owners(&paths, &procs, &claude_pids);

        assert_eq!(owners.get(JOB).map(String::as_str), Some(SESSION));
    }
}
