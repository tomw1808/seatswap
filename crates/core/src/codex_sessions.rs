//! Codex's session files: the rollout files under `<codex home>/sessions` (task C-1 of the 1.2.0 plan).
//!
//! One rollout is a JSON-lines file, `sessions/YYYY/MM/DD/rollout-<time>-<uuid>.jsonl`.
//! Its `session_meta` names the session and its folder; a `turn_context` names the
//! model of a turn; an `event_msg` of type `token_count` carries Codex's running
//! total, and `task_started`/`task_complete` say whether a task is open. Only
//! numbers and names leave this module.
//!
//! [`Tokens`]' fields follow OpenAI's usage fields, which Codex copies into
//! `total_token_usage`: `input_tokens` includes the cached part, so
//! `input = input_tokens - cached_input_tokens`; `cr = cached_input_tokens`;
//! `out = output_tokens` (which includes `reasoning_output_tokens`); and
//! `cw = cache_write_input_tokens`. OpenAI bills no cache write, so Codex's
//! real lines do not carry `cache_write_input_tokens`; a missing one is 0 by
//! that documented rule, not a guess. The total is running: the newest
//! `token_count` in time is taken as it is, never added up, so a later line
//! replaces the tokens a session reports, while a line without a total
//! replaces nothing.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::transcripts::Tokens;

/// One Codex session, as its rollout file gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSession {
    /// The `session_meta` line's `payload.id`.
    pub id: String,
    /// The `session_meta` line's `payload.cwd`.
    pub cwd: PathBuf,
    /// The last `turn_context` line's `payload.model` of the file whose last
    /// event is newest.
    pub model: Option<String>,
    /// The `session_meta` line's time.
    pub started: DateTime<Utc>,
    /// The newest line's time; `started` when no other line is newer.
    pub last: DateTime<Utc>,
    /// The newest `token_count`'s `info.total_token_usage` by event time over
    /// all the session's files, mapped as the module doc says; zero before the
    /// first `token_count`, so a file without a total never resets it.
    pub tokens: Tokens,
    /// The newest total minus the newest total at or before `now - 60 min`; the
    /// whole newest total when there is none. `None` when the newest total is
    /// lower than that baseline in a field, so the last hour is not known; the
    /// read warns about it then.
    pub last_hour: Option<Tokens>,
    /// A `task_started` without a later `task_complete`.
    pub busy: bool,
    /// The rollout file this session came from, so the process walk's open
    /// files can be matched to it. Not part of the page's shape.
    pub(crate) file: PathBuf,
}

/// What one read of Codex's session folder found, and what it could not read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexRead {
    pub sessions: Vec<CodexSession>,
    pub warnings: Vec<String>,
}

/// Every Codex session whose rollout file was last written at or after `since`.
/// Every day folder is listed, whatever its date; a rollout file is opened only
/// when one `metadata` call says its last write is at or after `since`, and an
/// older file is never read. The file's last write therefore decides, not its
/// day folder, so a session resumed in an old folder is read and there is no
/// UTC-versus-local day-folder question. A folder that does not exist is no
/// session and no warning.
///
/// A `token_count` with `info: null` is Codex's normal line before the first
/// answer: it changes nothing and is no warning. One whose `info` is an object
/// without `total_token_usage` is a warning, and so is a `token_count` with no
/// `info` key at all.
///
/// Two rollout files with the same session id are one session. The file whose
/// last event is newer gives its model, busy and last, and `started` is the
/// earlier of the two; the session's `tokens` is the newest total in time over
/// all its files, so a newer file without a total changes no token. `last_hour`
/// is the newest total minus the newest total at or before `now - 60 min` (the
/// whole newest total when there is none); a newest total lower than that
/// baseline makes it not known, with a warning naming the session.
///
/// A torn last line (no newline) is ignored. A broken whole line is a warning
/// naming the file and the line, and the rest of the file is still read. A file
/// without a `session_meta` line is a warning and no session.
pub fn sessions(codex_home: &Path, since: DateTime<Utc>, now: DateTime<Utc>) -> CodexRead {
    let root = codex_home.join("sessions");
    let mut read = CodexRead::default();
    let mut rollouts = Vec::new();
    for day in day_folders(&root, &mut read.warnings) {
        let entries = match fs::read_dir(&day) {
            Ok(entries) => entries,
            Err(source) => {
                read.warnings.push(format!("{}: {source}", day.display()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    read.warnings.push(format!("{}: {source}", day.display()));
                    continue;
                }
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            let file = entry.path();
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
                    // it can belong to the range, so it is not opened.
                    if modified < since {
                        continue;
                    }
                }
                Err(source) => {
                    read.warnings.push(format!(
                        "{}: the last write cannot be read, so the file was not read: {source}",
                        file.display()
                    ));
                    // Without the last write, the file cannot be said to be in
                    // the range; the warning says it was not read.
                    continue;
                }
            }
            if let Some(rollout) = read_one(&file, &mut read.warnings) {
                rollouts.push(rollout);
            }
        }
    }
    read.sessions = merge_rollouts(rollouts, now, &mut read.warnings);
    read.sessions
        .sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
    read
}

/// The `YYYY/MM/DD` folders of the store. Every day folder is listed, whatever
/// its date: the files' last writes, not the folders, decide what is read. Only
/// folders named as numbers are day folders; anything else is skipped. A day
/// folder that is a link to a folder is followed, and a link to something else
/// or to nothing is a warning naming it.
fn day_folders(root: &Path, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    let mut days = Vec::new();
    let years = match fs::read_dir(root) {
        Ok(years) => years,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return days,
        Err(source) => {
            warnings.push(format!("{}: {source}", root.display()));
            return days;
        }
    };
    for year in years {
        let year = match year {
            Ok(year) => year,
            Err(source) => {
                warnings.push(format!("{}: {source}", root.display()));
                continue;
            }
        };
        if !folder(&year, warnings) {
            continue;
        }
        if number(&year.file_name()).is_none() {
            continue;
        }
        let months = match fs::read_dir(year.path()) {
            Ok(months) => months,
            Err(source) => {
                warnings.push(format!("{}: {source}", year.path().display()));
                continue;
            }
        };
        for month in months {
            let month = match month {
                Ok(month) => month,
                Err(source) => {
                    warnings.push(format!("{}: {source}", year.path().display()));
                    continue;
                }
            };
            if !folder(&month, warnings) {
                continue;
            }
            if number(&month.file_name()).is_none() {
                continue;
            }
            let entries = match fs::read_dir(month.path()) {
                Ok(entries) => entries,
                Err(source) => {
                    warnings.push(format!("{}: {source}", month.path().display()));
                    continue;
                }
            };
            for day in entries {
                let day = match day {
                    Ok(day) => day,
                    Err(source) => {
                        warnings.push(format!("{}: {source}", month.path().display()));
                        continue;
                    }
                };
                if !folder(&day, warnings) {
                    continue;
                }
                if number(&day.file_name()).is_none() {
                    continue;
                }
                days.push(day.path());
            }
        }
    }
    days.sort();
    days
}

/// Whether a year, month or day entry is a folder. A plain entry is a folder
/// when its type says so; a link is followed with [`fs::metadata`], and one that
/// points at something other than a folder, or at nothing, is a warning naming
/// it. A plain file is neither a folder nor a warning.
fn folder(entry: &fs::DirEntry, warnings: &mut Vec<String>) -> bool {
    let path = entry.path();
    let kind = match entry.file_type() {
        Ok(kind) => kind,
        Err(source) => {
            warnings.push(format!("{}: {source}", path.display()));
            return false;
        }
    };
    if !kind.is_symlink() {
        return kind.is_dir();
    }
    match fs::metadata(&path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                true
            } else {
                warnings.push(format!(
                    "{}: the link does not point at a folder",
                    path.display()
                ));
                false
            }
        }
        Err(source) => {
            warnings.push(format!(
                "{}: the link cannot be followed: {source}",
                path.display()
            ));
            false
        }
    }
}

/// A folder name as its number; `None` when it is not one.
fn number(name: &std::ffi::OsStr) -> Option<u32> {
    name.to_str()?.parse().ok()
}

/// One rollout file as it was read: its session fields and every token total
/// with its line's time, so [`merge_rollouts`] can find the last hour across
/// two files of one session. `None` when it has no whole line, no `session_meta`
/// or cannot be read.
fn read_one(file: &Path, warnings: &mut Vec<String>) -> Option<Rollout> {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(source) => {
            warnings.push(format!("{}: {source}", file.display()));
            return None;
        }
    };
    // A file still being written ends with a line without its newline: that
    // torn line is not read.
    let end = bytes.iter().rposition(|byte| *byte == b'\n')?;
    let mut meta: Option<(String, PathBuf, DateTime<Utc>)> = None;
    let mut meta_seen = false;
    let mut model = None;
    let mut totals = Vec::new();
    let mut busy = false;
    let mut newest: Option<DateTime<Utc>> = None;
    for (index, raw) in bytes[..end].split(|byte| *byte == b'\n').enumerate() {
        let number = index + 1;
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let text = match std::str::from_utf8(raw) {
            Ok(text) => text,
            Err(_) => {
                warnings.push(format!(
                    "{}: line {number} is not valid UTF-8",
                    file.display()
                ));
                continue;
            }
        };
        let line: Line = match serde_json::from_str(text) {
            Ok(line) => line,
            Err(_) => {
                warnings.push(format!("{}: line {number} is not JSON", file.display()));
                continue;
            }
        };
        let Some(timestamp) = line.timestamp else {
            warnings.push(format!(
                "{}: line {number} has no timestamp",
                file.display()
            ));
            continue;
        };
        let at = match DateTime::parse_from_rfc3339(&timestamp) {
            Ok(at) => at.with_timezone(&Utc),
            Err(_) => {
                warnings.push(format!(
                    "{}: line {number} has a timestamp that is not an RFC 3339 time",
                    file.display()
                ));
                continue;
            }
        };
        newest = Some(newest.map_or(at, |newest| newest.max(at)));
        match line.kind.as_str() {
            "session_meta" => {
                meta_seen = true;
                if meta.is_none() {
                    let payload = line.payload.unwrap_or_default();
                    match (payload.id, payload.cwd) {
                        (Some(id), Some(cwd)) if !id.is_empty() && !cwd.is_empty() => {
                            meta = Some((id, PathBuf::from(cwd), at));
                        }
                        _ => warnings.push(format!(
                            "{}: line {number} is a session_meta without id or cwd",
                            file.display()
                        )),
                    }
                }
            }
            "turn_context" => {
                if let Some(value) = line.payload.and_then(|payload| payload.model) {
                    if !value.is_empty() {
                        model = Some(value);
                    }
                }
            }
            "event_msg" => {
                let Some(payload) = line.payload else {
                    continue;
                };
                match payload.kind.as_deref() {
                    Some("token_count") => {
                        let info = match payload.info {
                            // `info: null` is Codex's normal line before the
                            // first answer: it carries no total and is no
                            // warning. A missing `info` key is not that line.
                            Some(None) => continue,
                            Some(Some(info)) => info,
                            None => {
                                warnings.push(format!(
                                    "{}: line {number} is a token_count without an info key",
                                    file.display()
                                ));
                                continue;
                            }
                        };
                        let Some(usage) = info.total_token_usage else {
                            warnings.push(format!(
                                "{}: line {number} is a token_count without total_token_usage",
                                file.display()
                            ));
                            continue;
                        };
                        let (Some(input), Some(cached), Some(output)) = (
                            usage.input_tokens,
                            usage.cached_input_tokens,
                            usage.output_tokens,
                        ) else {
                            warnings.push(format!(
                                "{}: line {number} has a total_token_usage without input_tokens, cached_input_tokens or output_tokens",
                                file.display()
                            ));
                            continue;
                        };
                        if cached > input {
                            warnings.push(format!(
                                "{}: line {number} has cached_input_tokens {cached} above input_tokens {input}; its total is not taken",
                                file.display()
                            ));
                            continue;
                        }
                        // OpenAI's `input_tokens` includes the cached part and
                        // its `output_tokens` includes the reasoning tokens.
                        // `cache_write_input_tokens` is absent from Codex's real
                        // lines (OpenAI bills no cache write), so a missing one
                        // is 0 by the documented rule in the module doc.
                        totals.push((
                            at,
                            Tokens {
                                input: input - cached,
                                out: output,
                                cw: usage.cache_write_input_tokens.unwrap_or(0),
                                cr: cached,
                            },
                        ));
                    }
                    Some("task_started") => busy = true,
                    Some("task_complete") => busy = false,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let Some((id, cwd, started)) = meta else {
        if !meta_seen {
            warnings.push(format!("{}: no session_meta line", file.display()));
        }
        return None;
    };
    let last = newest.map_or(started, |newest| newest.max(started));
    Some(Rollout {
        id,
        cwd,
        model,
        started,
        last,
        busy,
        file: file.to_path_buf(),
        totals,
    })
}

/// One rollout file before sessions that share an id are merged.
#[derive(Debug, Clone)]
struct Rollout {
    id: String,
    cwd: PathBuf,
    model: Option<String>,
    started: DateTime<Utc>,
    last: DateTime<Utc>,
    busy: bool,
    file: PathBuf,
    /// The totals of every `token_count` line with that line's time.
    totals: Vec<(DateTime<Utc>, Tokens)>,
}

/// Turn the read files into sessions, one per id. The file whose last event is
/// newer gives the session's model, busy and last (a tie goes to the later
/// path, so the read order cannot change the result), and `started` is the
/// earlier of the files. The tokens are the newest total in time over all the
/// files, so a newer file without a total keeps the older file's total and no
/// file ever resets it to zero; a session with no total at all has zero.
///
/// Every file's totals are put in time order for the last hour, which compares
/// two of them: the newest total minus the newest total at or before
/// `now - 60 min` (the whole newest total when there is none). Only that pair
/// counts: a drop elsewhere in the history changes nothing. A newest total
/// below the baseline is a warning and leaves the last hour not known.
fn merge_rollouts(
    rollouts: Vec<Rollout>,
    now: DateTime<Utc>,
    warnings: &mut Vec<String>,
) -> Vec<CodexSession> {
    let mut by_id: BTreeMap<String, Vec<Rollout>> = BTreeMap::new();
    for rollout in rollouts {
        by_id.entry(rollout.id.clone()).or_default().push(rollout);
    }
    let mut sessions = Vec::with_capacity(by_id.len());
    for (id, group) in by_id {
        let giver = group
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.last.cmp(&b.last).then_with(|| a.file.cmp(&b.file)))
            .map(|(index, _)| index);
        let Some(giver) = giver else {
            continue;
        };
        let giver = &group[giver];
        let started = group.iter().fold(giver.started, |earliest, rollout| {
            earliest.min(rollout.started)
        });
        let mut totals: Vec<(DateTime<Utc>, PathBuf, Tokens)> = group
            .iter()
            .flat_map(|rollout| {
                rollout
                    .totals
                    .iter()
                    .map(move |(at, tokens)| (*at, rollout.file.clone(), *tokens))
            })
            .collect();
        totals.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let tokens = totals
            .last()
            .map(|(_, _, tokens)| *tokens)
            .unwrap_or_default();
        let after = now - Duration::minutes(60);
        let baseline = totals
            .iter()
            .rev()
            .find(|(at, _, _)| *at <= after)
            .map(|(_, _, tokens)| *tokens);
        let last_hour = match baseline {
            None => Some(tokens),
            Some(baseline) => subtract(tokens, baseline),
        };
        if last_hour.is_none() {
            warnings.push(format!(
                "Codex session {id}'s token total went down; its last hour is not known"
            ));
        }
        sessions.push(CodexSession {
            id,
            cwd: giver.cwd.clone(),
            model: giver.model.clone(),
            started,
            last: giver.last,
            tokens,
            last_hour,
            busy: giver.busy,
            file: giver.file.clone(),
        });
    }
    sessions
}

/// `newer - older`, field by field; `None` when one of `newer`'s counts is
/// smaller, so no negative token count is ever made.
fn subtract(newer: Tokens, older: Tokens) -> Option<Tokens> {
    Some(Tokens {
        input: newer.input.checked_sub(older.input)?,
        out: newer.out.checked_sub(older.out)?,
        cw: newer.cw.checked_sub(older.cw)?,
        cr: newer.cr.checked_sub(older.cr)?,
    })
}

/// One line of a rollout, as far as this module looks at it.
#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    payload: Option<Payload>,
}

/// The part of a line's payload this module reads; every other key is ignored.
#[derive(Deserialize, Default)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    /// `None` when the key is absent, `Some(None)` when it is `null`: a
    /// `token_count` without the key is broken, while `info: null` is Codex's
    /// normal early line.
    #[serde(default, deserialize_with = "info_or_null")]
    info: Option<Option<Info>>,
}

/// Read a present `info` key, keeping `null` apart from an absent key: serde
/// would collapse both into `None` on a plain `Option`.
fn info_or_null<'de, D>(deserializer: D) -> Result<Option<Option<Info>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Info>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use filetime::FileTime;
    use tempfile::TempDir;

    use crate::paths::CodexPaths;

    const ATLAS: &str = "3f7b2c1e-9a4d-4e8f-8b6a-2c5d9e1f7a30";
    const NOTES: &str = "c1a2b3c4-d5e6-4f70-8a9b-0c1d2e3f4a5b";
    const OLD: &str = "8e5d4c3b-2a19-4876-8543-210fedcba987";
    const CWD: &str = "/home/sam/projects/atlas";
    const MODEL: &str = "gpt-6.1-sol";

    fn store() -> (TempDir, CodexPaths) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = CodexPaths::at(tmp.path());
        (tmp, paths)
    }

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("a test time")
    }

    fn line(timestamp: &str, ordinal: u32, kind: &str, payload: serde_json::Value) -> String {
        serde_json::json!({
            "timestamp": timestamp,
            "ordinal": ordinal,
            "type": kind,
            "payload": payload,
        })
        .to_string()
    }

    fn meta(timestamp: &str, id: &str) -> String {
        line(
            timestamp,
            0,
            "session_meta",
            serde_json::json!({
                "id": id,
                "session_id": id,
                "cwd": CWD,
                "originator": "codex-tui",
                "source": "cli",
                "cli_version": "0.51.0",
                "model_provider": "openai",
            }),
        )
    }

    fn turn_context(timestamp: &str, ordinal: u32, model: &str) -> String {
        line(
            timestamp,
            ordinal,
            "turn_context",
            serde_json::json!({
                "turn_id": "9b8a7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d",
                "cwd": CWD,
                "model": model,
                "effort": "high",
            }),
        )
    }

    fn token_count(timestamp: &str, ordinal: u32, usage: [u64; 4]) -> String {
        line(
            timestamp,
            ordinal,
            "event_msg",
            serde_json::json!({
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": usage[0],
                        "cached_input_tokens": usage[1],
                        "cache_write_input_tokens": usage[2],
                        "output_tokens": usage[3],
                        "reasoning_output_tokens": 100,
                        "total_tokens": usage[0] + usage[3],
                    },
                    "last_token_usage": {
                        "input_tokens": 100,
                        "cached_input_tokens": 40,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 20,
                        "reasoning_output_tokens": 10,
                        "total_tokens": 120,
                    },
                },
                "rate_limits": {
                    "primary": {
                        "used_percent": 12.5,
                        "window_minutes": 300,
                        "resets_at": "2026-11-14T22:53:10Z",
                    },
                },
            }),
        )
    }

    /// A `token_count` as Codex really writes it: no `cache_write_input_tokens`
    /// (OpenAI bills no cache write), with `input_tokens` including the cached
    /// part and `output_tokens` including the reasoning tokens.
    fn token_count_without_cache_write(
        timestamp: &str,
        ordinal: u32,
        input: u64,
        cached: u64,
        output: u64,
    ) -> String {
        line(
            timestamp,
            ordinal,
            "event_msg",
            serde_json::json!({
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "output_tokens": output,
                        "reasoning_output_tokens": 100,
                        "total_tokens": input + output,
                    },
                },
                "rate_limits": {
                    "primary": {
                        "used_percent": 12.5,
                        "window_minutes": 300,
                        "resets_at": "2026-11-14T22:53:10Z",
                    },
                },
            }),
        )
    }

    fn task(timestamp: &str, ordinal: u32, kind: &str) -> String {
        let payload = if kind == "task_started" {
            serde_json::json!({
                "type": kind,
                "turn_id": "9b8a7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d",
                "started_at": 1794696790000_i64,
            })
        } else {
            serde_json::json!({
                "type": kind,
                "turn_id": "9b8a7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d",
                "completed_at": 1794696850000_i64,
                "duration_ms": 60000,
            })
        };
        line(timestamp, ordinal, "event_msg", payload)
    }

    /// Write one rollout file and give it a known last write.
    fn write(
        paths: &CodexPaths,
        day: [&str; 3],
        start: &str,
        id: &str,
        lines: &[String],
        modified: &str,
    ) -> PathBuf {
        let dir = paths
            .home
            .join("sessions")
            .join(day[0])
            .join(day[1])
            .join(day[2]);
        write_in(&dir, start, id, lines, modified)
    }

    /// Write one rollout file into `folder` and give it a known last write.
    fn write_in(folder: &Path, start: &str, id: &str, lines: &[String], modified: &str) -> PathBuf {
        fs::create_dir_all(folder).expect("a day folder");
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        let file = folder.join(format!("rollout-{start}-{id}.jsonl"));
        fs::write(&file, text).expect("a rollout file");
        let seconds = utc(modified).timestamp();
        filetime::set_file_mtime(&file, FileTime::from_unix_time(seconds, 0))
            .expect("a modification time");
        file
    }

    #[test]
    fn a_rollout_file_is_read() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T17-53-10",
            ATLAS,
            &[
                meta("2026-11-14T17:53:10.250Z", ATLAS),
                turn_context("2026-11-14T17:53:11.000Z", 1, MODEL),
                token_count("2026-11-14T17:54:00.000Z", 2, [12000, 8000, 300, 900]),
                task("2026-11-14T17:54:01.000Z", 3, "task_started"),
                task("2026-11-14T17:55:01.000Z", 4, "task_complete"),
            ],
            "2026-11-14T18:00:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T00:00:00Z"),
            utc("2026-11-14T19:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(read.sessions.len(), 1);
        let session = &read.sessions[0];
        assert_eq!(session.id, ATLAS);
        assert_eq!(session.cwd, PathBuf::from(CWD));
        assert_eq!(session.model.as_deref(), Some(MODEL));
        assert_eq!(session.started, utc("2026-11-14T17:53:10.250Z"));
        assert_eq!(session.last, utc("2026-11-14T17:55:01.000Z"));
        assert_eq!(
            session.tokens,
            Tokens {
                input: 4000,
                out: 900,
                cw: 300,
                cr: 8000,
            },
            "input_tokens includes the cached part; out includes reasoning"
        );
        assert!(!session.busy);
    }

    #[test]
    fn an_open_task_is_busy() {
        let (_tmp, paths) = store();
        // A task that was started again after its first completion is open.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T18-00-00",
            ATLAS,
            &[
                meta("2026-11-14T18:00:00.000Z", ATLAS),
                task("2026-11-14T18:00:01.000Z", 1, "task_started"),
                task("2026-11-14T18:01:01.000Z", 2, "task_complete"),
                task("2026-11-14T18:02:01.000Z", 3, "task_started"),
            ],
            "2026-11-14T18:03:00Z",
        );
        // A task that was completed is not.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T18-10-00",
            NOTES,
            &[
                meta("2026-11-14T18:10:00.000Z", NOTES),
                task("2026-11-14T18:10:01.000Z", 1, "task_started"),
                task("2026-11-14T18:11:01.000Z", 2, "task_complete"),
            ],
            "2026-11-14T18:12:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T00:00:00Z"),
            utc("2026-11-14T19:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        let open = read
            .sessions
            .iter()
            .find(|session| session.id == ATLAS)
            .expect("the open task");
        assert!(open.busy);
        assert_eq!(
            open.tokens,
            Tokens::default(),
            "a session with no total yet has used nothing, not a reset"
        );
        assert_eq!(
            open.last_hour,
            Some(Tokens::default()),
            "a session with no total yet used nothing in the last hour"
        );
        let done = read
            .sessions
            .iter()
            .find(|session| session.id == NOTES)
            .expect("the finished task");
        assert!(!done.busy);
    }

    #[test]
    fn only_the_last_token_count_counts() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T18-20-00",
            ATLAS,
            &[
                meta("2026-11-14T18:20:00.000Z", ATLAS),
                token_count("2026-11-14T18:20:30.000Z", 1, [1000, 400, 10, 100]),
                token_count("2026-11-14T18:21:30.000Z", 2, [2500, 1200, 40, 300]),
                token_count("2026-11-14T18:22:30.000Z", 3, [4000, 2600, 90, 700]),
            ],
            "2026-11-14T18:23:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T00:00:00Z"),
            utc("2026-11-14T19:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(read.sessions.len(), 1);
        assert_eq!(
            read.sessions[0].tokens,
            Tokens {
                input: 1400,
                out: 700,
                cw: 90,
                cr: 2600,
            },
            "the running total is taken as it is, not added up"
        );
    }

    #[test]
    fn a_file_from_before_since_is_skipped() {
        let (_tmp, paths) = store();
        let since = utc("2026-11-14T10:00:00Z");
        // Its day folder is before `since`'s day, but every day folder is
        // listed: its last write, before `since`, is what skips it.
        write(
            &paths,
            ["2026", "11", "10"],
            "2026-11-10T09-00-00",
            OLD,
            &[meta("2026-11-10T09:00:00.000Z", OLD)],
            "2026-11-10T09:30:00Z",
        );
        // Its day is `since`'s day, but its last write is before `since`.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T08-50-00",
            NOTES,
            &[meta("2026-11-14T08:50:00.000Z", NOTES)],
            "2026-11-14T09:00:00Z",
        );
        // This one is in range.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T10-30-00",
            ATLAS,
            &[meta("2026-11-14T10:30:00.000Z", ATLAS)],
            "2026-11-14T11:00:00Z",
        );

        let read = sessions(&paths.home, since, utc("2026-11-14T11:00:00Z"));

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(read.sessions.len(), 1);
        assert_eq!(read.sessions[0].id, ATLAS);
    }

    #[test]
    fn a_broken_line_is_a_warning_and_the_rest_is_read() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T18-30-00",
            ATLAS,
            &[
                meta("2026-11-14T18:30:00.000Z", ATLAS),
                "{not json}".to_string(),
                turn_context("2026-11-14T18:30:01.000Z", 2, MODEL),
            ],
            "2026-11-14T18:31:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T00:00:00Z"),
            utc("2026-11-14T19:00:00Z"),
        );

        assert_eq!(read.sessions.len(), 1);
        assert_eq!(read.sessions[0].model.as_deref(), Some(MODEL));
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        assert!(
            read.warnings[0].contains("line 2"),
            "the warning names the line: {}",
            read.warnings[0]
        );
    }

    #[test]
    fn a_file_without_session_meta_is_a_warning_and_no_session() {
        let (_tmp, paths) = store();
        let file = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T18-40-00",
            ATLAS,
            &[turn_context("2026-11-14T18:40:01.000Z", 1, MODEL)],
            "2026-11-14T18:41:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T00:00:00Z"),
            utc("2026-11-14T19:00:00Z"),
        );

        assert!(read.sessions.is_empty());
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        assert!(
            read.warnings[0].contains(file.to_str().expect("a text path")),
            "the warning names the file: {}",
            read.warnings[0]
        );
    }

    #[test]
    fn proof_a_token_count_without_info_is_no_warning_and_keeps_the_total() {
        let (_tmp, paths) = store();
        // Codex writes `info: null` before the first answer and may write it
        // again later; it is a normal line, not a broken one.
        let empty = line(
            "2026-11-14T18:50:30.000Z",
            2,
            "event_msg",
            serde_json::json!({"type": "token_count", "info": null, "rate_limits": {}}),
        );
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T18-50-00",
            ATLAS,
            &[
                meta("2026-11-14T18:50:00.000Z", ATLAS),
                token_count("2026-11-14T18:50:20.000Z", 1, [4000, 2600, 90, 700]),
                empty,
            ],
            "2026-11-14T18:51:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T00:00:00Z"),
            utc("2026-11-14T19:00:00Z"),
        );

        assert_eq!(
            read.sessions[0].tokens.out, 700,
            "the earlier total is kept"
        );
        assert!(
            read.warnings.is_empty(),
            "a null info is Codex's normal line before an answer: {:?}",
            read.warnings
        );
    }

    #[test]
    fn proof_one_session_id_in_two_files_is_one_session() {
        let (_tmp, paths) = store();
        // A session continued in a second file (a resume or a fork that keeps
        // the id) must not become two rows with the same session id.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T19-00-00",
            ATLAS,
            &[meta("2026-11-14T19:00:00.000Z", ATLAS)],
            "2026-11-14T19:01:00Z",
        );
        write(
            &paths,
            ["2026", "11", "15"],
            "2026-11-15T08-00-00",
            ATLAS,
            &[meta("2026-11-15T08:00:00.000Z", ATLAS)],
            "2026-11-15T08:01:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T00:00:00Z"),
            utc("2026-11-15T09:00:00Z"),
        );

        assert_eq!(
            read.sessions.iter().filter(|s| s.id == ATLAS).count(),
            1,
            "{:?}",
            read.sessions
        );
    }

    #[test]
    fn proof_a_resumed_old_file_written_now_is_read() {
        let (_tmp, paths) = store();
        // A resumed session keeps writing to the file of the day it began; the
        // file's write time, not its day folder, says it is current.
        write(
            &paths,
            ["2026", "11", "10"],
            "2026-11-10T09-00-00",
            ATLAS,
            &[
                meta("2026-11-10T09:00:00.000Z", ATLAS),
                task("2026-11-14T10:20:00.000Z", 1, "task_started"),
            ],
            "2026-11-14T10:20:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T11:00:00Z"),
        );

        assert_eq!(read.sessions.len(), 1, "{:?}", read.warnings);
    }

    #[test]
    fn a_token_count_without_a_total_usage_is_a_warning() {
        let (_tmp, paths) = store();
        let empty_info = line(
            "2026-11-14T11:50:30.000Z",
            2,
            "event_msg",
            serde_json::json!({"type": "token_count", "info": {}}),
        );
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T11-50-00",
            ATLAS,
            &[
                meta("2026-11-14T11:50:00.000Z", ATLAS),
                token_count("2026-11-14T11:50:20.000Z", 1, [4000, 2600, 0, 700]),
                empty_info,
            ],
            "2026-11-14T11:51:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert_eq!(
            read.sessions[0].tokens.out, 700,
            "the earlier total is kept"
        );
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        assert!(
            read.warnings[0].contains("without total_token_usage"),
            "an info object without a total is still a warning: {}",
            read.warnings[0]
        );
    }

    #[test]
    fn one_session_id_in_two_files_keeps_the_newer_file_in_both_orders() {
        let (_tmp, paths) = store();
        let first = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T19-00-00",
            ATLAS,
            &[
                meta("2026-11-14T19:00:00.000Z", ATLAS),
                turn_context("2026-11-14T19:00:01.000Z", 1, "gpt-6.1-sol"),
                token_count_without_cache_write("2026-11-14T19:00:20.000Z", 2, 4000, 2600, 700),
                task("2026-11-14T19:00:30.000Z", 3, "task_started"),
            ],
            "2026-11-14T19:01:00Z",
        );
        let second = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T20-00-00",
            ATLAS,
            &[
                meta("2026-11-14T20:00:00.000Z", ATLAS),
                turn_context("2026-11-14T20:00:01.000Z", 1, "gpt-6.2-sol"),
                token_count_without_cache_write("2026-11-14T20:00:20.000Z", 2, 6000, 4000, 800),
            ],
            "2026-11-14T20:01:00Z",
        );
        let now = utc("2026-11-14T20:10:00Z");
        let first_rollout = read_one(&first, &mut Vec::new()).expect("the first file");
        let second_rollout = read_one(&second, &mut Vec::new()).expect("the second file");

        let mut forward_warnings = Vec::new();
        let forward = merge_rollouts(
            vec![first_rollout.clone(), second_rollout.clone()],
            now,
            &mut forward_warnings,
        );
        let mut backward_warnings = Vec::new();
        let backward = merge_rollouts(
            vec![second_rollout, first_rollout],
            now,
            &mut backward_warnings,
        );

        assert_eq!(
            forward, backward,
            "the read order must not change the session"
        );
        assert!(forward_warnings.is_empty(), "{forward_warnings:?}");
        assert_eq!(forward.len(), 1);
        let session = &forward[0];
        assert_eq!(session.started, utc("2026-11-14T19:00:00.000Z"));
        assert_eq!(session.last, utc("2026-11-14T20:00:20.000Z"));
        assert_eq!(session.model.as_deref(), Some("gpt-6.2-sol"));
        assert!(!session.busy, "the newer file has no open task");
        assert_eq!(
            session.tokens,
            Tokens {
                input: 2000,
                out: 800,
                cw: 0,
                cr: 4000,
            },
            "the newer file gives the tokens"
        );
    }

    #[test]
    fn an_old_file_before_since_is_not_read() {
        let (_tmp, paths) = store();
        let since = utc("2026-11-14T10:00:00Z");
        let _file = write(
            &paths,
            ["2026", "11", "10"],
            "2026-11-10T09-00-00",
            OLD,
            &["{not json}".to_string()],
            "2026-11-10T09:30:00Z",
        );

        let read = sessions(&paths.home, since, utc("2026-11-14T11:00:00Z"));

        assert!(read.sessions.is_empty());
        assert!(
            read.warnings.is_empty(),
            "a file written before since is not opened: {:?}",
            read.warnings
        );
    }

    #[test]
    fn a_real_token_count_line_without_a_cache_write_maps_the_usage() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T11-50-00",
            ATLAS,
            &[
                meta("2026-11-14T11:50:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 1, 12000, 8000, 900),
            ],
            "2026-11-14T11:56:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(
            read.sessions[0].tokens,
            Tokens {
                input: 4000,
                out: 900,
                cw: 0,
                cr: 8000,
            },
            "input_tokens includes the cached part; a missing cache write is 0"
        );
    }

    #[test]
    fn a_cached_count_above_the_input_is_a_warning_and_its_total_is_not_taken() {
        let (_tmp, paths) = store();
        let file = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T11-50-00",
            ATLAS,
            &[
                meta("2026-11-14T11:50:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T11:50:20.000Z", 1, 4000, 2600, 700),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 2, 1000, 1500, 100),
            ],
            "2026-11-14T11:56:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert_eq!(
            read.sessions[0].tokens,
            Tokens {
                input: 1400,
                out: 700,
                cw: 0,
                cr: 2600,
            },
            "the bad line's total is not taken"
        );
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        let warning = &read.warnings[0];
        assert!(
            warning.contains("line 3"),
            "the warning names the line: {warning}"
        );
        assert!(
            warning.contains(file.to_str().expect("a text path")),
            "the warning names the file: {warning}"
        );
    }

    #[test]
    fn last_hour_is_the_newest_total_minus_the_one_before_the_hour() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T10-00-00",
            ATLAS,
            &[
                meta("2026-11-14T10:00:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T10:30:00.000Z", 1, 10000, 6000, 1000),
                token_count_without_cache_write("2026-11-14T11:10:00.000Z", 2, 14000, 8000, 1500),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 3, 20000, 12000, 2000),
            ],
            "2026-11-14T11:56:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        let session = &read.sessions[0];
        assert_eq!(
            session.last_hour,
            Some(Tokens {
                input: 4000,
                out: 1000,
                cw: 0,
                cr: 6000,
            }),
            "total(-5 min) - total(-90 min), the last total at or before -60 min"
        );
    }

    #[test]
    fn a_session_within_the_hour_has_its_whole_total_for_last_hour() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T11-40-00",
            ATLAS,
            &[
                meta("2026-11-14T11:40:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 1, 10000, 6000, 1000),
            ],
            "2026-11-14T11:56:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        let session = &read.sessions[0];
        assert_eq!(
            session.last_hour,
            Some(Tokens {
                input: 4000,
                out: 1000,
                cw: 0,
                cr: 6000,
            }),
            "a first total within the hour is the whole newest total"
        );
    }

    #[test]
    fn a_newest_total_below_the_baseline_is_a_warning_and_the_last_hour_is_not_known() {
        let (_tmp, paths) = store();
        // The newest total at or before the hour's start is the baseline.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T10-20-00",
            ATLAS,
            &[
                meta("2026-11-14T10:20:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T10:30:00.000Z", 1, 20000, 12000, 2000),
            ],
            "2026-11-14T10:31:00Z",
        );
        // A new file of the same session writes a lower total than the baseline.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T11-41-00",
            ATLAS,
            &[
                meta("2026-11-14T10:20:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 1, 10000, 6000, 1000),
            ],
            "2026-11-14T11:56:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert_eq!(read.sessions.len(), 1);
        let warning = read
            .warnings
            .iter()
            .find(|warning| warning.contains("went down"))
            .expect("a newest total below the baseline is a warning");
        assert!(
            warning.contains(ATLAS),
            "the warning names the session: {warning}"
        );
        let session = &read.sessions[0];
        assert_eq!(
            session.tokens,
            Tokens {
                input: 4000,
                out: 1000,
                cw: 0,
                cr: 6000,
            },
            "the session's tokens are its newest total"
        );
        assert_eq!(
            session.last_hour, None,
            "the last hour is not known when the newest total is below the baseline"
        );
    }
    /// Two files of one session; the newer one has no token total yet (a
    /// resumed session's new file starts with `info: null` lines).
    fn old_file_with_total_new_file_without(paths: &CodexPaths) {
        write(
            paths,
            ["2026", "11", "14"],
            "2026-11-14T10-00-00",
            ATLAS,
            &[
                meta("2026-11-14T10:00:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T10:05:00.000Z", 1, 10000, 6000, 1000),
            ],
            "2026-11-14T10:06:00Z",
        );
        write(
            paths,
            ["2026", "11", "14"],
            "2026-11-14T11-50-00",
            ATLAS,
            &[
                meta("2026-11-14T10:00:00.000Z", ATLAS),
                turn_context("2026-11-14T11:50:00.000Z", 1, MODEL),
            ],
            "2026-11-14T11:51:00Z",
        );
    }

    #[test]
    fn proof_a_newer_file_without_a_total_is_no_went_down_warning() {
        let (_tmp, paths) = store();
        old_file_with_total_new_file_without(&paths);
        let read = sessions(
            &paths.home,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );
        assert_eq!(read.sessions.len(), 1);
        // No total went down: the old file's only total is the session's only
        // total, and it is also the baseline for the hour.
        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(
            read.sessions[0].last_hour,
            Some(Tokens::default()),
            "the old file's total is the baseline, so nothing was used in the hour"
        );
    }

    #[test]
    fn proof_a_newer_file_without_a_total_keeps_the_older_files_total() {
        let (_tmp, paths) = store();
        old_file_with_total_new_file_without(&paths);
        let read = sessions(
            &paths.home,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );
        assert_eq!(
            read.sessions[0].tokens,
            Tokens {
                input: 4000,
                out: 1000,
                cw: 0,
                cr: 6000,
            },
            "the session's tokens fall to 0 although a total of 10000 input was read"
        );
    }

    #[test]
    fn proof_a_token_count_without_an_info_key_is_a_warning() {
        let (_tmp, paths) = store();
        let no_info = line(
            "2026-11-14T11:50:30.000Z",
            2,
            "event_msg",
            serde_json::json!({"type": "token_count"}),
        );
        let file = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T11-50-00",
            ATLAS,
            &[meta("2026-11-14T11:50:00.000Z", ATLAS), no_info],
            "2026-11-14T11:51:00Z",
        );
        let read = sessions(
            &paths.home,
            utc("2026-11-14T09:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        let warning = &read.warnings[0];
        assert!(
            warning.contains(file.to_str().expect("a text path")),
            "the warning names the file: {warning}"
        );
        assert!(
            warning.contains("line 2"),
            "the warning names the line: {warning}"
        );
        assert!(
            warning.contains("without an info key"),
            "the warning says what is missing: {warning}"
        );
    }

    /// A `token_count` whose timestamp is not a time at all.
    #[test]
    fn an_invalid_timestamp_warning_names_the_file_and_line_not_its_text() {
        let (_tmp, paths) = store();
        let text = "yesterday at noon";
        let file = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T11-50-00",
            ATLAS,
            &[
                meta("2026-11-14T11:50:00.000Z", ATLAS),
                line(text, 1, "turn_context", serde_json::json!({"model": MODEL})),
            ],
            "2026-11-14T11:51:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T11:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert_eq!(read.sessions.len(), 1);
        assert!(
            read.sessions[0].model.is_none(),
            "the line with the invalid timestamp is skipped"
        );
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        let warning = &read.warnings[0];
        assert!(
            warning.contains(file.to_str().expect("a text path")),
            "the warning names the file: {warning}"
        );
        assert!(
            warning.contains("line 2"),
            "the warning names the line: {warning}"
        );
        assert!(
            !warning.contains(text),
            "the warning does not repeat the field's text: {warning}"
        );
    }

    /// Two files of one session whose totals form one series: file A's older
    /// total is above file B's, but only the newest total at or before the hour
    /// and the newest total are compared.
    #[test]
    fn a_drop_before_the_baseline_is_no_warning_and_the_last_hour_is_the_two_newest_totals() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T06-50-00",
            ATLAS,
            &[
                meta("2026-11-14T06:50:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T07:00:00.000Z", 1, 20000, 12000, 2000),
            ],
            "2026-11-14T07:01:00Z",
        );
        // A newer file of the same session starts below file A's total; its own
        // series is the one the hour compares.
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T10-20-00",
            ATLAS,
            &[
                meta("2026-11-14T06:50:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T10:30:00.000Z", 1, 10000, 6000, 1000),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 2, 14000, 8000, 1500),
            ],
            "2026-11-14T11:56:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T06:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        let session = &read.sessions[0];
        assert_eq!(
            session.last_hour,
            Some(Tokens {
                input: 2000,
                out: 500,
                cw: 0,
                cr: 2000,
            }),
            "total(-5 min) - total(-90 min); the older drop changes nothing"
        );
        assert_eq!(
            session.tokens,
            Tokens {
                input: 6000,
                out: 1500,
                cw: 0,
                cr: 8000,
            },
            "the session's tokens are its newest total, in file B"
        );
    }

    /// A drop between the baseline and the newest total is not compared: only
    /// the baseline and the newest total are.
    #[test]
    fn a_drop_between_the_baseline_and_the_newest_total_is_no_warning() {
        let (_tmp, paths) = store();
        write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T10-00-00",
            ATLAS,
            &[
                meta("2026-11-14T10:00:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T10:30:00.000Z", 1, 10000, 6000, 1000),
                token_count_without_cache_write("2026-11-14T11:10:00.000Z", 2, 5000, 3000, 500),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 3, 14000, 8000, 1500),
            ],
            "2026-11-14T11:56:00Z",
        );

        let read = sessions(
            &paths.home,
            utc("2026-11-14T10:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(
            read.sessions[0].last_hour,
            Some(Tokens {
                input: 2000,
                out: 500,
                cw: 0,
                cr: 2000,
            }),
            "the newest total minus the baseline, ignoring the dip between them"
        );
        assert_eq!(
            read.sessions[0].tokens,
            Tokens {
                input: 6000,
                out: 1500,
                cw: 0,
                cr: 8000,
            },
            "the session's tokens are its newest total"
        );
    }

    /// The tokens are the newest total in time over the files, whichever order
    /// the files are merged in.
    #[test]
    fn the_newest_total_is_taken_in_both_orders() {
        let (_tmp, paths) = store();
        let with_total = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T10-00-00",
            ATLAS,
            &[
                meta("2026-11-14T10:00:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T10:05:00.000Z", 1, 10000, 6000, 1000),
            ],
            "2026-11-14T10:06:00Z",
        );
        let without_total = write(
            &paths,
            ["2026", "11", "14"],
            "2026-11-14T10-50-00",
            ATLAS,
            &[
                meta("2026-11-14T10:00:00.000Z", ATLAS),
                turn_context("2026-11-14T10:50:00.000Z", 1, MODEL),
            ],
            "2026-11-14T10:51:00Z",
        );
        let now = utc("2026-11-14T11:00:00Z");
        let with_total = read_one(&with_total, &mut Vec::new()).expect("the file with a total");
        let without_total =
            read_one(&without_total, &mut Vec::new()).expect("the file without a total");

        let mut forward_warnings = Vec::new();
        let forward = merge_rollouts(
            vec![with_total.clone(), without_total.clone()],
            now,
            &mut forward_warnings,
        );
        let mut backward_warnings = Vec::new();
        let backward = merge_rollouts(vec![without_total, with_total], now, &mut backward_warnings);

        assert_eq!(
            forward, backward,
            "the read order must not change the session"
        );
        assert!(forward_warnings.is_empty(), "{forward_warnings:?}");
        assert_eq!(forward.len(), 1);
        assert_eq!(
            forward[0].tokens,
            Tokens {
                input: 4000,
                out: 1000,
                cw: 0,
                cr: 6000,
            },
            "the total of the file without a newer one is the session's total"
        );
        assert_eq!(
            forward[0].last_hour,
            Some(Tokens {
                input: 4000,
                out: 1000,
                cw: 0,
                cr: 6000,
            }),
            "the only total is within the hour, so it is the whole last hour"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_day_folder_that_is_a_link_to_a_folder_is_read() {
        use std::os::unix::fs::symlink;

        let (_tmp, paths) = store();
        let moved = paths.home.join("moved-sessions").join("14");
        write_in(
            &moved,
            "2026-11-14T11-50-00",
            ATLAS,
            &[
                meta("2026-11-14T11:50:00.000Z", ATLAS),
                token_count_without_cache_write("2026-11-14T11:55:00.000Z", 1, 4000, 2600, 700),
            ],
            "2026-11-14T11:56:00Z",
        );
        let day = paths
            .home
            .join("sessions")
            .join("2026")
            .join("11")
            .join("14");
        fs::create_dir_all(day.parent().expect("a month folder")).expect("a month folder");
        symlink(&moved, &day).expect("a day folder link");

        let read = sessions(
            &paths.home,
            utc("2026-11-14T11:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert!(read.warnings.is_empty(), "{:?}", read.warnings);
        assert_eq!(read.sessions.len(), 1);
        assert_eq!(read.sessions[0].id, ATLAS);
        assert_eq!(
            read.sessions[0].tokens,
            Tokens {
                input: 1400,
                out: 700,
                cw: 0,
                cr: 2600,
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_day_folder_link_to_something_else_or_to_nothing_is_a_warning() {
        use std::os::unix::fs::symlink;

        let (_tmp, paths) = store();
        let month = paths.home.join("sessions").join("2026").join("11");
        fs::create_dir_all(&month).expect("a month folder");
        let note = paths.home.join("not-a-folder.json");
        fs::write(&note, "{}\n").expect("a file");
        let to_file = month.join("14");
        symlink(&note, &to_file).expect("a link to a file");
        let broken = month.join("15");
        symlink(paths.home.join("gone"), &broken).expect("a broken link");

        let read = sessions(
            &paths.home,
            utc("2026-11-14T11:00:00Z"),
            utc("2026-11-14T12:00:00Z"),
        );

        assert!(read.sessions.is_empty());
        assert_eq!(read.warnings.len(), 2, "{:?}", read.warnings);
        assert!(
            read.warnings
                .iter()
                .any(|warning| warning.contains(to_file.to_str().expect("a text path"))),
            "the link to a file is named: {:?}",
            read.warnings
        );
        assert!(
            read.warnings
                .iter()
                .any(|warning| warning.contains(broken.to_str().expect("a text path"))),
            "the broken link is named: {:?}",
            read.warnings
        );
    }
}
