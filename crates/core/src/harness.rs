//! DeepSeek Harness sessions: flash workers and their jobs (spec §3.8, task H-1
//! of the 1.2.0 plan).
//!
//! A Harness session is one folder below `<sessions>/<cwd-key>/<id>` with a
//! zstd-compressed JSON-lines file and an empty `session.lock` beside it. The
//! header names the session, its parent and its delegation depth; the events
//! give its turns, tokens, model and title. Only numbers, names, a title and a
//! job's label leave this module. A Harness session that is its own row is
//! named by its title, which the live report keeps. A flash job's task text is
//! the `label` of its flash-agents report ([`JobReports`]); a flash job's own
//! title is flash-agents' dispatcher prompt and a flash worker's names its
//! folder, so neither is shown. A label and a worker's title are task texts,
//! shown live and never written. No title or label reaches a history record.
//!
//! A job started by a flash tool is linked to the Claude Code session whose
//! transcript names it. On Linux a running job that no transcript names yet is
//! found through the process that holds its `session.lock` open, walking up its
//! parents to a pid with a Claude Code session file. Elsewhere a running job
//! appears once its result is collected in a transcript.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::paths::{HarnessEnv, PathsError};
use crate::transcripts::{ClosedJob, SessionSeen, Tokens};

/// The folder a flash worker's copy was made from, for a session whose
/// Claude Code session is not known.
///
/// flash-agents keeps a repository's worker copies in one of three layouts:
/// `<base>/<key>/slots/<n>`, `<tmp>/flash-mcp/<key>/<pid>/<instance>/slots/<slot>`
/// and the older `<key>/<pid>/slots/<slot>`, where `<key>` is the first 12
/// hex digits of the SHA-256 of the folder its server was started in, `<pid>`
/// is digits, `<instance>` hex, and a slot is `<digits>`, `slot-<digits>` or
/// `slot-<digits>-<digits>` (seen on Windows:
/// `%TEMP%\flash-mcp\67cebb192d26\8308\8aef170c\slots\slot-0`). The key is
/// the nearest 12-hex component before `slots` that only a pid and an
/// instance stand between. `roots` are the folders that may have been copied
/// (the known Claude Code sessions' folders); the one whose key matches is
/// the source, in its recorded spelling, with the copy's own subfolder (all
/// after the slot) kept. A Windows root is also matched with its drive letter
/// upper- and lower-cased, as its server may have been started with either.
/// `None` when `cwd` lies in no such copy or no root matches: the caller then
/// keeps the copy's folder, never a guessed one.
pub fn copy_source(cwd: &Path, roots: &[&str]) -> Option<String> {
    let parts: Vec<&str> = cwd
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let (key, rest) = (1..parts.len()).find_map(|at| {
        if parts[at] != "slots" || !parts.get(at + 1).is_some_and(|slot| is_slot(slot)) {
            return None;
        }
        copy_key_before(&parts[..at]).map(|key| (key, &parts[at + 2..]))
    })?;
    let root = roots.iter().find(|root| {
        drive_spellings(root)
            .iter()
            .any(|spelling| copy_key(spelling) == key)
    })?;
    let mut source = PathBuf::from(root);
    for part in rest {
        source.push(part);
    }
    source.into_os_string().into_string().ok()
}

/// The key of a copy whose `slots` folder follows `before`: the nearest
/// 12-hex component with at most a pid and an instance after it.
fn copy_key_before<'p>(before: &[&'p str]) -> Option<&'p str> {
    let key = |part: &str| part.len() == 12 && is_hex(part);
    match before {
        [.., last] if key(last) => Some(last),
        [.., found, pid] if key(found) && is_digits(pid) => Some(found),
        [.., found, pid, instance] if key(found) && is_digits(pid) && is_hex(instance) => {
            Some(found)
        }
        _ => None,
    }
}

/// A slot folder's name: `<digits>`, `slot-<digits>` or `slot-<digits>-<digits>`.
fn is_slot(name: &str) -> bool {
    match name.strip_prefix("slot-") {
        None => is_digits(name),
        Some(numbers) => match numbers.split_once('-') {
            None => is_digits(numbers),
            Some((first, second)) => is_digits(first) && is_digits(second),
        },
    }
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_hex(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `root` as recorded and, for a Windows root (`<letter>:…`), with only its
/// drive letter upper- and lower-cased: the spellings its server may have
/// hashed.
fn drive_spellings(root: &str) -> Vec<String> {
    let mut spellings = vec![root.to_string()];
    let bytes = root.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let rest = &root[1..];
        for letter in [bytes[0].to_ascii_uppercase(), bytes[0].to_ascii_lowercase()] {
            let spelling = format!("{}{rest}", char::from(letter));
            if !spellings.contains(&spelling) {
                spellings.push(spelling);
            }
        }
    }
    spellings
}

/// The key flash-agents names a folder's worker copies by: the first 12 hex
/// digits of the SHA-256 of the folder's path.
fn copy_key(root: &str) -> String {
    Sha256::digest(root.as_bytes())
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether a Harness session id is a flash job's (`flash-task-<uuid>`); a
/// worker's id is a plain UUID.
pub fn is_flash_job(id: &str) -> bool {
    id.starts_with("flash-task-")
}

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
    /// flash-agents' state folder, placed by its own rule
    /// ([`crate::paths::harness_folders`]): `$FLASH_DATA_DIR`, else
    /// `%LOCALAPPDATA%\flash-agents` on Windows, `~/Library/Application
    /// Support/flash-agents` on macOS, `$XDG_STATE_HOME/flash-agents` or
    /// `~/.local/state/flash-agents` elsewhere.
    pub reports: PathBuf,
}

impl HarnessPaths {
    /// Resolve from the real environment. Only the app calls this; tests build
    /// their own folders with [`HarnessPaths::at`] or pass the variables to
    /// [`HarnessPaths::resolve_with`].
    pub fn resolve() -> Result<HarnessPaths, PathsError> {
        let home = dirs::home_dir().ok_or(PathsError::NoHome)?;
        let env = HarnessEnv {
            dsh_home: std::env::var_os("DSH_HOME"),
            flash_data_dir: std::env::var_os("FLASH_DATA_DIR"),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            local_app_data: std::env::var_os("LOCALAPPDATA"),
        };
        Self::resolve_with(&env, &home)
    }

    /// Same as [`resolve`](Self::resolve) without reading the process
    /// environment (for tests). The folder rules live in
    /// [`crate::paths::harness_folders`], beside Codex's resolver.
    pub fn resolve_with(env: &HarnessEnv, home: &Path) -> Result<HarnessPaths, PathsError> {
        let (sessions, reports) = crate::paths::harness_folders(env, home)?;
        Ok(HarnessPaths { sessions, reports })
    }

    /// The folders of a test, never read from the environment.
    pub fn at(sessions: PathBuf, reports: PathBuf) -> HarnessPaths {
        HarnessPaths { sessions, reports }
    }
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
    /// The `title` of the last `session/title` event, as written; an empty or
    /// all-space title, or a last event without one, is no title. A Harness row is named by it, like a
    /// Claude Code row by its `ai-title`; a worker outside a flash job shows it as its task.
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

/// Whether a directory entry is a folder. A type that cannot be read is a
/// warning naming the entry and the error, never silently "not a folder".
fn entry_is_dir(
    file_type: io::Result<fs::FileType>,
    path: &Path,
    warnings: &mut Vec<String>,
) -> bool {
    match file_type {
        Ok(kind) => kind.is_dir(),
        Err(source) => {
            warnings.push(format!(
                "{}: the file type cannot be read: {source}",
                path.display()
            ));
            false
        }
    }
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
        if !entry_is_dir(key.file_type(), &key.path(), &mut read.warnings) {
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
            if !entry_is_dir(entry.file_type(), &entry.path(), &mut read.warnings) {
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
                    session.file = crate::transcripts::file_below(&paths.sessions, &file);
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
                    .and_then(|source| source.model.clone())
                    // An empty or all-space model names none, as in Codex's
                    // sessions: the session keeps its last named model.
                    .filter(|model| !model.trim().is_empty());
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
                // The last event wins; one without a title clears it.
                session.title = event
                    .data
                    .and_then(|data| data.title)
                    .and_then(crate::transcripts::session_title);
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

/// What seatswap reads of one job report: its `sessionId` and its `label`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    session_id: Option<String>,
    label: Option<String>,
}

/// One job report as it was read: the file's job id (`job-<8 hex>` or
/// `flash-task-<uuid>`, the name before `.report.json`), its Harness id and its
/// label. The label is a task text: it is kept in memory only.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobReport {
    job: String,
    session_id: Option<String>,
    label: Option<String>,
}

/// flash-agents' job reports, `<reports>/<key>/patches/<job>.report.json`,
/// each read once (spec §3.8). flash-agents writes a job's report once, when
/// the job ends, so a file already read is not read again; the folders are
/// listed on every [`refresh`](Self::refresh) to find the new ones, and a file
/// that left them is forgotten. A file that cannot be read or parsed is a
/// warning naming it on every refresh while it lasts, and it is read again on
/// the next one; it never gives a label or an id.
///
/// The labels are task texts: they live in this value only and are never
/// written to the history or to any file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobReports {
    /// Every report read, by its file.
    read: BTreeMap<PathBuf, JobReport>,
    /// The first report, in file order, of each job id.
    by_job: BTreeMap<String, PathBuf>,
    /// Each Harness id's label, from the first report in file order that
    /// names it and has a label.
    labels: BTreeMap<String, String>,
}

impl JobReports {
    /// List every report file and read the ones not read before. Returns the
    /// warnings of this refresh: a folder that cannot be listed, a report that
    /// cannot be read or parsed. A missing reports folder is no report and no
    /// warning.
    pub fn refresh(&mut self, paths: &HarnessPaths) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut listed: BTreeSet<PathBuf> = BTreeSet::new();
        let keys = match fs::read_dir(&paths.reports) {
            Ok(keys) => keys,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                self.read.clear();
                self.index();
                return warnings;
            }
            Err(source) => {
                // Nothing can be listed: keep what was read, so a passing
                // error does not drop every label.
                warnings.push(format!("{}: {source}", paths.reports.display()));
                return warnings;
            }
        };
        let mut complete = true;
        for key in keys {
            let key = match key {
                Ok(key) => key,
                Err(source) => {
                    warnings.push(format!("{}: {source}", paths.reports.display()));
                    complete = false;
                    continue;
                }
            };
            if !entry_is_dir(key.file_type(), &key.path(), &mut warnings) {
                continue;
            }
            let patches = key.path().join("patches");
            let entries = match fs::read_dir(&patches) {
                Ok(entries) => entries,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    warnings.push(format!("{}: {source}", patches.display()));
                    complete = false;
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(source) => {
                        warnings.push(format!("{}: {source}", patches.display()));
                        complete = false;
                        continue;
                    }
                };
                let path = entry.path();
                let Some(job) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(REPORT_SUFFIX))
                    .map(str::to_string)
                else {
                    continue;
                };
                listed.insert(path.clone());
                if self.read.contains_key(&path) {
                    continue;
                }
                if let Some(report) = read_report(&path, job, &mut warnings) {
                    self.read.insert(path, report);
                }
            }
        }
        // A folder that could not be listed may still hold its reports.
        if complete {
            self.read.retain(|path, _| listed.contains(path));
        }
        self.index();
        warnings
    }

    /// Rebuild the look-ups from the reports read, in file order.
    fn index(&mut self) {
        self.by_job.clear();
        self.labels.clear();
        for (path, report) in &self.read {
            self.by_job
                .entry(report.job.clone())
                .or_insert_with(|| path.clone());
            if let (Some(session), Some(label)) = (&report.session_id, &report.label) {
                self.labels
                    .entry(session.clone())
                    .or_insert_with(|| label.clone());
            }
        }
    }

    /// The label of the job whose Harness id is `id`, from the report that
    /// names it in its `sessionId`; `None` while no such report exists (a
    /// running job) or when it has no label. An empty or all-space label is
    /// no label.
    pub fn label(&self, id: &str) -> Option<&str> {
        self.labels.get(id).map(String::as_str)
    }

    /// The `sessionId` of job `job`'s report; a report without one is a
    /// warning naming the file. `None` when no report of that job was read.
    fn session_of(&self, job: &str, warnings: &mut Vec<String>) -> Option<String> {
        let path = self.by_job.get(job)?;
        let report = self.read.get(path)?;
        match &report.session_id {
            Some(session) => Some(session.clone()),
            None => {
                warnings.push(format!("{}: no sessionId", path.display()));
                None
            }
        }
    }
}

/// Read one report file. A file that cannot be read or is not the report's
/// JSON is a warning naming it, and `None`.
fn read_report(path: &Path, job: String, warnings: &mut Vec<String>) -> Option<JobReport> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) => {
            warnings.push(format!("{}: {source}", path.display()));
            return None;
        }
    };
    match serde_json::from_slice::<Report>(&bytes) {
        Ok(report) => Some(JobReport {
            job,
            session_id: report.session_id,
            label: report.label.filter(|label| !label.trim().is_empty()),
        }),
        Err(error) => {
            warnings.push(format!(
                "{}: not valid JSON at line {} column {}",
                path.display(),
                error.line(),
                error.column()
            ));
            None
        }
    }
}

/// The Claude Code sessions whose transcripts named a Harness job, and the jobs
/// of Claude Code sessions that have left the scan state. Every
/// `flash-task-<uuid>` is taken as it is; every `job-<8 hex>` id is turned into
/// its `flash-task-…` id through its report (`reports`, read with
/// [`JobReports::refresh`]).
pub fn links(
    reports: &JobReports,
    seen: &BTreeMap<String, SessionSeen>,
    closed: &BTreeMap<String, ClosedJob>,
) -> HarnessLinks {
    let mut links = HarnessLinks::default();
    // A resumed session carries the old transcript's tool results, so the same
    // job can be named by two sessions: the later session `last` owns it.
    let mut owner_at: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    for (session, seen) in seen {
        for id in &seen.flash_jobs {
            name_job(reports, id, session, seen.last, &mut links, &mut owner_at);
        }
    }
    for (id, link) in closed {
        name_job(
            reports,
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
    reports: &JobReports,
    id: &str,
    session: &str,
    last: DateTime<Utc>,
    links: &mut HarnessLinks,
    owner_at: &mut BTreeMap<String, DateTime<Utc>>,
) {
    let job = if is_flash_job(id) {
        Some(id.to_string())
    } else if id.starts_with("job-") {
        reports.session_of(id, &mut links.warnings)
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

/// The processes a [`proc_links`] walk can read. The app uses [`RealProc`]; a
/// test builds its own table.
pub trait ProcSource {
    /// Every pid the system lists, or why the table cannot be read.
    fn pids(&self) -> io::Result<Vec<u32>>;
    /// The files `pid` holds open, or why its list cannot be read.
    fn open_files(&self, pid: u32) -> io::Result<Vec<PathBuf>>;
    /// The pid's parent, when the system says.
    fn parent(&self, pid: u32) -> Option<u32>;
}

/// The real `/proc` on Linux; elsewhere it has no processes to walk, and
/// [`proc_links`] returns no link (the documented rule: on Windows a running
/// job appears once its result is collected in a transcript).
#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone, Copy)]
pub struct RealProc;

#[cfg(target_os = "linux")]
impl ProcSource for RealProc {
    fn pids(&self) -> io::Result<Vec<u32>> {
        let entries = fs::read_dir("/proc")?;
        let mut pids = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Ok(pid) = name.parse() {
                pids.push(pid);
            }
        }
        Ok(pids)
    }

    fn open_files(&self, pid: u32) -> io::Result<Vec<PathBuf>> {
        let entries = fs::read_dir(format!("/proc/{pid}/fd"))?;
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry?;
            if let Ok(path) = fs::read_link(entry.path()) {
                files.push(path);
            }
        }
        Ok(files)
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
    fn pids(&self) -> io::Result<Vec<u32>> {
        Ok(Vec::new())
    }

    fn open_files(&self, _pid: u32) -> io::Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    fn parent(&self, _pid: u32) -> Option<u32> {
        None
    }
}

/// What one walk of the process table found: the locks held, the Claude Code
/// sessions their holders hang under, every file a process holds open, and what
/// could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcLinks {
    /// Harness session id → Claude Code session id.
    pub owners: BTreeMap<String, String>,
    /// The Harness sessions whose lock a process holds.
    pub locks: BTreeSet<String>,
    /// Every file a process holds open. Codex's sessions use the same walk to
    /// find their rollout files (C-1); the set is empty where there is no
    /// `/proc`.
    pub files: BTreeSet<PathBuf>,
    /// A process table, a file list or the sessions folder that could not be
    /// read, naming the path and the error.
    pub warnings: Vec<String>,
}

/// Walk the process table once: every Harness lock held, the Claude Code
/// session of a lock holder's parent when there is one, and every file a
/// process holds open. The sessions folder is resolved once, because the kernel
/// reports an open file by its resolved path; a folder that does not exist has
/// no lock to find and is no warning. Any other `canonicalize` error is a
/// warning naming the folder and the error, never a silent empty walk.
pub fn proc_links(
    paths: &HarnessPaths,
    procs: &dyn ProcSource,
    claude_pids: &BTreeMap<u32, String>,
) -> ProcLinks {
    match fs::canonicalize(&paths.sessions) {
        Ok(sessions) => proc_links_in(Some(&sessions), procs, claude_pids),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            proc_links_in(None, procs, claude_pids)
        }
        Err(source) => {
            let mut links = proc_links_in(None, procs, claude_pids);
            links.warnings.push(format!(
                "{}: the folder cannot be resolved: {source}",
                paths.sessions.display()
            ));
            links
        }
    }
}

/// The same one walk with the Harness sessions folder already resolved;
/// `None` when there is no Harness store, so only the open files are collected
/// (C-1 needs them for Codex when there is no Harness folder).
#[cfg(target_os = "linux")]
pub fn proc_links_in(
    sessions: Option<&Path>,
    procs: &dyn ProcSource,
    claude_pids: &BTreeMap<u32, String>,
) -> ProcLinks {
    let mut links = ProcLinks::default();
    let pids = match procs.pids() {
        Ok(pids) => pids,
        Err(source) => {
            links.warnings.push(format!("/proc: {source}"));
            return links;
        }
    };
    for pid in pids {
        let files = match procs.open_files(pid) {
            Ok(files) => files,
            Err(source) => {
                links.warnings.push(format!("/proc/{pid}/fd: {source}"));
                continue;
            }
        };
        let ids: BTreeSet<String> = match sessions {
            Some(sessions) => files
                .iter()
                .filter_map(|file| harness_id_of(sessions, file))
                .collect(),
            None => BTreeSet::new(),
        };
        links.files.extend(files);
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
pub fn proc_links_in(
    _sessions: Option<&Path>,
    _procs: &dyn ProcSource,
    _claude_pids: &BTreeMap<u32, String>,
) -> ProcLinks {
    ProcLinks::default()
}

/// Whether a Harness session is working now: it has no `turn/end` after its
/// last `turn/start`, and either a process holds its lock or its last event is
/// less than [`HARNESS_QUIET_MINUTES`] old. An unfinished file that has been
/// quiet for longer and has no lock holder is a cancelled or killed job, shown
/// as done.
pub fn is_running(session: &HarnessSession, lock_held: bool, now: DateTime<Utc>) -> bool {
    !session.finished && is_alive(session, lock_held, now)
}

/// Whether a Harness session's process is still there: a process holds its
/// lock, or its last event is less than [`HARNESS_QUIET_MINUTES`] old. A
/// session of its own between two turns (waiting for its user) is open; a
/// flash job does one task, so once its turn ended it is done.
pub fn is_open(session: &HarnessSession, lock_held: bool, now: DateTime<Utc>) -> bool {
    if is_flash_job(&session.id) {
        is_running(session, lock_held, now)
    } else {
        is_alive(session, lock_held, now)
    }
}

fn is_alive(session: &HarnessSession, lock_held: bool, now: DateTime<Utc>) -> bool {
    lock_held || now.signed_duration_since(session.last) < Duration::minutes(HARNESS_QUIET_MINUTES)
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

    /// flash-agents names a folder's worker copies by the first 12 hex digits
    /// of the SHA-256 of its path (seen with 0.1.13: `~/projects/seatswap` →
    /// `6228ea01ce3f`); a copy's subfolder is kept, and a copy of a folder no
    /// root names, or a path that is no copy, gives none.
    #[test]
    fn a_worker_copy_names_the_folder_it_was_copied_from() {
        assert_eq!(copy_key("/home/sam/projects/atlas"), "8279d42859b9");
        let roots = ["/home/sam/projects/ledger", "/home/sam/projects/atlas"];

        assert_eq!(
            copy_source(Path::new("/home/sam/fm/8279d42859b9/slots/10"), &roots).as_deref(),
            Some("/home/sam/projects/atlas")
        );
        assert_eq!(
            copy_source(
                Path::new("/home/sam/fm/8279d42859b9/slots/3/crates/core"),
                &roots
            )
            .map(PathBuf::from),
            Some(
                PathBuf::from("/home/sam/projects/atlas")
                    .join("crates")
                    .join("core")
            ),
            "the copy's subfolder is kept"
        );
        assert_eq!(
            copy_source(Path::new(CWD), &roots),
            None,
            "no root has that key"
        );
        assert_eq!(
            copy_source(Path::new("/home/sam/projects/atlas"), &roots),
            None,
            "not a copy"
        );
        assert_eq!(
            copy_source(Path::new("/home/sam/fm/8279d42859b9/slot/10"), &roots),
            None,
            "not the copies' layout"
        );
    }

    /// Every layout flash-agents keeps its worker copies in, with every slot
    /// name: `<base>/<key>/slots/<n>`, the older `<key>/<pid>/slots/<slot>`
    /// and `<tmp>/flash-mcp/<key>/<pid>/<instance>/slots/<slot>`. Anything
    /// between the key and `slots` other than a pid and an instance, or a
    /// slot of another shape, is no copy.
    #[test]
    fn every_worker_copy_layout_and_slot_name_names_its_source() {
        let roots = ["/home/sam/projects/ledger", "/home/sam/projects/atlas"];
        let atlas = Some("/home/sam/projects/atlas");
        for slot in ["10", "slot-0", "slot-3-12"] {
            for copy in [
                format!("/home/sam/fm/8279d42859b9/slots/{slot}"),
                format!("/home/sam/fm/8279d42859b9/8308/slots/{slot}"),
                format!("/tmp/flash-mcp/8279d42859b9/8308/8aef170c/slots/{slot}"),
            ] {
                assert_eq!(
                    copy_source(Path::new(&copy), &roots).as_deref(),
                    atlas,
                    "{copy}"
                );
            }
        }
        assert_eq!(
            copy_source(
                Path::new("/tmp/flash-mcp/8279d42859b9/8308/8aef170c/slots/slot-0/crates/core"),
                &roots
            )
            .map(PathBuf::from),
            Some(
                PathBuf::from("/home/sam/projects/atlas")
                    .join("crates")
                    .join("core")
            ),
            "everything after the slot is the copy's subfolder"
        );
        for copy in [
            "/home/sam/fm/8279d42859b9/slots/slot-",
            "/home/sam/fm/8279d42859b9/slots/slot-1-2-3",
            "/home/sam/fm/8279d42859b9/slots/slot-a",
            "/home/sam/fm/8279d42859b9/slots/s1",
            "/home/sam/fm/8279d42859b9/slots",
            "/home/sam/fm/8279d42859b9/work/slots/1",
            "/home/sam/fm/8279d42859b9/8308/work/slots/1",
            "/home/sam/fm/8279d42859b9/8308/8aef170c/77/slots/1",
            "/home/sam/fm/8279D42859B9/slots/1",
        ] {
            assert_eq!(copy_source(Path::new(copy), &roots), None, "{copy}");
        }
    }

    /// On Windows a folder may be recorded with either case of its drive
    /// letter and hashed by flash-agents with the other (`c:\projects\seatswap`
    /// → `80577963972b`, `C:\projects\seatswap` → `eae022a91fde`): both
    /// match, and the source keeps the spelling the root was recorded with.
    /// The paths are written with `/`, so the test runs on every system.
    #[test]
    fn a_windows_root_matches_with_either_drive_letter() {
        assert_eq!(copy_key(r"c:\projects\seatswap"), "80577963972b");
        assert_eq!(copy_key(r"C:\projects\seatswap"), "eae022a91fde");
        for key in ["80577963972b", "eae022a91fde"] {
            let copy = format!("/Temp/flash-mcp/{key}/8308/8aef170c/slots/slot-0");
            for root in [r"C:\projects\seatswap", r"c:\projects\seatswap"] {
                assert_eq!(
                    copy_source(Path::new(&copy), &[root]).as_deref(),
                    Some(root),
                    "{copy} from {root}"
                );
            }
        }
        assert_eq!(
            copy_source(
                Path::new("/Temp/flash-mcp/80577963972b/8308/8aef170c/slots/slot-0"),
                &[r"C:\projects\other"]
            ),
            None,
            "another folder on the same drive"
        );
    }

    fn store() -> (TempDir, HarnessPaths) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = HarnessPaths::at(tmp.path().join("sessions"), tmp.path().join("flash-agents"));
        (tmp, paths)
    }

    #[test]
    fn a_relative_dsh_home_is_an_error_naming_the_variable_and_the_value() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let env = HarnessEnv {
            dsh_home: Some("dsh".into()),
            ..HarnessEnv::default()
        };
        let error = HarnessPaths::resolve_with(&env, tmp.path())
            .expect_err("a relative DSH_HOME is refused");

        assert!(
            matches!(
                &error,
                PathsError::RelativeHarnessFolder { variable: "DSH_HOME", value }
                    if value == Path::new("dsh")
            ),
            "expected a relative DSH_HOME error, got {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "$DSH_HOME must be an absolute path, got dsh"
        );
    }

    /// The variables reach the folder rules of `paths::harness_folders`, whose
    /// tests cover every platform's default.
    #[test]
    fn absolute_harness_folders_are_used() {
        let root = Path::new(if cfg!(windows) { r"C:\" } else { "/" });
        let home = root.join("home").join("sam");
        let dsh = root.join("state").join("dsh");
        let data = root.join("state").join("flash");

        let env = HarnessEnv {
            dsh_home: Some(dsh.clone().into_os_string()),
            flash_data_dir: Some(data.clone().into_os_string()),
            ..HarnessEnv::default()
        };
        let paths = HarnessPaths::resolve_with(&env, &home).expect("absolute folders are fine");
        assert_eq!(paths.sessions, dsh.join("sessions"));
        assert_eq!(paths.reports, data);

        let paths =
            HarnessPaths::resolve_with(&HarnessEnv::default(), &home).expect("the defaults");
        assert_eq!(paths.sessions, home.join(".dsh").join("sessions"));
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

    /// The job reports of the store, read once as the sessions thread does.
    fn reports_of(paths: &HarnessPaths) -> JobReports {
        let mut reports = JobReports::default();
        let warnings = reports.refresh(paths);
        assert!(warnings.is_empty(), "{warnings:?}");
        reports
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

    /// An answer whose model is empty or all spaces names no model: the
    /// session keeps the last one named, and a session that never named one
    /// has none, so no blank model reaches the window.
    #[test]
    fn an_empty_or_blank_model_is_no_model() {
        let (_tmp, paths) = store();
        let mut lines = job_lines();
        lines.push(assistant(1794696063000, "", 100, 50));
        lines.push(assistant(1794696064000, "   ", 100, 50));
        write_lines(&paths, JOB, &lines);
        write_lines(
            &paths,
            WORKER,
            &[
                header(WORKER, Some(JOB), 1, 1794696055000),
                assistant(1794696056000, " ", 2200, 1100),
            ],
        );

        let read = sessions(&paths, since());

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        let job = read.sessions.iter().find(|s| s.id == JOB).expect("the job");
        assert_eq!(job.model.as_deref(), Some(MODEL), "the last named model");
        assert_eq!(job.tokens.out, 3585 + 100, "the answers still count");
        let blank: Vec<_> = job
            .messages_detail
            .iter()
            .map(|m| m.model.clone())
            .collect();
        assert_eq!(blank[blank.len() - 2..], [None, None]);
        let worker = read
            .sessions
            .iter()
            .find(|s| s.id == WORKER)
            .expect("the worker");
        assert_eq!(worker.model, None);
        assert!(worker.messages_detail.iter().all(|m| m.model.is_none()));
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
        fn pids(&self) -> io::Result<Vec<u32>> {
            Ok(self.processes.keys().copied().collect())
        }

        fn open_files(&self, pid: u32) -> io::Result<Vec<PathBuf>> {
            Ok(self
                .processes
                .get(&pid)
                .map(|(files, _)| files.clone())
                .unwrap_or_default())
        }

        fn parent(&self, pid: u32) -> Option<u32> {
            self.processes.get(&pid).and_then(|(_, parent)| *parent)
        }
    }

    /// A process table that cannot be read, for the warning tests (Linux only,
    /// where the walk exists).
    #[cfg(target_os = "linux")]
    struct BrokenProc {
        pids: bool,
    }

    #[cfg(target_os = "linux")]
    impl ProcSource for BrokenProc {
        fn pids(&self) -> io::Result<Vec<u32>> {
            if self.pids {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            } else {
                Ok(vec![500])
            }
        }

        fn open_files(&self, _pid: u32) -> io::Result<Vec<PathBuf>> {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        }

        fn parent(&self, _pid: u32) -> Option<u32> {
            None
        }
    }

    // The process walk exists on Linux only (docs/windows.md).
    #[test]
    #[cfg(target_os = "linux")]
    fn an_unreadable_process_table_is_a_warning() {
        let (_tmp, paths) = store();

        let links = proc_links(&paths, &BrokenProc { pids: true }, &BTreeMap::new());

        assert!(links.owners.is_empty());
        assert_eq!(links.warnings.len(), 1, "{:?}", links.warnings);
        assert!(
            links.warnings[0].contains("/proc") && links.warnings[0].contains("denied"),
            "the warning names the path and the error: {}",
            links.warnings[0]
        );
    }

    // The process walk exists on Linux only (docs/windows.md).
    #[test]
    #[cfg(target_os = "linux")]
    fn an_unreadable_file_list_is_a_warning() {
        let (_tmp, paths) = store();

        let links = proc_links(&paths, &BrokenProc { pids: false }, &BTreeMap::new());

        assert!(links.owners.is_empty());
        assert_eq!(links.warnings.len(), 1, "{:?}", links.warnings);
        assert!(
            links.warnings[0].contains("/proc/500/fd") && links.warnings[0].contains("denied"),
            "the warning names the path and the error: {}",
            links.warnings[0]
        );
    }

    // A file used as a parent folder is ENOTDIR on Unix; Windows reports it as not found.
    #[test]
    #[cfg(unix)]
    fn a_sessions_folder_that_cannot_be_resolved_is_a_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A regular file cannot be a parent folder, so resolving below it fails
        // with more than "not found".
        let not_a_folder = tmp.path().join("store");
        fs::write(&not_a_folder, []).expect("a file");
        let paths = HarnessPaths::at(
            not_a_folder.join("sessions"),
            tmp.path().join("flash-agents"),
        );
        let procs = FakeProc {
            processes: BTreeMap::new(),
        };

        let links = proc_links(&paths, &procs, &BTreeMap::new());

        assert_eq!(links.warnings.len(), 1, "{:?}", links.warnings);
        assert!(
            links.warnings[0].contains(&paths.sessions.display().to_string()),
            "the warning names the folder: {}",
            links.warnings[0]
        );
        assert!(
            links.warnings[0].contains("cannot be resolved"),
            "the warning says what failed: {}",
            links.warnings[0]
        );
    }

    #[test]
    fn an_unreadable_file_type_is_a_warning_naming_the_entry() {
        let mut warnings = Vec::new();
        let is_dir = entry_is_dir(
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
            Path::new("/home/sam/harness/key"),
            &mut warnings,
        );

        assert!(!is_dir, "a type that cannot be read is not a folder");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("/home/sam/harness/key"));
        assert!(warnings[0].contains("denied"));
    }

    // The process walk exists on Linux only (docs/windows.md).
    #[test]
    #[cfg(target_os = "linux")]
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

        let owners = proc_links(&paths, &procs, &claude_pids).owners;

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

        let owners = proc_links(&paths, &procs, &claude_pids).owners;

        assert!(owners.is_empty(), "{owners:?}");
    }

    // The process walk exists on Linux only (docs/windows.md).
    #[test]
    #[cfg(target_os = "linux")]
    fn one_walk_collects_the_locks_and_the_open_files() {
        let (tmp, paths) = store();
        let lock = paths.sessions.join(KEY).join(JOB).join(SESSION_LOCK);
        fs::create_dir_all(lock.parent().expect("a session folder")).expect("a session folder");
        fs::write(&lock, "").expect("a lock");
        let lock = fs::canonicalize(&lock).expect("the resolved lock");
        let rollout = tmp
            .path()
            .join("codex")
            .join("sessions")
            .join("2026")
            .join("11")
            .join("14")
            .join("rollout-2026-11-14T17-53-10-3f7b2c1e-9a4d-4e8f-8b6a-2c5d9e1f7a30.jsonl");
        let procs = FakeProc {
            processes: BTreeMap::from([
                (500, (vec![lock.clone(), rollout.clone()], Some(1))),
                (1, (Vec::new(), None)),
            ]),
        };

        let links = proc_links(&paths, &procs, &BTreeMap::new());

        assert!(links.locks.contains(JOB));
        assert!(links.files.contains(&lock));
        assert!(
            links.files.contains(&rollout),
            "the same walk gives Codex the open files"
        );
    }

    #[test]
    fn a_job_report_gives_its_flash_task_id() {
        let (_tmp, paths) = store();
        write_report(&paths, "job-3e32c2fc", JOB);
        let mut seen = BTreeMap::new();
        let mut session = test_seen();
        session.flash_jobs = BTreeSet::from(["job-3e32c2fc".to_string()]);
        seen.insert(SESSION.to_string(), session);

        let links = links(&reports_of(&paths), &seen, &BTreeMap::new());

        assert!(links.warnings.is_empty(), "{:?}", links.warnings);
        assert_eq!(links.owners.get(JOB).map(String::as_str), Some(SESSION));
    }

    /// A flash job's label comes from its report, found by the report's
    /// `sessionId` for a background (`job-…`) and a direct (`flash-task-…`)
    /// call alike; a report is parsed once, a broken one is a warning naming
    /// it and gives no label, and a report that left the folder is forgotten.
    #[test]
    fn a_job_report_gives_its_label_and_is_read_once() {
        let (_tmp, paths) = store();
        let direct = "flash-task-8d2f0e7c-6b1a-4c3d-9e0f-1a2b3c4d5e6f";
        write_report(&paths, "job-3e32c2fc", JOB);
        let dir = paths.reports.join("acme").join("patches");
        fs::write(
            dir.join(format!("{direct}.report.json")),
            serde_json::json!({
                "key": "acme", "kind": "task", "sessionId": direct,
                "label": "Fix the invoice export", "status": "done",
                "stopReason": null, "timing": {}, "createdAt": "2026-11-14T09:41:00.000Z",
                "report": {}
            })
            .to_string(),
        )
        .expect("a direct call's report");
        // Not a report: a patch beside it is never read.
        fs::write(dir.join("job-3e32c2fc.patch"), "diff --git").expect("a patch");
        let mut reports = JobReports::default();

        assert!(reports.refresh(&paths).is_empty());
        assert_eq!(reports.label(JOB), Some("atlas link test"));
        assert_eq!(reports.label(direct), Some("Fix the invoice export"));
        assert_eq!(reports.label(WORKER), None, "no report names a worker");

        // A report is written once: a file already read is not parsed again.
        fs::write(dir.join("job-3e32c2fc.report.json"), "{not json").expect("rewritten");
        assert!(reports.refresh(&paths).is_empty());
        assert_eq!(reports.label(JOB), Some("atlas link test"));

        // A new report that cannot be parsed names its file, on every refresh.
        let broken = dir.join("job-0a1b2c3d.report.json");
        fs::write(&broken, "{\"sessionId\": ").expect("a broken report");
        for _ in 0..2 {
            let warnings = reports.refresh(&paths);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                warnings[0].contains(broken.to_str().expect("a text path")),
                "{warnings:?}"
            );
        }

        fs::remove_file(dir.join(format!("{direct}.report.json"))).expect("removed");
        fs::remove_file(&broken).expect("removed");
        assert!(reports.refresh(&paths).is_empty());
        assert_eq!(reports.label(direct), None, "a removed report is forgotten");

        let (_none, missing) = store();
        let mut empty = JobReports::default();
        assert!(
            empty.refresh(&missing).is_empty(),
            "a store without reports is no warning"
        );
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
            title: None,
            custom_title: None,
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

        let links = links(&reports_of(&paths), &seen, &BTreeMap::new());

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

        let found = links(&reports_of(&paths), &BTreeMap::new(), &closed);
        assert_eq!(found.owners.get(JOB).map(String::as_str), Some(SESSION));

        let mut newer = test_seen();
        newer.flash_jobs = BTreeSet::from([JOB.to_string()]);
        newer.last = "2026-11-14T11:00:00Z".parse().expect("a time");
        let seen = BTreeMap::from([("ffffffff-0000-4000-8000-000000000002".to_string(), newer)]);
        let found = links(&reports_of(&paths), &seen, &closed);
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

        let owners = proc_links(&paths, &procs, &claude_pids).owners;

        assert_eq!(owners.get(JOB).map(String::as_str), Some(SESSION));
    }
}
