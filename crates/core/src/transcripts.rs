//! Claude Code's transcripts: reading them incrementally, and what they say
//! about answers, user turns and agents (spec §1.6).
//!
//! Only numbers and names leave the parser: time, folder, model, block kinds,
//! token counts, session and agent ids. Message text is never stored, logged,
//! put into a warning or sent anywhere.
//!
//! The state is kept as one file per project folder in
//! [`AppPaths::transcripts_state_dir`], because one machine had 7,908
//! transcripts in 37 folders: a single file would be megabytes, rewritten at
//! every wake.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Timelike, Utc};
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::atomic::{self, Access};
use crate::history::TokenRecord;
use crate::paths::{AppPaths, ClaudePaths};
use crate::project::{self, ProjectKey};

/// The version of the state files this module writes and reads.
const STATE_VERSION: u32 = 1;
/// How many message ids a growing transcript keeps, so that an answer is
/// counted once even when its lines arrive far apart.
const RECENT_IDS: usize = 500;
/// A transcript that has not grown for this long drops its ids; it is read to
/// its end then, so nothing can be counted twice.
const KEEP_IDS_FOR_HOURS: i64 = 24;
/// An answer of this model never reached the API: no tokens to count.
const SYNTHETIC: &str = "<synthetic>";
/// How much of a transcript is read at a time.
const CHUNK_BYTES: usize = 64 * 1024;
/// The UTF-8 byte order mark, which some writers put at offset 0 of a file.
const BYTE_ORDER_MARK: [u8; 3] = [0xEF, 0xBB, 0xBF];
/// The two block kinds that mean the answer thought before it spoke.
const THINKING_BLOCKS: [&str; 2] = ["thinking", "redacted_thinking"];

#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: {source}")]
    State {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: transcripts state version {found} is not version {STATE_VERSION}")]
    Version { path: PathBuf, found: u32 },
}

/// One answer of a transcript, counted once by its message id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub session: String,
    pub agent: Option<String>,
    pub message_id: String,
    pub at: DateTime<Utc>,
    pub cwd: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub thinking: bool,
}

/// A real user message of a main transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTurn {
    pub session: String,
    pub at: DateTime<Utc>,
}

/// One `agent-<id>.meta.json` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMeta {
    pub session: String,
    pub agent: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub finished: bool,
    pub spawn_depth: u32,
}

/// How far a transcript was read, and the ids of the answers it gave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileState {
    /// The offset just after the last whole line that was read.
    pub offset: u64,
    /// The file's modification time when the scan last read new bytes from it.
    pub grew: DateTime<Utc>,
    /// The last [`RECENT_IDS`] message ids, while the file still grows.
    pub recent: Vec<String>,
    /// The time of the newest answer this file has given; absent in a state file
    /// written before this field means `None`.
    pub last_answer: Option<DateTime<Utc>>,
}

/// What a main transcript says about its session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSeen {
    /// The name of the project folder the transcript is in.
    pub folder: String,
    pub cwd: String,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub last_model: Option<String>,
    pub last_user_turn: Option<DateTime<Utc>>,
    pub thinking_since_user: u32,
    /// The session record last written from this session, so a restart does not
    /// write the same record again. Absent in an old state file means none.
    #[serde(default)]
    pub recorded: Option<RecordedSession>,
}

/// The session record last written from a [`SessionSeen`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedSession {
    /// The `last` answer time the record covers.
    pub last: DateTime<Utc>,
    /// Whether that record was the closing one (`closed: true`).
    pub closed: bool,
}

/// Everything remembered between two scans.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanState {
    pub version: u32,
    /// Keyed by the path below `projects_dir` with `/`: `<folder>/<session>.jsonl`
    /// or `<folder>/<session>/subagents/agent-<id>.jsonl`.
    pub files: BTreeMap<String, FileState>,
    pub sessions: BTreeMap<String, SessionSeen>,
    /// The project folders whose state changed since the last save.
    #[serde(skip)]
    changed: BTreeSet<String>,
}

impl ScanState {
    /// Read the state of every project folder. A missing folder is the default.
    pub fn load(app: &AppPaths) -> Result<ScanState, TranscriptError> {
        let dir = app.transcripts_state_dir();
        let mut state = ScanState {
            version: STATE_VERSION,
            ..ScanState::default()
        };
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(state),
            Err(source) => return Err(TranscriptError::Io { path: dir, source }),
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| TranscriptError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let kind = entry.file_type().map_err(|source| TranscriptError::Io {
                path: path.clone(),
                source,
            })?;
            if kind.is_file() {
                paths.push(path);
            }
        }
        paths.sort();
        for path in paths {
            let bytes = fs::read(&path).map_err(|source| TranscriptError::Io {
                path: path.clone(),
                source,
            })?;
            let file: StateFile =
                serde_json::from_slice(&bytes).map_err(|source| TranscriptError::State {
                    path: path.clone(),
                    source,
                })?;
            if file.version != STATE_VERSION {
                return Err(TranscriptError::Version {
                    path,
                    found: file.version,
                });
            }
            state.files.extend(file.files);
            state.sessions.extend(file.sessions);
        }
        Ok(state)
    }

    /// Write the state files of the project folders that changed since the last
    /// save, and return how many were written or deleted.
    pub fn save(
        &mut self,
        claude: &ClaudePaths,
        app: &AppPaths,
        now: DateTime<Utc>,
    ) -> Result<u32, TranscriptError> {
        let projects = claude.projects_dir();
        // Check every transcript first, without changing anything: an error
        // leaves the state exactly as it was before the call.
        let mut gone: Vec<String> = Vec::new();
        for key in self.files.keys() {
            match fs::metadata(projects.join(key)) {
                Ok(_) => {}
                // Claude Code removed the transcript; its entry goes.
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    gone.push(key.clone());
                }
                // An unreadable transcript is not a gone one: nothing is pruned.
                Err(source) => {
                    return Err(TranscriptError::Io {
                        path: projects.join(key),
                        source,
                    })
                }
            }
        }
        // Prune: a transcript that has not grown for 24 hours drops its ids, an
        // entry whose transcript is gone is removed, and so is a session without
        // any transcript left.
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for (key, file) in &mut self.files {
            if now.signed_duration_since(file.grew) > Duration::hours(KEEP_IDS_FOR_HOURS)
                && !file.recent.is_empty()
            {
                file.recent.clear();
                touched.insert(folder_of(key).to_string());
            }
        }
        for key in gone {
            self.files.remove(&key);
            touched.insert(folder_of(&key).to_string());
        }
        // The (folder, session) pairs that still have a transcript, built once.
        let mut with_transcript: BTreeSet<(String, String)> = BTreeSet::new();
        for key in self.files.keys() {
            if let Some((folder, rest)) = key.split_once('/') {
                let session = match rest.split_once('/') {
                    Some((session, _)) => session,
                    None => rest.strip_suffix(".jsonl").unwrap_or(rest),
                };
                with_transcript.insert((folder.to_string(), session.to_string()));
            }
        }
        let without_transcript: Vec<String> = self
            .sessions
            .iter()
            .filter(|(session, seen)| {
                !with_transcript.contains(&(seen.folder.clone(), (*session).clone()))
            })
            .map(|(session, _)| session.clone())
            .collect();
        for session in without_transcript {
            if let Some(seen) = self.sessions.remove(&session) {
                touched.insert(seen.folder);
            }
        }
        for folder in touched {
            self.changed.insert(folder);
        }

        let dir = app.transcripts_state_dir();
        if !self.changed.is_empty() {
            atomic::ensure_private_dir(&dir).map_err(|source| TranscriptError::Io {
                path: dir.clone(),
                source,
            })?;
        }
        let mut written = 0u32;
        for folder in &self.changed {
            let state = self.state_of(folder);
            let path = dir.join(format!("{folder}.json"));
            if state.files.is_empty() && state.sessions.is_empty() {
                match fs::remove_file(&path) {
                    Ok(()) => written += 1,
                    Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => return Err(TranscriptError::Io { path, source }),
                }
            } else {
                let bytes =
                    serde_json::to_vec(&state).map_err(|source| TranscriptError::State {
                        path: path.clone(),
                        source,
                    })?;
                atomic::write_atomic(&path, &bytes, Access::Private).map_err(|source| {
                    TranscriptError::Io {
                        path: path.clone(),
                        source,
                    }
                })?;
                written += 1;
            }
        }
        self.version = STATE_VERSION;
        self.changed.clear();
        Ok(written)
    }

    fn touch(&mut self, folder: &str) {
        self.changed.insert(folder.to_string());
    }

    /// The entries of one project folder, as its state file holds them.
    fn state_of(&self, folder: &str) -> StateFile {
        let mut state = StateFile {
            version: STATE_VERSION,
            files: BTreeMap::new(),
            sessions: BTreeMap::new(),
        };
        for (key, file) in &self.files {
            if folder_of(key) == folder {
                state.files.insert(key.clone(), file.clone());
            }
        }
        for (session, seen) in &self.sessions {
            if seen.folder == folder {
                state.sessions.insert(session.clone(), seen.clone());
            }
        }
        state
    }
}

/// What one scan found, including the state it read into. The state is staged:
/// [`commit`](Scan::commit) moves it into the caller's [`ScanState`] once the
/// answers' token lines were written. A scan that is dropped leaves the old
/// state, so the next scan reads the same lines again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    pub answers: Vec<Answer>,
    pub user_turns: Vec<UserTurn>,
    pub complete: bool,
    pub warnings: Vec<String>,
    /// The state after this scan; [`commit`](Scan::commit) moves it out.
    next: ScanState,
}

impl Scan {
    /// The state after this scan: the sessions it saw and the offsets it read
    /// to. `token_records` needs it to name a session's folder.
    pub fn state(&self) -> &ScanState {
        &self.next
    }

    /// Move the scan state past what this scan read. Call it only after the
    /// answers' token lines were written: a scan that is not committed reads
    /// the same lines again next time, and its answers are counted again then.
    pub fn commit(&mut self, state: &mut ScanState) {
        *state = std::mem::take(&mut self.next);
    }
}

/// Read what is new in every transcript under `projects_dir`. The [`ScanState`]
/// is not changed; the result carries the state to [`commit`](Scan::commit)
/// once the answers' token lines are written.
pub fn scan(
    claude: &ClaudePaths,
    state: &ScanState,
    budget_bytes: u64,
) -> Result<Scan, TranscriptError> {
    let mut next = state.clone();
    let projects = claude.projects_dir();
    let mut files = candidates(&projects)?;
    // A file whose length equals its stored offset has nothing new: it is not opened.
    files.retain(|candidate| {
        candidate.len != next.files.get(&candidate.key).map_or(0, |file| file.offset)
    });
    // Running sessions come before a backlog.
    files.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.key.cmp(&b.key)));

    let mut run = ScanRun {
        state: &mut next,
        budget: budget_bytes,
        used: 0,
        answers: Vec::new(),
        user_turns: Vec::new(),
        warnings: Vec::new(),
    };
    let mut complete = true;
    for (index, candidate) in files.iter().enumerate() {
        if run.used >= run.budget {
            complete = false;
            break;
        }
        let outcome = run.read(candidate)?;
        run.used = run.used.saturating_add(outcome.consumed);
        if outcome.budget_hit {
            if !outcome.at_eof || index + 1 < files.len() {
                complete = false;
            }
            break;
        }
    }
    let ScanRun {
        answers,
        user_turns,
        warnings,
        ..
    } = run;
    Ok(Scan {
        answers,
        user_turns,
        complete,
        warnings,
        next,
    })
}

/// The agents a session started, from their `*.meta.json` files. Unreadable
/// files are left out.
pub fn agents_of(claude: &ClaudePaths, session: &str) -> Vec<AgentMeta> {
    let mut out = Vec::new();
    let dir = claude.projects_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return out;
    };
    let mut folders: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.path().is_dir() {
            folders.push(entry.path());
        }
    }
    folders.sort();
    for folder in folders {
        let subagents = folder.join(session).join("subagents");
        let Ok(entries) = fs::read_dir(&subagents) else {
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let file_name = entry.file_name();
            let Some(agent) = file_name.to_str().and_then(agent_meta_id) else {
                continue;
            };
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(meta) = serde_json::from_slice::<AgentMetaFile>(&bytes) else {
                continue;
            };
            out.push(AgentMeta {
                session: session.to_string(),
                agent: agent.to_string(),
                agent_type: meta.agent_type,
                description: meta.description,
                finished: meta.finished.unwrap_or(false),
                spawn_depth: meta.spawn_depth,
            });
        }
    }
    out.sort_by(|a, b| a.agent.cmp(&b.agent));
    out
}

/// One [`TokenRecord`] per (session, hour, model).
pub fn token_records(answers: &[Answer], state: &ScanState, home: &Path) -> Vec<TokenRecord> {
    // The earliest answer of a group names its folder, so the answers are taken
    // in time order.
    let mut ordered: Vec<&Answer> = answers.iter().collect();
    ordered.sort_by_key(|answer| answer.at);

    let mut groups: BTreeMap<GroupKey, TokenGroup> = BTreeMap::new();
    let mut projects: HashMap<String, ProjectKey> = HashMap::new();
    for answer in ordered {
        let cwd = answer_cwd(answer, state);
        let key = (
            hour_start(answer.at),
            answer.session.clone(),
            answer.model.clone(),
        );
        let group = groups.entry(key).or_insert_with(|| {
            let project = projects
                .entry(cwd.clone())
                .or_insert_with(|| project::identify(Path::new(&cwd), home))
                .clone();
            TokenGroup {
                cwd: cwd.clone(),
                project,
                input: 0,
                output: 0,
                cache_write: 0,
                cache_read: 0,
                answers: 0,
                agent_answers: 0,
            }
        });
        group.input += answer.input;
        group.output += answer.output;
        group.cache_write += answer.cache_write;
        group.cache_read += answer.cache_read;
        group.answers += 1;
        if answer.agent.is_some() {
            group.agent_answers += 1;
        }
    }
    groups
        .into_iter()
        .map(|((t, session, model), group)| TokenRecord {
            t,
            session,
            cwd: group.cwd,
            project: group.project,
            model,
            input: group.input,
            out: group.output,
            cw: group.cache_write,
            cr: group.cache_read,
            answers: group.answers,
            agent_answers: group.agent_answers,
        })
        .collect()
}

// ---- the implementation ----

/// The state file of one project folder: `<folder name>.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateFile {
    version: u32,
    files: BTreeMap<String, FileState>,
    sessions: BTreeMap<String, SessionSeen>,
}

/// A transcript that may have new bytes.
struct Candidate {
    key: String,
    session: String,
    agent: Option<String>,
    folder: String,
    path: PathBuf,
    len: u64,
    mtime: DateTime<Utc>,
}

/// What reading one transcript did.
struct Outcome {
    /// The bytes of whole lines that were read in this scan.
    consumed: u64,
    /// The budget was reached after a whole line.
    budget_hit: bool,
    /// No whole line is left in the file.
    at_eof: bool,
}

/// What the path of a transcript says about it; a line of the file never
/// changes these.
struct Reading<'a> {
    session: &'a str,
    agent: Option<&'a str>,
    folder: &'a str,
    /// The file was read again from 0 because it shrank.
    reread: bool,
    /// `last_answer` when this reading began: the reread guard compares against
    /// it, while the file state's own copy moves up as answers are counted.
    last_answer: Option<DateTime<Utc>>,
}

/// One line at a time: a line of a session with its own order of answers and
/// user turns.
enum Event {
    Answer(usize),
    User(DateTime<Utc>),
}

/// The part of a scan that belongs to one file.
struct FileScan {
    state: FileState,
    events: Vec<Event>,
    /// The message ids of this scan, with the index of their answer.
    ids: HashMap<String, usize>,
    unreadable: usize,
}

/// The scan as it runs.
struct ScanRun<'a> {
    state: &'a mut ScanState,
    budget: u64,
    used: u64,
    answers: Vec<Answer>,
    user_turns: Vec<UserTurn>,
    warnings: Vec<String>,
}

impl ScanRun<'_> {
    fn read(&mut self, candidate: &Candidate) -> Result<Outcome, TranscriptError> {
        let mut stored = match self.state.files.get(&candidate.key) {
            Some(file) => file.clone(),
            None => FileState {
                offset: 0,
                grew: candidate.mtime,
                recent: Vec::new(),
                last_answer: None,
            },
        };
        let mut file = match File::open(&candidate.path) {
            Ok(file) => file,
            // Claude Code removed the transcript between listing and opening it.
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(Outcome {
                    consumed: 0,
                    budget_hit: false,
                    at_eof: true,
                })
            }
            Err(source) => {
                return Err(TranscriptError::Io {
                    path: candidate.path.clone(),
                    source,
                })
            }
        };
        let mut reread = false;
        let shorter = candidate.len < stored.offset;
        if shorter {
            if stored.recent.is_empty() {
                // It had stopped growing, so it cannot be counted twice: leave it.
                return self.not_counted(candidate, &mut file, stored);
            }
            self.warnings.push(format!(
                "{} shrank from {} to {} bytes; reading it again from the start",
                candidate.key, stored.offset, candidate.len
            ));
            stored.offset = 0;
            reread = true;
        }
        let mut start = stored.offset;
        if !shorter && start > 0 {
            // The byte before a stored offset is the '\n' that ended the last line
            // read. If it is not, the file was replaced by one whose lines fall
            // elsewhere and the offset points into the middle of a line.
            let last = byte_before(&mut file, start, &candidate.path)?;
            if last != Some(b'\n') {
                if stored.recent.is_empty() {
                    return self.not_counted(candidate, &mut file, stored);
                }
                self.warnings.push(format!(
                    "{} was replaced after it had been read; reading it again from the start",
                    candidate.key
                ));
                stored.offset = 0;
                start = 0;
                reread = true;
            }
        }
        if let Err(source) = file.seek(SeekFrom::Start(start)) {
            return Err(TranscriptError::Io {
                path: candidate.path.clone(),
                source,
            });
        }
        let reading = Reading {
            session: &candidate.session,
            agent: candidate.agent.as_deref(),
            folder: &candidate.folder,
            reread,
            last_answer: stored.last_answer,
        };
        let mut file_scan = FileScan {
            state: stored,
            events: Vec::new(),
            ids: HashMap::new(),
            unreadable: 0,
        };
        let mut pending: Vec<u8> = Vec::new();
        // The file offset of pending[0].
        let mut pending_start = start;
        // The start of the next line in pending; everything before it was read.
        let mut line_from = 0usize;
        // How much of pending was searched for '\n'; nothing before it holds one.
        let mut searched = 0usize;
        let mut chunk = vec![0u8; CHUNK_BYTES];
        let mut offset = start;
        let mut budget_hit = false;
        let mut at_eof = false;
        'lines: loop {
            while let Some(found) = pending[searched..].iter().position(|byte| *byte == b'\n') {
                let end = searched + found;
                let mut line = &pending[line_from..end];
                if pending_start + line_from as u64 == 0 && line.starts_with(&BYTE_ORDER_MARK) {
                    line = &line[BYTE_ORDER_MARK.len()..];
                }
                if line.last() == Some(&b'\r') {
                    line = &line[..line.len() - 1];
                }
                offset = pending_start + end as u64 + 1;
                self.line(&reading, line, &mut file_scan);
                line_from = end + 1;
                searched = end + 1;
                if self.used.saturating_add(offset - start) >= self.budget {
                    budget_hit = true;
                    break 'lines;
                }
            }
            // The bytes not searched yet hold no '\n'; the next chunk is searched from there.
            searched = pending.len();
            // The consumed part is removed once per chunk, not once per line.
            if line_from > 0 {
                pending.drain(..line_from);
                pending_start += line_from as u64;
                searched -= line_from;
                line_from = 0;
            }
            let read = match file.read(&mut chunk) {
                Ok(0) => {
                    at_eof = true;
                    break;
                }
                Ok(read) => read,
                Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    return Err(TranscriptError::Io {
                        path: candidate.path.clone(),
                        source,
                    })
                }
            };
            pending.extend_from_slice(&chunk[..read]);
        }
        // The bytes after the last '\n' are not read; the offset stays before them.
        let consumed = offset - start;
        let mut file_state = file_scan.state;
        file_state.offset = offset;
        if consumed > 0 {
            file_state.grew = candidate.mtime;
        }
        if file_scan.unreadable > 0 {
            self.warnings.push(format!(
                "{}: {} lines could not be read",
                candidate.key, file_scan.unreadable
            ));
        }
        self.apply_events(&candidate.session, file_scan.events);
        if self.state.files.get(&candidate.key) != Some(&file_state) {
            self.state.files.insert(candidate.key.clone(), file_state);
            self.state.touch(&candidate.folder);
        }
        Ok(Outcome {
            consumed,
            budget_hit,
            at_eof: at_eof || offset >= candidate.len,
        })
    }

    /// A transcript that had stopped growing was replaced: warn once, move the
    /// offset to the end of its last whole line, and count nothing.
    fn not_counted(
        &mut self,
        candidate: &Candidate,
        file: &mut File,
        mut stored: FileState,
    ) -> Result<Outcome, TranscriptError> {
        self.warnings.push(format!(
            "{} was replaced after it had been read; its new content is not counted",
            candidate.key
        ));
        stored.offset = last_whole_line_end(file, &candidate.path)?;
        self.state.files.insert(candidate.key.clone(), stored);
        self.state.touch(&candidate.folder);
        Ok(Outcome {
            consumed: 0,
            budget_hit: false,
            at_eof: true,
        })
    }

    /// One line: a narrow look at it, nothing of its text kept.
    fn line(&mut self, reading: &Reading, bytes: &[u8], file: &mut FileScan) {
        if bytes.iter().all(|byte| byte.is_ascii_whitespace()) {
            // A blank line, or one of only spaces and tabs, says nothing.
            return;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            file.unreadable += 1;
            return;
        };
        // The type decides the shape; every other key is looked at only on an
        // answer or a user line, whatever shape it has elsewhere.
        let Ok(head) = serde_json::from_str::<LineType>(text) else {
            file.unreadable += 1;
            return;
        };
        if head.kind != "assistant" && head.kind != "user" {
            // attachment, system, api-request, queue-operation, …: skipped quietly.
            return;
        }
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            file.unreadable += 1;
            return;
        };
        if head.kind == "assistant" {
            self.assistant(reading, &line, file);
        } else {
            self.user(reading, &line, file);
        }
    }

    fn assistant(&mut self, reading: &Reading, line: &Line, file: &mut FileScan) {
        let (Some(at), Some(cwd), Some(message)) =
            (line.timestamp, line.cwd.as_deref(), line.message.as_ref())
        else {
            file.unreadable += 1;
            return;
        };
        let (Some(id), Some(model), Some(usage)) = (
            message.id.as_deref(),
            message.model.as_deref(),
            message.usage.as_ref(),
        ) else {
            file.unreadable += 1;
            return;
        };
        let (Some(input), Some(output)) = (usage.input_tokens, usage.output_tokens) else {
            file.unreadable += 1;
            return;
        };
        if model == SYNTHETIC {
            return;
        }
        let thinking = message.content.as_ref().is_some_and(Content::thinking);
        if let Some(&index) = file.ids.get(id) {
            // A further line of an answer this scan already counted.
            if thinking {
                if let Some(answer) = self.answers.get_mut(index) {
                    answer.thinking = true;
                }
            }
            return;
        }
        if file.state.recent.iter().any(|known| known == id) {
            // Counted before this scan, or by the scan before a reread.
            return;
        }
        // On a reread, an answer from before the file was read again is not
        // counted twice even when its id has left `recent`.
        if reading.reread && reading.last_answer.is_some_and(|last| at <= last) {
            return;
        }
        let cwd = project::canonical_cwd(cwd);
        let index = self.answers.len();
        file.ids.insert(id.to_string(), index);
        if file.state.recent.len() >= RECENT_IDS {
            file.state.recent.remove(0);
        }
        file.state.recent.push(id.to_string());
        file.state.last_answer = Some(file.state.last_answer.map_or(at, |last| last.max(at)));
        self.answers.push(Answer {
            session: reading.session.to_string(),
            agent: reading.agent.map(str::to_string),
            message_id: id.to_string(),
            at,
            cwd: cwd.clone(),
            model: model.to_string(),
            input,
            output,
            cache_write: usage.cache_creation_input_tokens.unwrap_or(0),
            cache_read: usage.cache_read_input_tokens.unwrap_or(0),
            thinking,
        });
        if reading.agent.is_none() {
            self.observe(reading, at, Some(&cwd), Some(model));
            file.events.push(Event::Answer(index));
        }
    }

    fn user(&mut self, reading: &Reading, line: &Line, file: &mut FileScan) {
        let Some(at) = line.timestamp else {
            file.unreadable += 1;
            return;
        };
        if reading.agent.is_some() {
            // A user turn comes from a main transcript only.
            return;
        }
        let cwd = line.cwd.as_deref().map(project::canonical_cwd);
        let real = !line.is_meta.unwrap_or(false)
            && match &line.origin {
                None => true,
                Some(origin) => origin.kind.as_deref() == Some("human"),
            }
            && line
                .message
                .as_ref()
                .and_then(|message| message.content.as_ref())
                .is_some_and(Content::is_real_user_message);
        let suppressed = reading.reread
            && self
                .state
                .sessions
                .get(reading.session)
                .is_some_and(|seen| seen.last_user_turn.is_some_and(|last| at <= last));
        if suppressed {
            return;
        }
        self.observe(reading, at, cwd.as_deref(), None);
        if real {
            self.user_turns.push(UserTurn {
                session: reading.session.to_string(),
                at,
            });
            file.events.push(Event::User(at));
        }
    }

    /// Note a line of a main transcript in its session.
    fn observe(
        &mut self,
        reading: &Reading,
        at: DateTime<Utc>,
        cwd: Option<&str>,
        model: Option<&str>,
    ) {
        let seen = self
            .state
            .sessions
            .entry(reading.session.to_string())
            .or_insert_with(|| SessionSeen {
                folder: reading.folder.to_string(),
                cwd: cwd.unwrap_or_default().to_string(),
                first: at,
                last: at,
                last_model: model.map(str::to_string),
                last_user_turn: None,
                thinking_since_user: 0,
                recorded: None,
            });
        if at >= seen.last {
            if let Some(cwd) = cwd {
                seen.cwd = cwd.to_string();
            }
            if let Some(model) = model {
                seen.last_model = Some(model.to_string());
            }
        }
        if at < seen.first {
            seen.first = at;
        }
        if at > seen.last {
            seen.last = at;
        }
        self.state.touch(reading.folder);
    }

    /// Apply a file's answers and user turns to its session, in the order they
    /// were read: a user turn resets the thinking count.
    fn apply_events(&mut self, session: &str, events: Vec<Event>) {
        let answers = &self.answers;
        let Some(seen) = self.state.sessions.get_mut(session) else {
            return;
        };
        for event in events {
            match event {
                Event::Answer(index) => {
                    if answers.get(index).is_some_and(|answer| answer.thinking) {
                        seen.thinking_since_user = seen.thinking_since_user.saturating_add(1);
                    }
                }
                Event::User(at) => {
                    seen.last_user_turn = Some(seen.last_user_turn.map_or(at, |last| last.max(at)));
                    seen.thinking_since_user = 0;
                }
            }
        }
    }
}

/// Every transcript that may be read: `<folder>/<session>.jsonl` and
/// `<folder>/<session>/subagents/agent-<id>.jsonl`. Nothing else under
/// `projects` is looked at.
fn candidates(dir: &Path) -> Result<Vec<Candidate>, TranscriptError> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(source) => {
            return Err(TranscriptError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| TranscriptError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.path().is_dir() {
            folders.push((name.to_string(), entry.path()));
        }
    }
    folders.sort();
    for (folder, path) in folders {
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(TranscriptError::Io {
                    path: path.clone(),
                    source,
                })
            }
        };
        let mut inner: Vec<(String, PathBuf)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| TranscriptError::Io {
                path: path.clone(),
                source,
            })?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            inner.push((name.to_string(), entry.path()));
        }
        inner.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entry_path) in inner {
            if !entry_path.is_dir() {
                let Some(session) = name.strip_suffix(".jsonl") else {
                    continue;
                };
                if session.is_empty() {
                    continue;
                }
                push_candidate(&mut out, &folder, &entry_path, session, None, &name)?;
                continue;
            }
            // <folder>/<session>/subagents/agent-<id>.jsonl
            let subagents = entry_path.join("subagents");
            let entries = match fs::read_dir(&subagents) {
                Ok(entries) => entries,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(TranscriptError::Io {
                        path: subagents.clone(),
                        source,
                    })
                }
            };
            for entry in entries {
                let entry = entry.map_err(|source| TranscriptError::Io {
                    path: subagents.clone(),
                    source,
                })?;
                let file_name = entry.file_name();
                let Some(agent_name) = file_name.to_str() else {
                    continue;
                };
                let Some(agent) = agent_id(agent_name) else {
                    continue;
                };
                if entry.path().is_dir() {
                    continue;
                }
                push_candidate(
                    &mut out,
                    &folder,
                    &entry.path(),
                    &name,
                    Some(agent),
                    agent_name,
                )?;
            }
        }
    }
    Ok(out)
}

fn push_candidate(
    out: &mut Vec<Candidate>,
    folder: &str,
    path: &Path,
    session: &str,
    agent: Option<&str>,
    file_name: &str,
) -> Result<(), TranscriptError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(TranscriptError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let modified = metadata.modified().map_err(|source| TranscriptError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let key = match agent {
        Some(_) => format!("{folder}/{session}/subagents/{file_name}"),
        None => format!("{folder}/{file_name}"),
    };
    out.push(Candidate {
        key,
        session: session.to_string(),
        agent: agent.map(str::to_string),
        folder: folder.to_string(),
        path: path.to_path_buf(),
        len: metadata.len(),
        mtime: DateTime::<Utc>::from(modified),
    });
    Ok(())
}

/// `<id>` of `agent-<id>.jsonl`.
fn agent_id(file_name: &str) -> Option<&str> {
    let id = file_name.strip_prefix("agent-")?.strip_suffix(".jsonl")?;
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// `<id>` of `agent-<id>.meta.json`.
fn agent_meta_id(file_name: &str) -> Option<&str> {
    let id = file_name
        .strip_prefix("agent-")?
        .strip_suffix(".meta.json")?;
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// The project folder a `files` key belongs to.
fn folder_of(key: &str) -> &str {
    match key.split_once('/') {
        Some((folder, _)) => folder,
        None => key,
    }
}

/// The byte at `offset - 1` of the open file, or `None` when the file ends
/// before it.
fn byte_before(file: &mut File, offset: u64, path: &Path) -> Result<Option<u8>, TranscriptError> {
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(offset - 1))
        .map_err(|source| TranscriptError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    loop {
        match file.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(TranscriptError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        }
    }
}

/// The offset just after the last '\n' of the open file, or 0 when it has none.
fn last_whole_line_end(file: &mut File, path: &Path) -> Result<u64, TranscriptError> {
    let mut end = file
        .seek(SeekFrom::End(0))
        .map_err(|source| TranscriptError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut buffer = vec![0u8; CHUNK_BYTES];
    while end > 0 {
        let take = end.min(CHUNK_BYTES as u64) as usize;
        let from = end - take as u64;
        file.seek(SeekFrom::Start(from))
            .map_err(|source| TranscriptError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut filled = 0usize;
        while filled < take {
            match file.read(&mut buffer[filled..take]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    return Err(TranscriptError::Io {
                        path: path.to_path_buf(),
                        source,
                    })
                }
            }
        }
        if filled < take {
            // The file shrank while it was read: start again from its new end.
            end = file
                .seek(SeekFrom::End(0))
                .map_err(|source| TranscriptError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
            continue;
        }
        if let Some(index) = buffer[..take].iter().rposition(|byte| *byte == b'\n') {
            return Ok(from + index as u64 + 1);
        }
        end = from;
    }
    Ok(0)
}

/// The start of the hour `at` falls in, in UTC.
fn hour_start(at: DateTime<Utc>) -> DateTime<Utc> {
    at - Duration::minutes(i64::from(at.minute()))
        - Duration::seconds(i64::from(at.second()))
        - Duration::nanoseconds(i64::from(at.nanosecond()))
}

/// A group of [`TokenRecord`], keyed by session, hour and model.
type GroupKey = (DateTime<Utc>, String, String);

struct TokenGroup {
    /// The cwd of the group's earliest answer.
    cwd: String,
    project: ProjectKey,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    answers: u32,
    agent_answers: u32,
}

/// The folder an answer's tokens belong to: its own for a main transcript, its
/// session's for an agent whose parent session is known.
fn answer_cwd(answer: &Answer, state: &ScanState) -> String {
    if answer.agent.is_some() {
        if let Some(seen) = state.sessions.get(&answer.session) {
            if !seen.cwd.is_empty() {
                return seen.cwd.clone();
            }
        }
    }
    answer.cwd.clone()
}

/// The `type` of a transcript line, the only key read before the rest: every
/// other line type has its own shape.
#[derive(Deserialize)]
struct LineType {
    #[serde(rename = "type")]
    kind: String,
}

/// A transcript line, as far as seatswap looks at it: never a message text.
/// Only an `assistant` or a `user` line is read into it.
#[derive(Deserialize)]
struct Line {
    timestamp: Option<DateTime<Utc>>,
    cwd: Option<String>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    origin: Option<Origin>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Origin {
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    content: Option<Content>,
    usage: Option<Usage>,
}

/// The four token counts. The cache counts are absent when no cache was used;
/// a cache count that is present but not a number makes the line unreadable.
#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// `message.content` as far as seatswap looks at it: whether it is a string,
/// and the `type` of each block. Text of it is not kept.
struct Content {
    is_string: bool,
    blocks: Vec<Option<String>>,
}

impl Content {
    /// A real user message: a string, or blocks that include a `text` block.
    fn is_real_user_message(&self) -> bool {
        self.is_string
            || self
                .blocks
                .iter()
                .any(|kind| kind.as_deref() == Some("text"))
    }

    fn thinking(&self) -> bool {
        self.blocks
            .iter()
            .any(|kind| matches!(kind.as_deref(), Some(kind) if THINKING_BLOCKS.contains(&kind)))
    }
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Content, D::Error> {
        struct ContentVisitor;

        impl<'de> Visitor<'de> for ContentVisitor {
            type Value = Content;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string or a list of content blocks")
            }

            fn visit_str<E>(self, _text: &str) -> Result<Content, E> {
                Ok(Content {
                    is_string: true,
                    blocks: Vec::new(),
                })
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Content, A::Error> {
                let mut blocks = Vec::new();
                while let Some(block) = seq.next_element::<Block>()? {
                    blocks.push(block.kind);
                }
                Ok(Content {
                    is_string: false,
                    blocks,
                })
            }
        }

        deserializer.deserialize_any(ContentVisitor)
    }
}

/// One `agent-<id>.meta.json`: `agentType` and `description` are optional,
/// `spawnDepth` is in every file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentMetaFile {
    agent_type: Option<String>,
    description: Option<String>,
    /// Absent while the agent runs; `false` was never seen.
    finished: Option<bool>,
    spawn_depth: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    const SESSION: &str = "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34";
    const SESSION_B: &str = "2b7d9c1e-5a4f-4c3b-9e8d-7f6a5b4c3d2e";
    const FOLDER: &str = "-home-sam-projects-atlas";
    const FOLDER_B: &str = "-home-sam-projects-ledger";
    const KEY: &str = "-home-sam-projects-atlas/7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34.jsonl";
    const AT: &str = "2026-11-12T09:41:07.412Z";
    const ENGINE: &str = "/home/sam/projects/atlas/engine";
    const OPUS: &str = "claude-opus-5-5";
    const SONNET: &str = "claude-sonnet-5-2";
    /// input, output, cache write, cache read of the README §5 answer.
    const USAGE: (u64, u64, u64, u64) = (2, 166, 26173, 28126);
    const BUDGET: u64 = 64 * 1024 * 1024;

    fn fixture() -> (tempfile::TempDir, ClaudePaths, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let claude = ClaudePaths::for_config_dir(tmp.path(), tmp.path());
        let app = AppPaths::at(&tmp.path().join("app"));
        (tmp, claude, app)
    }

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn blocks(kinds: &[&str]) -> Vec<serde_json::Value> {
        kinds
            .iter()
            .map(|kind| match *kind {
                "thinking" => {
                    serde_json::json!({"type": "thinking", "thinking": "", "signature": "EqQ"})
                }
                "redacted_thinking" => {
                    serde_json::json!({"type": "redacted_thinking", "data": "EqQ"})
                }
                "text" => serde_json::json!({"type": "text", "text": "fix the failing tests"}),
                other => serde_json::json!({"type": other}),
            })
            .collect()
    }

    fn assistant_line(
        id: &str,
        at: &str,
        cwd: &str,
        model: &str,
        kinds: &[&str],
        usage: (u64, u64, u64, u64),
    ) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": "0b6f4c1a-2d3e-4f50-8a9b-0c1d2e3f4a5b",
            "parentUuid": "91c2a7b3-4c5d-4e6f-8a90-1b2c3d4e5f60",
            "sessionId": SESSION,
            "timestamp": at,
            "cwd": cwd,
            "gitBranch": "main",
            "isSidechain": false,
            "requestId": "req_011CZk3",
            "version": "2.1.322",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": blocks(kinds),
                "stop_reason": "tool_use",
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

    fn answer(id: &str, at: &str) -> String {
        assistant_line(id, at, ENGINE, OPUS, &["thinking"], USAGE)
    }

    /// An answer whose `requestId` is padded so that the line is `len` bytes.
    fn answer_of_len(id: &str, at: &str, len: usize) -> String {
        let base = serde_json::json!({
            "type": "assistant",
            "timestamp": at,
            "cwd": "/home/sam/atlas",
            "requestId": "",
            "message": {
                "id": id,
                "model": OPUS,
                "content": [],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }
        })
        .to_string();
        assert!(
            base.len() <= len,
            "an answer line is at least {} bytes, not {len}",
            base.len()
        );
        let pad = "x".repeat(len - base.len());
        base.replace("\"requestId\":\"\"", &format!("\"requestId\":\"{pad}\""))
    }

    fn user_string(text: &str, at: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": at,
            "sessionId": SESSION,
            "isSidechain": false,
            "message": {"role": "user", "content": text}
        })
        .to_string()
    }

    fn user_tool_result(at: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": at,
            "sessionId": SESSION,
            "isSidechain": false,
            "toolUseResult": {},
            "sourceToolAssistantUUID": "0b6f4c1a-2d3e-4f50-8a9b-0c1d2e3f4a5b",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_01", "content": "ok"}
            ]}
        })
        .to_string()
    }

    fn user_meta(at: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": at,
            "sessionId": SESSION,
            "isMeta": true,
            "message": {"role": "user", "content": [{"type": "text", "text": "..."}]}
        })
        .to_string()
    }

    fn user_with_origin(kind: &str, at: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": at,
            "sessionId": SESSION,
            "origin": {"kind": kind},
            "message": {"role": "user", "content": "fix the failing tests"}
        })
        .to_string()
    }

    fn meta_json(finished: Option<bool>) -> String {
        let mut value = serde_json::json!({
            "agentType": "general-purpose",
            "description": "fix the failing tests",
            "isBuiltIn": true,
            "spawnDepth": 1,
            "toolUseId": "toolu_01Nkn4"
        });
        if let Some(finished) = finished {
            value["finished"] = serde_json::json!(finished);
        }
        value.to_string()
    }

    fn folder(claude: &ClaudePaths, name: &str) -> PathBuf {
        claude.projects_dir().join(name)
    }

    fn write_lines(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(&path, text).unwrap();
        path
    }

    fn append(path: &Path, line: &str) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(format!("{line}\n").as_bytes()).unwrap();
    }

    fn scan_run(claude: &ClaudePaths) -> (ScanState, Scan) {
        let mut state = ScanState::default();
        let scan = scan_commit(claude, &mut state, BUDGET);
        (state, scan)
    }

    /// One scan and its commit, as a pass that wrote its token lines does.
    fn scan_commit(claude: &ClaudePaths, state: &mut ScanState, budget: u64) -> Scan {
        let mut scan = scan(claude, state, budget).unwrap();
        scan.commit(state);
        scan
    }

    #[test]
    fn an_answer_over_two_lines_counts_once() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                assistant_line("msg_01QmB7xT", AT, ENGINE, OPUS, &["thinking"], USAGE),
                assistant_line("msg_01QmB7xT", AT, ENGINE, OPUS, &["tool_use"], USAGE),
            ],
        );

        let (state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 1, "{:?}", scan.answers);
        let answer = &scan.answers[0];
        assert_eq!(answer.session, SESSION);
        assert_eq!(answer.agent, None);
        assert_eq!(answer.message_id, "msg_01QmB7xT");
        assert_eq!(answer.at, at(AT));
        assert_eq!(answer.cwd, ENGINE);
        assert_eq!(answer.model, OPUS);
        assert_eq!(answer.input, 2);
        assert_eq!(answer.output, 166);
        assert_eq!(answer.cache_write, 26173);
        assert_eq!(answer.cache_read, 28126);
        assert!(answer.thinking);
        assert_eq!(
            state.files[KEY].offset,
            fs::metadata(folder(&claude, FOLDER).join(format!("{SESSION}.jsonl")))
                .unwrap()
                .len()
        );
    }

    #[test]
    fn the_same_id_far_apart_counts_once() {
        let (_tmp, claude, _app) = fixture();
        let mut lines = vec![
            assistant_line("msg_01QmB7xT", AT, ENGINE, OPUS, &["thinking"], USAGE),
            assistant_line("msg_01QmB7xT", AT, ENGINE, OPUS, &["tool_use"], USAGE),
        ];
        for minute in 0..10 {
            lines.push(user_tool_result(&format!(
                "2026-11-12T09:4{minute}:30.000Z"
            )));
        }
        lines.push(assistant_line(
            "msg_01QmB7xT",
            "2026-11-12T09:50:00.000Z",
            ENGINE,
            OPUS,
            &["text"],
            USAGE,
        ));
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &lines,
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 1, "{:?}", scan.answers);
        assert_eq!(scan.answers[0].message_id, "msg_01QmB7xT");
        assert_eq!(scan.answers[0].at, at(AT), "the first line's time");
    }

    #[test]
    fn a_second_scan_reads_only_what_is_new() {
        let (_tmp, claude, _app) = fixture();
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.answers.len(), 1);

        append(
            &path,
            &assistant_line(
                "msg_b",
                "2026-11-12T09:42:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
        );
        let second = scan_commit(&claude, &mut state, BUDGET);

        assert_eq!(second.answers.len(), 1, "{:?}", second.answers);
        assert_eq!(second.answers[0].message_id, "msg_b");
        assert!(second.complete);
    }

    #[test]
    fn a_half_written_last_line_waits() {
        let (_tmp, claude, _app) = fixture();
        let first_line = answer("msg_a", AT);
        let second_line = assistant_line(
            "msg_b",
            "2026-11-12T09:42:00.000Z",
            ENGINE,
            OPUS,
            &["text"],
            USAGE,
        );
        let split = second_line.len() / 2;
        let path = folder(&claude, FOLDER).join(format!("{SESSION}.jsonl"));
        fs::create_dir_all(folder(&claude, FOLDER)).unwrap();
        fs::write(&path, format!("{first_line}\n{}", &second_line[..split])).unwrap();

        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.answers.len(), 1, "{:?}", first.answers);
        assert_eq!(first.answers[0].message_id, "msg_a");

        fs::write(&path, format!("{first_line}\n{second_line}\n")).unwrap();
        let second = scan_commit(&claude, &mut state, BUDGET);

        assert_eq!(second.answers.len(), 1, "{:?}", second.answers);
        assert_eq!(second.answers[0].message_id, "msg_b");
    }

    #[test]
    fn a_shrunk_file_restarts_without_double_counting() {
        let (_tmp, claude, _app) = fixture();
        let first_line = answer("msg_a", AT);
        let second_line = assistant_line(
            "msg_b",
            "2026-11-12T09:42:00.000Z",
            ENGINE,
            OPUS,
            &["text"],
            USAGE,
        );
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[first_line.clone(), second_line.clone()],
        );
        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.answers.len(), 2);

        let bytes = fs::read(&path).unwrap();
        let cut = first_line.len() + 1 + second_line.len() / 2;
        fs::write(&path, &bytes[..cut]).unwrap();
        let second = scan_commit(&claude, &mut state, BUDGET);

        assert!(second.answers.is_empty(), "{:?}", second.answers);
        assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
        assert_eq!(
            state.files[KEY].offset,
            first_line.len() as u64 + 1,
            "the offset stops after the last whole line of the reread"
        );
    }

    #[test]
    fn tool_results_and_meta_lines_are_not_user_turns() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                user_string("fix the failing tests", "2026-11-12T09:40:58.101Z"),
                user_tool_result("2026-11-12T09:41:09.730Z"),
                user_meta("2026-11-12T09:40:57.000Z"),
            ],
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.user_turns.len(), 1, "{:?}", scan.user_turns);
        assert_eq!(scan.user_turns[0].at, at("2026-11-12T09:40:58.101Z"));
        assert_eq!(scan.user_turns[0].session, SESSION);
        assert!(scan.answers.is_empty());
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    }

    #[test]
    fn notifications_and_messages_from_other_sessions_are_not_user_turns() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                user_with_origin("task-notification", "2026-11-12T09:40:58.101Z"),
                user_with_origin("peer", "2026-11-12T09:40:59.101Z"),
                user_with_origin("human", "2026-11-12T09:41:00.101Z"),
            ],
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.user_turns.len(), 1, "{:?}", scan.user_turns);
        assert_eq!(scan.user_turns[0].at, at("2026-11-12T09:41:00.101Z"));
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    }

    #[test]
    fn other_line_types_are_skipped_quietly() {
        let (_tmp, claude, _app) = fixture();
        let lines: Vec<String> = ["attachment", "system", "api-request", "queue-operation"]
            .iter()
            .map(|kind| {
                serde_json::json!({"type": kind, "timestamp": AT, "sessionId": SESSION}).to_string()
            })
            .collect();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &lines,
        );

        let (_state, scan) = scan_run(&claude);

        assert!(scan.answers.is_empty());
        assert!(scan.user_turns.is_empty());
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
        assert!(scan.complete);
    }

    #[test]
    fn thinking_since_the_user_is_counted_and_reset() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                user_string("fix the failing tests", "2026-11-12T09:40:58.101Z"),
                assistant_line(
                    "msg_a",
                    "2026-11-12T09:41:07.412Z",
                    ENGINE,
                    OPUS,
                    &["thinking"],
                    USAGE,
                ),
                assistant_line(
                    "msg_b",
                    "2026-11-12T09:41:20.000Z",
                    ENGINE,
                    OPUS,
                    &["tool_use"],
                    USAGE,
                ),
                user_string("and the linter", "2026-11-12T09:42:00.000Z"),
                assistant_line(
                    "msg_c",
                    "2026-11-12T09:42:30.000Z",
                    ENGINE,
                    OPUS,
                    &["redacted_thinking"],
                    USAGE,
                ),
            ],
        );

        let (state, scan) = scan_run(&claude);

        assert_eq!(scan.user_turns.len(), 2, "{:?}", scan.user_turns);
        let seen = &state.sessions[SESSION];
        assert_eq!(seen.thinking_since_user, 1);
        assert_eq!(seen.last_user_turn, Some(at("2026-11-12T09:42:00.000Z")));
        assert_eq!(seen.folder, FOLDER);
        assert_eq!(seen.cwd, ENGINE);
        assert_eq!(seen.last_model.as_deref(), Some(OPUS));
        assert_eq!(seen.first, at("2026-11-12T09:40:58.101Z"));
        assert_eq!(seen.last, at("2026-11-12T09:42:30.000Z"));
    }

    #[test]
    fn synthetic_answers_are_skipped() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[assistant_line(
                "msg_synthetic",
                AT,
                ENGINE,
                SYNTHETIC,
                &["text"],
                USAGE,
            )],
        );

        let (_state, scan) = scan_run(&claude);

        assert!(scan.answers.is_empty());
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    }

    #[test]
    fn an_agent_answer_belongs_to_its_session() {
        let (_tmp, claude, _app) = fixture();
        let dir = folder(&claude, FOLDER).join(SESSION).join("subagents");
        write_lines(&dir, "agent-a875.jsonl", &[answer("msg_agent", AT)]);

        let (state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 1, "{:?}", scan.answers);
        assert_eq!(scan.answers[0].session, SESSION);
        assert_eq!(scan.answers[0].agent.as_deref(), Some("a875"));
        assert!(
            !state.sessions.contains_key(SESSION),
            "an agent's line never makes a session"
        );
    }

    #[test]
    fn agents_of_reads_the_meta_files() {
        let (_tmp, claude, _app) = fixture();
        let dir = folder(&claude, FOLDER).join(SESSION).join("subagents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agent-a875.meta.json"), meta_json(Some(true))).unwrap();

        let metas = agents_of(&claude, SESSION);

        assert_eq!(
            metas,
            vec![AgentMeta {
                session: SESSION.to_string(),
                agent: "a875".to_string(),
                agent_type: Some("general-purpose".to_string()),
                description: Some("fix the failing tests".to_string()),
                finished: true,
                spawn_depth: 1,
            }]
        );
    }

    #[test]
    fn an_agent_without_finished_is_still_running() {
        let (_tmp, claude, _app) = fixture();
        let dir = folder(&claude, FOLDER).join(SESSION).join("subagents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agent-a875.meta.json"), meta_json(None)).unwrap();

        let metas = agents_of(&claude, SESSION);

        assert_eq!(metas.len(), 1);
        assert!(!metas[0].finished);
    }

    #[test]
    fn the_budget_stops_and_the_next_scan_continues() {
        let (_tmp, claude, _app) = fixture();
        let mut ids = Vec::new();
        for session in [SESSION, SESSION_B] {
            let lines: Vec<String> = (0..4)
                .map(|n| {
                    let id = format!("msg_{}", &session[..8]);
                    let id = format!("{id}_{n}");
                    ids.push(id.clone());
                    answer_of_len(&id, AT, 249)
                })
                .collect();
            let path = write_lines(
                &folder(&claude, FOLDER),
                &format!("{session}.jsonl"),
                &lines,
            );
            assert_eq!(fs::metadata(&path).unwrap().len(), 1000);
        }

        let mut state = ScanState::default();
        let first = scan_commit(&claude, &mut state, 1200);
        assert!(!first.complete, "the budget stops the first scan");
        let second = scan_commit(&claude, &mut state, 1200);
        assert!(second.complete, "the second scan reads the rest");

        let mut seen: Vec<String> = first
            .answers
            .iter()
            .chain(second.answers.iter())
            .map(|answer| answer.message_id.clone())
            .collect();
        seen.sort();
        let mut expected = ids;
        expected.sort();
        assert_eq!(seen, expected, "every answer exactly once");
    }

    #[test]
    fn only_the_folders_that_changed_are_saved() {
        let (_tmp, claude, app) = fixture();
        let atlas_path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        write_lines(
            &folder(&claude, FOLDER_B),
            &format!("{SESSION_B}.jsonl"),
            &[assistant_line(
                "msg_b",
                "2026-11-12T10:00:00.000Z",
                "/home/sam/projects/ledger",
                OPUS,
                &["text"],
                USAGE,
            )],
        );
        let mut state = ScanState::default();
        scan_commit(&claude, &mut state, BUDGET);
        assert_eq!(state.save(&claude, &app, Utc::now()).unwrap(), 2);

        let state_dir = app.transcripts_state_dir();
        let atlas_file = state_dir.join(format!("{FOLDER}.json"));
        let other_file = state_dir.join(format!("{FOLDER_B}.json"));
        assert!(atlas_file.exists() && other_file.exists());
        let other_bytes = fs::read(&other_file).unwrap();
        let other_mtime = fs::metadata(&other_file).unwrap().modified().unwrap();

        // One file per project folder, with the short shape the plan gives.
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(&atlas_file).unwrap()).unwrap();
        assert_eq!(written["version"], 1);
        assert!(written["files"][KEY]["offset"].is_u64());
        assert!(written["files"][KEY]["grew"].is_string());
        assert!(written["files"][KEY]["recent"].is_array());
        let seen = &written["sessions"][SESSION];
        assert!(seen["folder"].is_string());
        assert!(seen["lastModel"].is_string());
        assert!(seen["lastUserTurn"].is_null());
        assert_eq!(seen["thinkingSinceUser"], 1);

        append(
            &atlas_path,
            &assistant_line(
                "msg_c",
                "2026-11-12T09:42:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
        );
        scan_commit(&claude, &mut state, BUDGET);
        assert_eq!(state.save(&claude, &app, Utc::now()).unwrap(), 1);

        assert_eq!(fs::read(&other_file).unwrap(), other_bytes);
        assert_eq!(
            fs::metadata(&other_file).unwrap().modified().unwrap(),
            other_mtime,
            "a folder that did not change is not written"
        );
        let loaded = ScanState::load(&app).unwrap();
        assert_eq!(loaded.files, state.files);
        assert_eq!(loaded.sessions, state.sessions);
    }

    #[test]
    fn a_file_that_stopped_growing_drops_its_ids() {
        let (_tmp, claude, app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        let (mut state, _scan) = scan_run(&claude);
        let offset = state.files[KEY].offset;
        assert!(!state.files[KEY].recent.is_empty());

        let wrote = state
            .save(&claude, &app, Utc::now() + Duration::hours(25))
            .unwrap();

        assert_eq!(wrote, 1);
        assert!(state.files[KEY].recent.is_empty(), "the ids are dropped");
        assert_eq!(state.files[KEY].offset, offset, "the offset does not move");
    }

    #[test]
    fn a_replaced_old_file_is_not_counted_again() {
        let (_tmp, claude, app) = fixture();
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        let (mut state, _scan) = scan_run(&claude);
        state
            .save(&claude, &app, Utc::now() + Duration::hours(25))
            .unwrap();
        assert!(state.files[KEY].recent.is_empty());

        let bytes = fs::read(&path).unwrap();
        let half = bytes.len() / 2;
        fs::write(&path, &bytes[..half]).unwrap();
        let second = scan_commit(&claude, &mut state, BUDGET);

        assert!(second.answers.is_empty(), "{:?}", second.answers);
        assert_eq!(
            second.warnings,
            vec![format!(
                "{KEY} was replaced after it had been read; its new content is not counted"
            )]
        );
        let last_newline = bytes[..half].iter().rposition(|byte| *byte == b'\n');
        assert_eq!(
            state.files[KEY].offset,
            last_newline.map_or(0, |index| index as u64 + 1),
            "the offset is the end of the last whole line within the first half"
        );
    }

    #[test]
    fn entries_of_deleted_transcripts_are_dropped() {
        let (_tmp, claude, app) = fixture();
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        let (mut state, _scan) = scan_run(&claude);
        assert!(state.files.contains_key(KEY));
        assert!(state.sessions.contains_key(SESSION));

        fs::remove_file(&path).unwrap();
        state.save(&claude, &app, Utc::now()).unwrap();

        assert!(!state.files.contains_key(KEY), "{:?}", state.files);
        assert!(
            !state.sessions.contains_key(SESSION),
            "{:?}",
            state.sessions
        );
    }

    #[test]
    fn a_windows_cwd_gets_one_spelling() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                assistant_line("msg_a", AT, r"c:\Users\sam\atlas", OPUS, &["text"], USAGE),
                assistant_line(
                    "msg_b",
                    "2026-11-12T09:42:00.000Z",
                    r"C:\Users\sam\atlas",
                    OPUS,
                    &["text"],
                    USAGE,
                ),
            ],
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 2);
        for answer in &scan.answers {
            assert_eq!(answer.cwd, r"C:\Users\sam\atlas");
        }
    }

    #[test]
    fn token_records_group_by_session_hour_and_model() {
        let (tmp, claude, _app) = fixture();
        let home = tmp.path();
        let repo = home.join("projects/atlas");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/sam/atlas.git\n",
        )
        .unwrap();
        let engine = repo.join("engine");
        fs::create_dir_all(&engine).unwrap();
        let engine = engine.to_str().unwrap().to_string();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                assistant_line(
                    "msg_1",
                    "2026-11-12T09:41:07.412Z",
                    &engine,
                    OPUS,
                    &["text"],
                    (2, 100, 26173, 28126),
                ),
                assistant_line(
                    "msg_2",
                    "2026-11-12T09:55:00.000Z",
                    &engine,
                    OPUS,
                    &["text"],
                    (2, 66, 26173, 28126),
                ),
                assistant_line(
                    "msg_3",
                    "2026-11-12T10:05:00.000Z",
                    &engine,
                    OPUS,
                    &["text"],
                    (2, 50, 26173, 28126),
                ),
            ],
        );
        write_lines(
            &folder(&claude, FOLDER).join(SESSION).join("subagents"),
            "agent-a875.jsonl",
            &[assistant_line(
                "msg_4",
                "2026-11-12T09:50:00.000Z",
                &engine,
                SONNET,
                &["text"],
                (2, 20, 26173, 28126),
            )],
        );

        let (state, scan) = scan_run(&claude);
        let records = token_records(&scan.answers, &state, home);

        assert_eq!(records.len(), 3, "{records:?}");
        let nine = at("2026-11-12T09:00:00Z");
        let ten = at("2026-11-12T10:00:00Z");
        let opus = records
            .iter()
            .find(|record| record.t == nine && record.model == OPUS)
            .unwrap();
        assert_eq!(opus.session, SESSION);
        assert_eq!(opus.cwd, engine);
        assert_eq!(opus.out, 166);
        assert_eq!(opus.answers, 2);
        assert_eq!(opus.agent_answers, 0);
        assert_eq!(
            opus.project,
            ProjectKey::Repo("github.com/sam/atlas".to_string())
        );
        let sonnet = records
            .iter()
            .find(|record| record.t == nine && record.model == SONNET)
            .unwrap();
        assert_eq!(sonnet.answers, 1);
        assert_eq!(sonnet.agent_answers, 1);
        assert_eq!(sonnet.out, 20);
        assert_eq!(sonnet.cwd, engine, "an agent uses its session's folder");
        let later = records.iter().find(|record| record.t == ten).unwrap();
        assert_eq!(later.answers, 1);
        assert_eq!(records[0].t, nine);
        assert_eq!(records[2].t, ten);
    }

    #[test]
    fn nothing_but_numbers_and_names_leaves_the_parser() {
        let (_tmp, claude, _app) = fixture();
        let line = assistant_line("msg_a", AT, ENGINE, OPUS, &["text"], USAGE)
            .replace("fix the failing tests", "SECRET-TEXT");
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[line],
        );

        let (state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 1);
        let printed = format!("{scan:?} {state:?}");
        assert!(
            !printed.contains("SECRET-TEXT"),
            "message text never leaves the parser"
        );
    }

    #[test]
    fn a_reread_file_does_not_repeat_user_turns() {
        let (_tmp, claude, _app) = fixture();
        let user = user_string("fix the failing tests", "2026-11-12T09:40:58.101Z");
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[user.clone(), answer("msg_a", AT)],
        );
        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.user_turns.len(), 1);
        assert_eq!(first.answers.len(), 1);

        fs::write(&path, format!("{user}\n")).unwrap();
        let second = scan_commit(&claude, &mut state, BUDGET);

        assert!(second.user_turns.is_empty(), "{:?}", second.user_turns);
        assert!(second.answers.is_empty(), "{:?}", second.answers);
        assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
        assert!(second.warnings[0].contains("shrank"));
    }

    #[test]
    fn a_line_longer_than_the_budget_is_still_read() {
        let (_tmp, claude, _app) = fixture();
        let line = answer_of_len("msg_long", AT, 3000);
        assert_eq!(line.len(), 3000);
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[line],
        );

        let mut state = ScanState::default();
        let scan = scan_commit(&claude, &mut state, 1000);

        assert_eq!(scan.answers.len(), 1, "{:?}", scan.answers);
        assert!(scan.complete, "one long line is read whole");
    }

    #[test]
    fn a_file_of_another_session_is_its_own_session() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION_B}.jsonl"),
            &[answer("msg_b", AT)],
        );

        let (state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 2);
        assert_eq!(state.sessions.len(), 2);
    }

    #[test]
    fn a_broken_assistant_line_is_counted_in_one_warning() {
        let (_tmp, claude, _app) = fixture();
        let broken = serde_json::json!({"type": "assistant", "timestamp": AT}).to_string();
        let not_json = "{\"type\": \"assistant\",".to_string();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[broken.clone(), not_json, answer("msg_a", AT)],
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 1);
        assert_eq!(
            scan.warnings,
            vec![format!("{KEY}: 2 lines could not be read")]
        );
    }

    #[test]
    fn lines_of_other_types_with_other_shapes_are_quiet() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                serde_json::json!({"type": "system", "message": "compacted"}).to_string(),
                serde_json::json!({"type": "attachment", "timestamp": 1794612960}).to_string(),
                serde_json::json!({"type": "summary", "cwd": {"x": 1}}).to_string(),
                answer("msg_a", AT),
            ],
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 1, "{:?}", scan.answers);
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    }

    #[test]
    fn blank_lines_and_a_byte_order_mark_are_quiet() {
        let (_tmp, claude, _app) = fixture();
        let first = answer("msg_a", AT);
        let second = assistant_line(
            "msg_b",
            "2026-11-12T09:42:00.000Z",
            ENGINE,
            OPUS,
            &["text"],
            USAGE,
        );
        let text = format!("\u{feff}{first}\n\n\r\n{second}\n");
        assert!(
            text.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]),
            "the file starts with a byte order mark"
        );
        let path = folder(&claude, FOLDER).join(format!("{SESSION}.jsonl"));
        fs::create_dir_all(folder(&claude, FOLDER)).unwrap();
        fs::write(&path, text.as_bytes()).unwrap();

        let (state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 2, "{:?}", scan.answers);
        assert_eq!(scan.answers[1].message_id, "msg_b");
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
        assert_eq!(
            state.files[KEY].offset,
            text.len() as u64,
            "the mark counts for the offset"
        );
    }

    #[test]
    fn a_shrunk_file_with_many_answers_counts_none_again() {
        let (_tmp, claude, _app) = fixture();
        let base = at(AT);
        let lines: Vec<String> = (0..800u32)
            .map(|n| {
                let at = (base + Duration::seconds(i64::from(n)))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                answer(&format!("msg_01QmB7xT{n:014}"), &at)
            })
            .collect();
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &lines,
        );
        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.answers.len(), 800);
        let old_offset = state.files[KEY].offset;

        let mut text = String::new();
        for line in &lines[..lines.len() - 1] {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(&path, text.as_bytes()).unwrap();
        assert!(
            (text.len() as u64) < old_offset,
            "the file is shorter than its offset"
        );
        let second = scan_commit(&claude, &mut state, BUDGET);

        assert!(second.answers.is_empty(), "{:?}", second.answers);
        assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
        assert!(
            second.warnings[0].contains("shrank"),
            "{:?}",
            second.warnings
        );
    }

    #[test]
    fn a_file_replaced_by_a_longer_one_is_read_again() {
        let (_tmp, claude, _app) = fixture();
        let first = vec![
            assistant_line("msg_a", AT, ENGINE, OPUS, &["thinking"], USAGE),
            assistant_line(
                "msg_b",
                "2026-11-12T09:42:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
            assistant_line(
                "msg_c",
                "2026-11-12T09:43:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
        ];
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &first,
        );
        let (mut state, first_scan) = scan_run(&claude);
        assert_eq!(first_scan.answers.len(), 3);
        let old_offset = state.files[KEY].offset as usize;

        // The same three answers, the first line a little longer, and three new ones.
        let longer = first[0].replace(
            "\"requestId\":\"req_011CZk3\"",
            &format!("\"requestId\":\"req_011CZk3{}\"", "x".repeat(40)),
        );
        let lines = [
            longer,
            first[1].clone(),
            first[2].clone(),
            assistant_line(
                "msg_d",
                "2026-11-12T09:44:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
            assistant_line(
                "msg_e",
                "2026-11-12T09:45:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
            assistant_line(
                "msg_f",
                "2026-11-12T09:46:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
        ];
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(&path, text.as_bytes()).unwrap();
        assert!(
            text.len() > old_offset,
            "the file is longer than its offset"
        );
        assert_ne!(
            text.as_bytes()[old_offset - 1],
            b'\n',
            "the old offset falls inside a line"
        );

        let second = scan_commit(&claude, &mut state, BUDGET);

        let mut ids: Vec<&str> = second
            .answers
            .iter()
            .map(|answer| answer.message_id.as_str())
            .collect();
        ids.sort();
        assert_eq!(ids, ["msg_d", "msg_e", "msg_f"], "{:?}", second.answers);
        assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
        assert!(
            second.warnings[0].contains("replaced"),
            "the file was replaced, not read from the middle: {:?}",
            second.warnings
        );
    }

    #[test]
    fn a_long_line_is_read_whole() {
        let (_tmp, claude, _app) = fixture();
        let long = answer_of_len("msg_long", "2026-11-12T09:42:00.000Z", 5 * 1024 * 1024);
        assert_eq!(long.len(), 5 * 1024 * 1024);
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                answer("msg_a", AT),
                long,
                assistant_line(
                    "msg_b",
                    "2026-11-12T09:43:00.000Z",
                    ENGINE,
                    OPUS,
                    &["text"],
                    USAGE,
                ),
            ],
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 3, "{:?}", scan.answers);
        assert_eq!(scan.answers[1].message_id, "msg_long");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_transcript_folder_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, claude, app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        let (mut state, scan) = scan_run(&claude);
        assert_eq!(scan.answers.len(), 1);
        let offset = state.files[KEY].offset;

        let dir = folder(&claude, FOLDER);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        let result = state.save(&claude, &app, Utc::now());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            matches!(&result, Err(TranscriptError::Io { .. })),
            "{result:?}"
        );
        assert_eq!(state.files[KEY].offset, offset, "the entry is still there");
    }

    #[test]
    fn a_reread_counts_two_new_answers_with_the_same_time() {
        let (_tmp, claude, _app) = fixture();
        let old_b = "2026-11-12T09:42:00.000Z";
        let new_at = "2026-11-12T09:43:00.000Z";
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                answer_of_len("msg_old_a", AT, 3000),
                answer_of_len("msg_old_b", old_b, 3000),
            ],
        );
        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.answers.len(), 2);
        let old_offset = state.files[KEY].offset;

        // The file shrinks, keeping the old answers and adding two new ones that
        // share a time later than the old last_answer.
        let text: String = [
            answer("msg_old_a", AT),
            answer("msg_old_b", old_b),
            answer("msg_new_a", new_at),
            answer("msg_new_b", new_at),
        ]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
        assert!(
            (text.len() as u64) < old_offset,
            "the file is shorter than its offset"
        );
        fs::write(&path, text.as_bytes()).unwrap();
        let second = scan_commit(&claude, &mut state, BUDGET);

        let mut ids: Vec<&str> = second
            .answers
            .iter()
            .map(|answer| answer.message_id.as_str())
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            ["msg_new_a", "msg_new_b"],
            "both new answers, none of the old: {:?}",
            second.answers
        );
        assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
        assert!(
            second.warnings[0].contains("shrank"),
            "{:?}",
            second.warnings
        );
    }

    #[test]
    fn a_replaced_file_ending_inside_a_line_warns_once() {
        let (_tmp, claude, app) = fixture();
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.answers.len(), 1);
        state
            .save(&claude, &app, Utc::now() + Duration::hours(25))
            .unwrap();
        assert!(state.files[KEY].recent.is_empty());

        let bytes = fs::read(&path).unwrap();
        let half = bytes.len() / 2;
        fs::write(&path, &bytes[..half]).unwrap();
        assert_ne!(bytes[half - 1], b'\n', "the new file ends inside a line");

        let second = scan_commit(&claude, &mut state, BUDGET);
        assert!(second.answers.is_empty(), "{:?}", second.answers);
        assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
        assert_eq!(state.files[KEY].offset, 0, "no whole line was read");

        let third = scan_commit(&claude, &mut state, BUDGET);
        assert!(third.answers.is_empty(), "{:?}", third.answers);
        assert!(third.warnings.is_empty(), "{:?}", third.warnings);
    }

    #[test]
    fn a_file_replaced_by_a_longer_one_without_ids_is_not_counted() {
        let (_tmp, claude, app) = fixture();
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        let (mut state, first) = scan_run(&claude);
        assert_eq!(first.answers.len(), 1);
        state
            .save(&claude, &app, Utc::now() + Duration::hours(25))
            .unwrap();
        assert!(state.files[KEY].recent.is_empty());
        let old_offset = state.files[KEY].offset as usize;

        let longer = answer("msg_a", AT).replace(
            "\"requestId\":\"req_011CZk3\"",
            &format!("\"requestId\":\"req_011CZk3{}\"", "x".repeat(100)),
        );
        let text: String = [
            longer,
            assistant_line(
                "msg_b",
                "2026-11-12T09:42:00.000Z",
                ENGINE,
                OPUS,
                &["text"],
                USAGE,
            ),
        ]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
        assert!(
            text.len() > old_offset,
            "the file is longer than its offset"
        );
        assert_ne!(
            text.as_bytes()[old_offset - 1],
            b'\n',
            "the old offset falls inside a line"
        );
        fs::write(&path, text.as_bytes()).unwrap();

        let second = scan_commit(&claude, &mut state, BUDGET);
        assert!(second.answers.is_empty(), "{:?}", second.answers);
        assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
        assert!(
            second.warnings[0].contains("is not counted"),
            "{:?}",
            second.warnings
        );
        assert_eq!(
            state.files[KEY].offset,
            text.len() as u64,
            "the offset is the end of the last whole line"
        );

        let third = scan_commit(&claude, &mut state, BUDGET);
        assert!(third.answers.is_empty(), "{:?}", third.answers);
        assert!(third.warnings.is_empty(), "{:?}", third.warnings);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, claude, app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[answer("msg_a", AT)],
        );
        write_lines(
            &folder(&claude, FOLDER_B),
            &format!("{SESSION_B}.jsonl"),
            &[answer("msg_b", AT)],
        );
        let (mut state, scan) = scan_run(&claude);
        assert_eq!(scan.answers.len(), 2);
        assert!(!state.files[KEY].recent.is_empty());

        let unreadable = folder(&claude, FOLDER_B);
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        let result = state.save(&claude, &app, Utc::now() + Duration::hours(25));
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            matches!(&result, Err(TranscriptError::Io { .. })),
            "{result:?}"
        );
        assert!(
            !state.files[KEY].recent.is_empty(),
            "a failed save changed a folder before it failed"
        );
    }

    #[test]
    fn a_crlf_split_across_two_chunks_is_one_line() {
        let (_tmp, claude, _app) = fixture();
        let json = answer_of_len("msg_crlf", AT, CHUNK_BYTES - 1);
        assert_eq!(json.len(), CHUNK_BYTES - 1);
        let path = write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[format!("{json}\r")],
        );
        let len = fs::metadata(&path).unwrap().len();
        assert_eq!(len, CHUNK_BYTES as u64 + 1);
        assert_eq!(fs::read(&path).unwrap()[CHUNK_BYTES - 1], b'\r');
        assert_eq!(fs::read(&path).unwrap()[CHUNK_BYTES], b'\n');

        let (state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 1, "{:?}", scan.answers);
        assert_eq!(scan.answers[0].message_id, "msg_crlf");
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
        assert_eq!(
            state.files[KEY].offset, len,
            "the offset is the file length"
        );
    }

    #[test]
    fn a_line_of_spaces_is_quiet() {
        let (_tmp, claude, _app) = fixture();
        write_lines(
            &folder(&claude, FOLDER),
            &format!("{SESSION}.jsonl"),
            &[
                answer("msg_a", AT),
                "   \t".to_string(),
                assistant_line(
                    "msg_b",
                    "2026-11-12T09:42:00.000Z",
                    ENGINE,
                    OPUS,
                    &["text"],
                    USAGE,
                ),
            ],
        );

        let (_state, scan) = scan_run(&claude);

        assert_eq!(scan.answers.len(), 2, "{:?}", scan.answers);
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
    }
}
