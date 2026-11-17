//! Bringing a host folder's files into another folder without losing a line
//! (spec §1.5): the import of other hosts' folders, and this host's own folder
//! when `historyDir` names a new root.
//!
//! A month file's lines are only ever appended, never rewritten: every reader
//! sorts the records it reads by their time ([`HistoryDir::read`]), so the
//! order of lines in a file does not matter, and a program that appends to the
//! same file meanwhile keeps its line. The source folder is never changed.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::read::{failed, file_lines, folder_exists, read_bytes, same_folder};
use super::{
    location, read_marker, read_or_create_machine_id, HistoryDir, HistoryError, SESSIONS_NOW_FILE,
};
use crate::atomic::{self, Access, MAX_LINE_BYTES};
use crate::locks;
use crate::paths::AppPaths;
use crate::store::Settings;

/// How long bringing the history along waits for the app lock: another
/// program may be bringing the same folder along right now, and waiting for it
/// is the same work as doing it.
pub const BRING_ALONG_LOCK_TIMEOUT: Duration = Duration::from_secs(120);

/// `history-root.json` in the data folder: the root this host's history folder
/// was last brought to or opened in.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastRoot {
    history_root: PathBuf,
}

/// Whether [`bring_along`] has anything to do: the remembered root differs
/// from the one the settings name now, or no root is remembered yet. Takes no
/// lock and writes nothing.
pub fn bring_along_due(app: &AppPaths, settings: &Settings) -> Result<bool, HistoryError> {
    let (root, _) = location(app, settings)?;
    Ok(read_last_root(app)?.as_deref() != Some(root.as_path()))
}

/// Bring this host's folder to the history root the settings name now, from
/// the root it was last in (`history-root.json`; when that file is missing,
/// `<data folder>/history`, the folder every earlier version used without the
/// setting). Under the app lock, so two programs never do it at once.
///
/// - `<old root>/<host>` missing or not marked: nothing of this host is
///   there; the new root is remembered.
/// - `<old root>/<host>` marked with another machine id: nothing is copied,
///   the new root is remembered, and one warning names the folder, both ids
///   and the steps that bring it along if it is this machine's after all.
/// - `<new root>/<host>` missing: it is made, marked with this machine's id,
///   and every file is copied.
/// - `<new root>/<host>` marked with this machine's id, or not marked at all
///   (which [`HistoryDir::open`] claims for this machine anyway): it is marked
///   and the files are merged ([`merge_folder`]).
/// - `<new root>/<host>` marked with another machine's id: nothing is copied
///   and the root is not remembered; opening the folder names the conflict
///   ([`HistoryError::HostTaken`]).
///
/// The old folder is never changed. The new root is remembered only after
/// everything was copied, so a failed or refused move is tried again at the
/// next start. The caller makes sure no program of this host writes the old
/// folder meanwhile (the tray app at its start, a command only while no tray
/// app runs). Returns the warnings: an old folder of another machine id, and
/// lines that were copied although they are not valid JSON.
pub fn bring_along(app: &AppPaths, settings: &Settings) -> Result<Vec<String>, HistoryError> {
    let (root, host) = location(app, settings)?;
    if read_last_root(app)?.as_deref() == Some(root.as_path()) {
        return Ok(Vec::new());
    }
    let _lock = locks::acquire_app_lock(app, BRING_ALONG_LOCK_TIMEOUT)?;
    // Again under the lock: another program may have brought it meanwhile.
    let last = read_last_root(app)?.unwrap_or_else(|| app.history_dir());
    if last == root || same_folder(&last, &root) {
        remember_root(app, &root)?;
        return Ok(Vec::new());
    }
    let old_dir = last.join(&host);
    let new_dir = root.join(&host);
    if !folder_exists(&old_dir)? {
        remember_root(app, &root)?;
        return Ok(Vec::new());
    }
    let machine_id = read_or_create_machine_id(app)?;
    let old_marker = old_dir.join("host.json");
    match read_marker(&old_marker)? {
        Some(marker) if marker.machine_id == machine_id => {}
        // Marked by another id: another machine's folder, or this machine's
        // from before its id was made anew. Only the person can tell, so it is
        // said once (the new root is remembered) with what brings it along.
        Some(marker) => {
            remember_root(app, &root)?;
            return Ok(vec![not_this_machines(
                &old_dir,
                &marker.machine_id,
                &machine_id,
                &new_dir,
                &last,
                &app.history_root_file(),
            )]);
        }
        // Not marked: this machine marks a folder before its first line, so
        // it never wrote a record there. Nothing to bring.
        None => {
            remember_root(app, &root)?;
            return Ok(Vec::new());
        }
    }
    if folder_exists(&new_dir)? {
        if let Some(marker) = read_marker(&new_dir.join("host.json"))? {
            if marker.machine_id != machine_id {
                return Ok(Vec::new());
            }
        }
        // No host.json: `HistoryDir::open` claims such a folder for this
        // machine, so it is merged like one marked with this machine's id.
    }
    atomic::ensure_private_dir(&new_dir).map_err(|source| HistoryError::Io {
        path: new_dir.clone(),
        source,
    })?;
    // Marked first: a copy that stops halfway is merged at the next start.
    HistoryDir::at(&root, &host).check_or_write_marker(
        &new_dir.join("host.json"),
        &machine_id,
        &app.machine_id_file(),
    )?;
    let merged = merge_folder(&old_dir, &new_dir)?;
    remember_root(app, &root)?;
    Ok(merged.warnings)
}

/// The warning for an old host folder marked with another machine id: what
/// was not brought, and the steps that bring it when it is this machine's.
fn not_this_machines(
    old_dir: &Path,
    theirs: &str,
    ours: &str,
    new_dir: &Path,
    old_root: &Path,
    root_file: &Path,
) -> String {
    format!(
        "{} was not brought to {}: its host.json names machine id {theirs:?}, and this \
         machine's id is {ours}. If that history is this machine's (its id was made anew), \
         quit seatswap, put {ours} into {} as machineId, write {{\"historyRoot\":{}}} into {} \
         and start seatswap again: it then brings that history along.",
        old_dir.display(),
        new_dir.display(),
        old_dir.join("host.json").display(),
        serde_json::Value::String(old_root.display().to_string()),
        root_file.display()
    )
}

/// The remembered root, or `None` when `history-root.json` is not there. A file
/// that does not parse, or names a relative folder, is an error naming it.
fn read_last_root(app: &AppPaths) -> Result<Option<PathBuf>, HistoryError> {
    let path = app.history_root_file();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(HistoryError::Io { path, source }),
    };
    let last: LastRoot = serde_json::from_str(&text).map_err(|source| HistoryError::Json {
        path: path.clone(),
        line: source.line(),
        source,
    })?;
    if !last.history_root.is_absolute() {
        return Err(failed(
            &path,
            io::ErrorKind::InvalidData,
            format!(
                "historyRoot {} is not an absolute folder",
                last.history_root.display()
            ),
        ));
    }
    Ok(Some(last.history_root))
}

fn remember_root(app: &AppPaths, root: &Path) -> Result<(), HistoryError> {
    let path = app.history_root_file();
    let last = LastRoot {
        history_root: root.to_path_buf(),
    };
    let text = serde_json::to_string(&last).map_err(|source| HistoryError::Json {
        path: path.clone(),
        line: 0,
        source,
    })?;
    atomic::ensure_private_dir(&app.data_dir).map_err(|source| HistoryError::Io {
        path: app.data_dir.clone(),
        source,
    })?;
    atomic::write_atomic(&path, text.as_bytes(), Access::Private)
        .map_err(|source| HistoryError::Io { path, source })
}

/// What [`merge_folder`] did.
pub(super) struct Merged {
    /// At least one file or line was written.
    pub changed: bool,
    /// One per copied line that is not valid JSON (kept as it is).
    pub warnings: Vec<String>,
}

/// Bring every file directly in `from_dir` into `to_dir`; `from_dir` is never
/// changed. A month file (`*.jsonl`) gets the lines it is missing
/// ([`merge_lines`]); any other file (`host.json`) is copied only when it is
/// missing. `sessions-now.json` is one host's live state, not history, and is
/// never copied (spec §3.8).
pub(super) fn merge_folder(from_dir: &Path, to_dir: &Path) -> Result<Merged, HistoryError> {
    let entries = fs::read_dir(from_dir).map_err(|source| HistoryError::Io {
        path: from_dir.to_path_buf(),
        source,
    })?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| HistoryError::Io {
            path: from_dir.to_path_buf(),
            source,
        })?;
        match entry.metadata() {
            Ok(metadata) if metadata.is_file() => files.push(entry.path()),
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(HistoryError::Io {
                    path: entry.path(),
                    source,
                })
            }
        }
    }
    // By name, so a copy is the same whatever order the file system lists in.
    files.sort();
    let mut merged = Merged {
        changed: false,
        warnings: Vec::new(),
    };
    for file in files {
        let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == SESSIONS_NOW_FILE {
            continue;
        }
        let target = to_dir.join(name);
        let changed = if name.ends_with(".jsonl") {
            merge_lines(&file, &target, &mut merged.warnings)?
        } else {
            copy_missing(&file, &target)?
        };
        merged.changed |= changed;
    }
    Ok(merged)
}

/// Whether `path` is there as a file; a folder in its place is an error.
fn file_there(path: &Path) -> Result<bool, HistoryError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(failed(
            path,
            io::ErrorKind::AlreadyExists,
            "exists but is not a file",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(HistoryError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Make the folder of `target`.
fn ensure_parent(target: &Path) -> Result<(), HistoryError> {
    let Some(dir) = target.parent() else {
        return Ok(());
    };
    atomic::ensure_private_dir(dir).map_err(|source| HistoryError::Io {
        path: dir.to_path_buf(),
        source,
    })
}

/// Copy `from` to `target` as it is, when `target` is not there.
fn copy_missing(from: &Path, target: &Path) -> Result<bool, HistoryError> {
    if file_there(target)? {
        return Ok(false);
    }
    let Some(bytes) = read_bytes(from)? else {
        return Ok(false);
    };
    ensure_parent(target)?;
    atomic::write_atomic(target, &bytes, Access::Private).map_err(|source| HistoryError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    Ok(true)
}

/// Add to `target` the lines of `from` it is missing. When one file's content
/// starts with the other's, the longer one is kept. Otherwise lines count as a
/// multiset: a line `k` times in `from` and `j < k` times in `target` is
/// appended `k − j` times, because identical records can repeat (several
/// lines per hour and model); lines already in `target` are never collapsed.
/// An empty line holds no record and is not copied. A copied line that is not
/// valid JSON is kept and named in `warnings`.
fn merge_lines(
    from: &Path,
    target: &Path,
    warnings: &mut Vec<String>,
) -> Result<bool, HistoryError> {
    let Some(theirs) = read_bytes(from)? else {
        return Ok(false);
    };
    let ours = if file_there(target)? {
        read_bytes(target)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    if ours.starts_with(&theirs) {
        return Ok(false);
    }
    let lines = file_lines(&theirs);
    if theirs.starts_with(&ours) && !ours.is_empty() && !ours.ends_with(b"\n") {
        // `target` ends inside a line that `from` completes: an append cannot
        // continue that line, so the longer file replaces it.
        let complete = ours.iter().filter(|byte| **byte == b'\n').count();
        for (index, line) in lines.iter().enumerate().skip(complete) {
            if let Some(warning) = not_json(from, index + 1, line, target) {
                warnings.push(warning);
            }
        }
        ensure_parent(target)?;
        atomic::write_atomic(target, &theirs, Access::Private).map_err(|source| {
            HistoryError::Io {
                path: target.to_path_buf(),
                source,
            }
        })?;
        return Ok(true);
    }

    let mut have: HashMap<&[u8], usize> = HashMap::new();
    for line in file_lines(&ours) {
        *have.entry(line).or_insert(0) += 1;
    }
    let mut missing = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        if let Some(count) = have.get_mut(line) {
            if *count > 0 {
                *count -= 1;
                continue;
            }
        }
        let number = index + 1;
        if line.len() + 1 > MAX_LINE_BYTES {
            return Err(failed(
                from,
                io::ErrorKind::InvalidData,
                format!(
                    "line {number} is more than {MAX_LINE_BYTES} bytes with its newline, so it \
                     cannot be added to {}",
                    target.display()
                ),
            ));
        }
        let text = match std::str::from_utf8(line) {
            Ok(text) => {
                if let Some(warning) = not_json(from, number, line, target) {
                    warnings.push(warning);
                }
                text.to_string()
            }
            Err(_) => {
                warnings.push(format!(
                    "{} line {number} is not valid UTF-8 text; it was copied to {} with the \
                     broken characters replaced",
                    from.display(),
                    target.display()
                ));
                String::from_utf8_lossy(line).into_owned()
            }
        };
        missing.push(text);
    }
    if missing.is_empty() {
        return Ok(false);
    }
    ensure_parent(target)?;
    atomic::append_lines(target, &missing, Access::Private).map_err(|source| HistoryError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    Ok(true)
}

/// The warning for a copied line that is not valid JSON, or `None` for one
/// that is.
fn not_json(from: &Path, number: usize, line: &[u8], target: &Path) -> Option<String> {
    if serde_json::from_slice::<serde_json::Value>(line).is_ok() {
        return None;
    }
    Some(format!(
        "{} line {number} is not valid JSON; it was copied to {} as it is",
        from.display(),
        target.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, Utc};
    use tempfile::tempdir;

    use crate::api::LimitKind;
    use crate::history::{
        AccountRef, Event, EventKind, Hosts, LimitSample, Range, Sample, Source, TokenRecord,
    };
    use crate::project::ProjectKey;

    const MACHINE: &str = "0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60";
    const OTHER_MACHINE: &str = "5d2f8a61-3c7e-4b19-a0d4-9e6b1c3f7a28";

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse::<DateTime<Utc>>().expect("test date")
    }

    fn account() -> AccountRef {
        AccountRef {
            acc: "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04".to_string(),
            org: "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20".to_string(),
        }
    }

    fn sample(t: &str, percent: f64) -> Sample {
        Sample {
            t: utc(t),
            account: account(),
            src: Source::Poll,
            l: vec![LimitSample {
                k: LimitKind::Session,
                m: None,
                p: percent,
                r: Some(utc("2026-11-14T19:30:00Z")),
                b: false,
            }],
        }
    }

    fn tokens(t: &str) -> TokenRecord {
        TokenRecord {
            t: utc(t),
            session: "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34".to_string(),
            cwd: "/home/sam/projects/atlas".to_string(),
            project: ProjectKey::Repo("github.com/sam/atlas".to_string()),
            model: "claude-opus-4-5-20251101".to_string(),
            input: 1_204,
            out: 3_877,
            cw: 18_442,
            cr: 412_090,
            answers: 6,
            agent_answers: 0,
        }
    }

    fn line<R: serde::Serialize>(record: &R) -> String {
        serde_json::to_string(record).expect("a record encodes")
    }

    /// A data folder with this machine's id, and the settings that name `new`
    /// as the history folder.
    struct Fixture {
        _tmp: tempfile::TempDir,
        app: AppPaths,
        old: PathBuf,
        new: PathBuf,
        settings: Settings,
    }

    fn fixture() -> Fixture {
        let tmp = tempdir().unwrap();
        let app = AppPaths::at(&tmp.path().join("data"));
        fs::create_dir_all(&app.data_dir).unwrap();
        fs::write(app.machine_id_file(), MACHINE).unwrap();
        let old = tmp.path().join("old");
        let new = tmp.path().join("Dropbox").join("seatswap");
        let settings = Settings {
            host_name: Some("atlas".to_string()),
            history_dir: Some(new.clone()),
            ..Settings::default()
        };
        Fixture {
            _tmp: tmp,
            app,
            old,
            new,
            settings,
        }
    }

    fn mark(root: &Path, machine_id: &str) {
        fs::create_dir_all(root.join("atlas")).unwrap();
        fs::write(
            root.join("atlas/host.json"),
            format!("{{\"machineId\":\"{machine_id}\"}}"),
        )
        .unwrap();
    }

    /// This host's history in `root`: marked, two samples, a switch, the live
    /// report.
    fn write_history(root: &Path) {
        mark(root, MACHINE);
        let dir = HistoryDir::at(root, "atlas");
        dir.append_many(&[
            sample("2026-10-30T09:00:00Z", 12.0),
            sample("2026-11-14T09:00:00Z", 41.0),
        ])
        .unwrap();
        dir.append(&Event {
            t: utc("2026-11-14T09:05:00Z"),
            kind: EventKind::AccountAdded {
                account: account(),
                plan: Some("max".to_string()),
            },
        })
        .unwrap();
        fs::write(
            dir.sessions_now_file("atlas"),
            "{\"at\":\"2026-11-14T09:05:00Z\",\"sessions\":[]}",
        )
        .unwrap();
    }

    /// Every file of one folder with its bytes, by name.
    fn folder_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.metadata().unwrap().is_file())
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    fn remembered(app: &AppPaths) -> Option<PathBuf> {
        read_last_root(app).unwrap()
    }

    #[test]
    fn copies_into_an_absent_folder_and_leaves_the_old_one_unchanged() {
        let f = fixture();
        write_history(&f.old);
        remember_root(&f.app, &f.old).unwrap();
        let before = folder_files(&f.old.join("atlas"));
        assert!(bring_along_due(&f.app, &f.settings).unwrap());

        let warnings = bring_along(&f.app, &f.settings).unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(folder_files(&f.old.join("atlas")), before, "old unchanged");
        let copied: Vec<_> = before
            .into_iter()
            .filter(|(name, _)| name != SESSIONS_NOW_FILE)
            .collect();
        assert_eq!(folder_files(&f.new.join("atlas")), copied);
        assert_eq!(remembered(&f.app), Some(f.new.clone()));
        // Parsed, so a Windows path's escaped backslashes compare as the path.
        let file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(f.app.history_root_file()).unwrap()).unwrap();
        assert_eq!(
            file,
            serde_json::json!({ "historyRoot": f.new.to_str().unwrap() })
        );
        assert!(!bring_along_due(&f.app, &f.settings).unwrap());
        // The folder opens, and the window reads the old records there.
        let dir = HistoryDir::open(&f.app, &f.settings).unwrap();
        let read = dir
            .read::<Sample>(&Range::ALL, &Hosts::One("atlas".to_string()))
            .unwrap();
        assert_eq!(read.records.len(), 2);
    }

    #[test]
    fn a_prefix_file_takes_the_longer_one() {
        let f = fixture();
        write_history(&f.old);
        remember_root(&f.app, &f.old).unwrap();
        // The new folder has the first sample only (an earlier move, or a
        // sync that stopped), and a samples file that is longer than the old
        // one's of October.
        mark(&f.new, MACHINE);
        let november = "samples-2026-11.jsonl";
        let october = "samples-2026-10.jsonl";
        let old_november = fs::read(f.old.join("atlas").join(november)).unwrap();
        fs::write(f.new.join("atlas").join(november), b"").unwrap();
        let longer = format!(
            "{}\n{}\n",
            line(&sample("2026-10-30T09:00:00Z", 12.0)),
            line(&sample("2026-10-30T10:00:00Z", 13.0))
        );
        fs::write(f.new.join("atlas").join(october), &longer).unwrap();

        bring_along(&f.app, &f.settings).unwrap();

        assert_eq!(
            fs::read(f.new.join("atlas").join(november)).unwrap(),
            old_november
        );
        assert_eq!(
            fs::read_to_string(f.new.join("atlas").join(october)).unwrap(),
            longer,
            "the longer file here stays as it is"
        );
    }

    #[test]
    fn a_file_torn_inside_a_line_is_completed_from_the_longer_one() {
        let f = fixture();
        write_history(&f.old);
        remember_root(&f.app, &f.old).unwrap();
        mark(&f.new, MACHINE);
        let name = "samples-2026-11.jsonl";
        let full = fs::read(f.old.join("atlas").join(name)).unwrap();
        fs::write(f.new.join("atlas").join(name), &full[..full.len() / 2]).unwrap();

        bring_along(&f.app, &f.settings).unwrap();

        assert_eq!(fs::read(f.new.join("atlas").join(name)).unwrap(), full);
    }

    #[test]
    fn two_divergent_files_keep_every_line_counted_as_a_multiset() {
        let f = fixture();
        mark(&f.old, MACHINE);
        mark(&f.new, MACHINE);
        remember_root(&f.app, &f.old).unwrap();
        let name = "tokens-2026-11.jsonl";
        let a = line(&tokens("2026-11-14T09:00:00Z"));
        let b = line(&tokens("2026-11-14T10:00:00Z"));
        let c = line(&tokens("2026-11-14T11:00:00Z"));
        let d = line(&tokens("2026-11-14T12:00:00Z"));
        let torn = "{\"t\":\"2026-11-14T13:00:00Z\",\"session\":\"7c1e";
        // Old: a, b twice (two identical lines of one hour and model), c and
        // a torn line. New: a, d twice, b once.
        fs::write(
            f.old.join("atlas").join(name),
            format!("{a}\n{b}\n{b}\n{c}\n{torn}\n"),
        )
        .unwrap();
        fs::write(
            f.new.join("atlas").join(name),
            format!("{a}\n{d}\n{d}\n{b}\n"),
        )
        .unwrap();
        let before = folder_files(&f.old.join("atlas"));

        let warnings = bring_along(&f.app, &f.settings).unwrap();

        // New's lines stay as they are, d twice included; old's missing lines
        // follow: b once more (twice there, once here), c, and the torn line.
        assert_eq!(
            fs::read_to_string(f.new.join("atlas").join(name)).unwrap(),
            format!("{a}\n{d}\n{d}\n{b}\n{b}\n{c}\n{torn}\n")
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("line 5 is not valid JSON")
                && warnings[0].contains(&f.old.join("atlas").join(name).display().to_string()),
            "{}",
            warnings[0]
        );
        assert_eq!(folder_files(&f.old.join("atlas")), before, "old unchanged");

        // A reader gets them in time order whatever their order in the file.
        let read = HistoryDir::at(&f.new, "atlas")
            .read::<TokenRecord>(&Range::ALL, &Hosts::One("atlas".to_string()))
            .unwrap();
        let times: Vec<_> = read.records.iter().map(|hosted| hosted.record.t).collect();
        let mut sorted = times.clone();
        sorted.sort();
        assert_eq!(times, sorted);
        assert_eq!(times.len(), 6);

        // A second run adds nothing.
        remember_root(&f.app, &f.old).unwrap();
        let again = folder_files(&f.new.join("atlas"));
        bring_along(&f.app, &f.settings).unwrap();
        assert_eq!(folder_files(&f.new.join("atlas")), again);
    }

    #[test]
    fn a_folder_of_another_machine_refuses_and_changes_nothing() {
        let f = fixture();
        write_history(&f.old);
        remember_root(&f.app, &f.old).unwrap();
        mark(&f.new, OTHER_MACHINE);
        let vega_line = format!("{}\n", line(&sample("2026-11-14T08:00:00Z", 3.0)));
        fs::write(f.new.join("atlas/samples-2026-11.jsonl"), &vega_line).unwrap();
        let before_new = folder_files(&f.new.join("atlas"));
        let before_old = folder_files(&f.old.join("atlas"));

        let warnings = bring_along(&f.app, &f.settings).unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(folder_files(&f.new.join("atlas")), before_new);
        assert_eq!(folder_files(&f.old.join("atlas")), before_old);
        assert_eq!(
            remembered(&f.app),
            Some(f.old.clone()),
            "tried again at the next start"
        );
        assert!(matches!(
            HistoryDir::open(&f.app, &f.settings),
            Err(HistoryError::HostTaken { .. })
        ));
    }

    #[test]
    fn a_folder_without_host_json_is_claimed_and_merged() {
        let f = fixture();
        write_history(&f.old);
        remember_root(&f.app, &f.old).unwrap();
        fs::create_dir_all(f.new.join("atlas")).unwrap();
        let here = format!("{}\n", line(&sample("2026-11-14T08:00:00Z", 30.0)));
        fs::write(f.new.join("atlas/samples-2026-11.jsonl"), &here).unwrap();

        bring_along(&f.app, &f.settings).unwrap();

        assert_eq!(
            fs::read_to_string(f.new.join("atlas/host.json")).unwrap(),
            format!("{{\"machineId\":\"{MACHINE}\"}}")
        );
        let merged = fs::read_to_string(f.new.join("atlas/samples-2026-11.jsonl")).unwrap();
        assert!(merged.starts_with(&here), "{merged}");
        assert_eq!(merged.lines().count(), 2);
        assert!(f.new.join("atlas/samples-2026-10.jsonl").is_file());
        assert_eq!(remembered(&f.app), Some(f.new.clone()));
    }

    #[test]
    fn a_first_run_brings_the_history_from_the_data_folder() {
        let f = fixture();
        // No history-root.json yet: the history was in `<data folder>/history`.
        write_history(&f.app.history_dir());
        let before = folder_files(&f.app.history_dir().join("atlas"));

        bring_along(&f.app, &f.settings).unwrap();

        assert!(f.new.join("atlas/samples-2026-10.jsonl").is_file());
        assert!(f.new.join("atlas/events-2026-11.jsonl").is_file());
        assert_eq!(
            folder_files(&f.app.history_dir().join("atlas")),
            before,
            "old unchanged"
        );
        assert_eq!(remembered(&f.app), Some(f.new.clone()));
    }

    #[test]
    fn a_first_run_on_the_default_folder_only_remembers_it() {
        let f = fixture();
        write_history(&f.app.history_dir());
        let settings = Settings {
            host_name: Some("atlas".to_string()),
            ..Settings::default()
        };
        assert!(bring_along_due(&f.app, &settings).unwrap());

        bring_along(&f.app, &settings).unwrap();

        assert_eq!(remembered(&f.app), Some(f.app.history_dir()));
        assert!(!f.new.exists());
        assert!(!bring_along_due(&f.app, &settings).unwrap());
    }

    #[test]
    fn an_unchanged_root_does_nothing() {
        let f = fixture();
        write_history(&f.old);
        remember_root(&f.app, &f.new).unwrap();
        let file = fs::read(f.app.history_root_file()).unwrap();

        assert!(!bring_along_due(&f.app, &f.settings).unwrap());
        assert!(bring_along(&f.app, &f.settings).unwrap().is_empty());

        assert!(!f.new.exists(), "nothing is copied");
        assert_eq!(fs::read(f.app.history_root_file()).unwrap(), file);
        assert!(!f.app.switch_lock().exists(), "no lock is taken");
    }

    #[test]
    fn an_old_folder_of_another_machine_is_not_brought() {
        let f = fixture();
        mark(&f.old, OTHER_MACHINE);
        HistoryDir::at(&f.old, "atlas")
            .append(&sample("2026-11-14T08:00:00Z", 3.0))
            .unwrap();
        remember_root(&f.app, &f.old).unwrap();
        let before = folder_files(&f.old.join("atlas"));

        let warnings = bring_along(&f.app, &f.settings).unwrap();

        assert!(!f.new.join("atlas").exists(), "nothing is copied");
        assert_eq!(folder_files(&f.old.join("atlas")), before, "old unchanged");
        assert_eq!(remembered(&f.app), Some(f.new.clone()), "said once");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let warning = &warnings[0];
        for part in [
            f.old.join("atlas").display().to_string(),
            OTHER_MACHINE.to_string(),
            MACHINE.to_string(),
            "was not brought to".to_string(),
            f.app.history_root_file().display().to_string(),
        ] {
            assert!(warning.contains(&part), "{part} missing in: {warning}");
        }
        assert!(bring_along(&f.app, &f.settings).unwrap().is_empty());

        // The steps the warning names bring it along.
        mark(&f.old, MACHINE);
        fs::write(
            f.app.history_root_file(),
            format!(
                "{{\"historyRoot\":{}}}",
                serde_json::Value::String(f.old.display().to_string())
            ),
        )
        .unwrap();
        assert!(bring_along(&f.app, &f.settings).unwrap().is_empty());
        assert!(f.new.join("atlas/samples-2026-11.jsonl").is_file());
        assert_eq!(remembered(&f.app), Some(f.new.clone()));
    }

    #[test]
    fn a_broken_remembered_root_is_an_error_naming_the_file() {
        let f = fixture();
        fs::write(f.app.history_root_file(), "{\"historyRoot\":\"history\"}").unwrap();
        let error = bring_along_due(&f.app, &f.settings).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains(&f.app.history_root_file().display().to_string())
                && text.contains("historyRoot history is not an absolute folder"),
            "{text}"
        );
    }

    #[test]
    fn the_import_merges_two_files_that_both_changed() {
        let here = tempdir().unwrap();
        let there = tempdir().unwrap();
        for root in [here.path(), there.path()] {
            fs::create_dir_all(root.join("vega")).unwrap();
            fs::write(
                root.join("vega/host.json"),
                format!("{{\"machineId\":\"{OTHER_MACHINE}\"}}"),
            )
            .unwrap();
        }
        let a = line(&sample("2026-11-14T07:00:00Z", 5.0));
        let b = line(&sample("2026-11-14T08:00:00Z", 6.0));
        let c = line(&sample("2026-11-14T09:00:00Z", 7.0));
        fs::write(
            here.path().join("vega/samples-2026-11.jsonl"),
            format!("{a}\n{b}\n"),
        )
        .unwrap();
        fs::write(
            there.path().join("vega/samples-2026-11.jsonl"),
            format!("{a}\n{c}\n"),
        )
        .unwrap();

        let imported = HistoryDir::at(here.path(), "atlas")
            .import(there.path())
            .unwrap();

        assert_eq!(imported.copied, vec!["vega"]);
        assert!(imported.warnings.is_empty());
        assert_eq!(
            fs::read_to_string(here.path().join("vega/samples-2026-11.jsonl")).unwrap(),
            format!("{a}\n{b}\n{c}\n"),
            "neither side's line is lost"
        );
    }
}
