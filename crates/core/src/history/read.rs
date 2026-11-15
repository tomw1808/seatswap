//! Reading a range from the history folder: the hosts asked for, the month
//! files that overlap the range, and a bad line skipped with a warning. A torn
//! line is always only a warning; other broken lines are an error only when they
//! are more than 1 % of the file's lines.
//!
//! The same reader answers what the hosts hold ([`HistoryDir::host_infos`]),
//! copies another history folder's host folders in ([`HistoryDir::import`]) and
//! writes the export ([`export_csv`], [`export_json`]). The export is raw data:
//! accounts by the server's ids, projects by the key as stored.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use chrono::{DateTime, NaiveDate, SecondsFormat, TimeZone, Utc};
use serde::Serialize;

use super::{
    read_marker, same_reset, Event, HistoryDir, HistoryError, Kind, Record, Sample, SessionRecord,
    TokenRecord, UpRecord, WindowRecord,
};
use crate::atomic::{self, Access};
use crate::paths;
use crate::project::ProjectKey;
use crate::projects::ProjectsLogRecord;

/// A time range: `from` inclusive, `to` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl Range {
    /// The range between two optional ends: an open start is
    /// [`DateTime::<Utc>::MIN_UTC`], an open end is [`DateTime::<Utc>::MAX_UTC`].
    /// A start that is not before the end is [`HistoryError::Range`], naming
    /// both.
    pub fn between(
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Range, HistoryError> {
        let from = from.unwrap_or(DateTime::<Utc>::MIN_UTC);
        let to = to.unwrap_or(DateTime::<Utc>::MAX_UTC);
        if from >= to {
            return Err(HistoryError::Range { from, to });
        }
        Ok(Range { from, to })
    }
}

/// Which hosts to read: every one in the folder, or one named host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hosts {
    All,
    One(String),
}

/// One record and the host whose folder held it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hosted<R> {
    pub host: String,
    pub record: R,
}

/// What an import did: the hosts at least one file was copied for, and the
/// hosts it refused, each with the reason. Both are sorted by host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub copied: Vec<String>,
    pub conflicts: Vec<HostConflict>,
}

/// A host folder the import did not copy, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostConflict {
    pub host: String,
    pub reason: String,
}

/// What a range read found, sorted by [`Record::at`] and then by host.
#[derive(Debug, Clone, PartialEq)]
pub struct Read<R> {
    pub records: Vec<Hosted<R>>,
    pub warnings: Vec<String>,
}

impl HistoryDir {
    pub fn read<R: Record>(&self, range: &Range, hosts: &Hosts) -> Result<Read<R>, HistoryError> {
        let mut records = Vec::new();
        let mut warnings = Vec::new();
        for host in self.hosts_to_read(hosts)? {
            let dir = self.root().join(&host);
            for file in month_files(&dir, R::KIND.stem(), range)? {
                let path = dir.join(&file);
                read_file::<R>(&path, &host, &file, range, &mut records, &mut warnings)?;
            }
        }
        records.sort_by(|a, b| {
            a.record
                .at()
                .cmp(&b.record.at())
                .then_with(|| a.host.cmp(&b.host))
        });
        Ok(Read { records, warnings })
    }

    /// The window records of the range, one per (account, kind, end within
    /// `same_reset`): the one with the latest `last_sample` counts (spec §1.5).
    /// On a tie the host name that sorts first wins, so the result does not
    /// depend on read order. Sorted by end, then account.
    pub fn finished_windows(
        &self,
        range: &Range,
        hosts: &Hosts,
    ) -> Result<Vec<Hosted<WindowRecord>>, HistoryError> {
        let read = self.read::<WindowRecord>(range, hosts)?;
        let mut windows: Vec<Hosted<WindowRecord>> = Vec::new();
        for hosted in read.records {
            let same = windows.iter().position(|kept| {
                kept.record.account == hosted.record.account
                    && kept.record.k == hosted.record.k
                    && same_reset(kept.record.end, hosted.record.end)
            });
            let Some(index) = same else {
                windows.push(hosted);
                continue;
            };
            let kept = &windows[index];
            let later = hosted.record.last_sample > kept.record.last_sample
                || (hosted.record.last_sample == kept.record.last_sample
                    && hosted.host < kept.host);
            if later {
                windows[index] = hosted;
            }
        }
        windows.sort_by(|a, b| {
            a.record
                .end
                .cmp(&b.record.end)
                .then_with(|| a.record.account.cmp(&b.record.account))
        });
        Ok(windows)
    }

    /// `Hosts::All` is every folder with a `host.json` plus this dir's own host
    /// (once each); `Hosts::One` is that folder only, and its name must pass
    /// `paths::host_name`'s rule.
    fn hosts_to_read(&self, hosts: &Hosts) -> Result<Vec<String>, HistoryError> {
        match hosts {
            Hosts::One(host) => Ok(vec![paths::host_name(Some(host))?]),
            Hosts::All => {
                let mut list = self.hosts()?;
                if !list.iter().any(|host| host == self.host()) {
                    list.push(self.host().to_string());
                }
                Ok(list)
            }
        }
    }
}

/// The month files of one kind in one host folder whose month overlaps the
/// range, by name, sorted. Only a name of exactly `<stem>-YYYY-MM.jsonl` is a
/// month file; a conflict copy or anything else is ignored.
fn month_files(dir: &Path, stem: &str, range: &Range) -> Result<Vec<String>, HistoryError> {
    if range.from >= range.to {
        return Ok(Vec::new());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(HistoryError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| HistoryError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let is_file = entry
            .metadata()
            .map_err(|source| HistoryError::Io {
                path: entry.path(),
                source,
            })?
            .is_file();
        if !is_file {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some((year, month)) = month_of_name(name, stem) else {
            continue;
        };
        if month_overlaps(year, month, range) {
            files.push(name.to_string());
        }
    }
    files.sort();
    Ok(files)
}

/// The months of `kind`'s files in one host folder, newest first, each as the
/// half-open range of its UTC month. The files that exist bound the list; a
/// name that is not exactly `<stem>-YYYY-MM.jsonl` is not a month file.
pub(super) fn months_of_kind(dir: &Path, kind: Kind) -> Result<Vec<Range>, HistoryError> {
    let every_month = Range {
        from: DateTime::<Utc>::MIN_UTC,
        to: DateTime::<Utc>::MAX_UTC,
    };
    let mut months = Vec::new();
    for file in month_files(dir, kind.stem(), &every_month)? {
        let Some((year, month)) = month_of_name(&file, kind.stem()) else {
            continue;
        };
        let (Some(from), Some(to)) = (month_start(year, month), next_month_start(year, month))
        else {
            continue;
        };
        months.push(Range { from, to });
    }
    months.sort_by_key(|range| std::cmp::Reverse(range.from));
    Ok(months)
}

/// The file of `kind` for one month, if it exists. The month range comes from
/// [`months_of_kind`].
fn file_of_month(
    dir: &Path,
    kind: Kind,
    month: &Range,
) -> Result<Option<std::path::PathBuf>, HistoryError> {
    let mut files = month_files(dir, kind.stem(), month)?;
    Ok(files.pop().map(|name| dir.join(name)))
}

/// `(year, month)` of a file named `<stem>-YYYY-MM.jsonl`, or `None` for any
/// other shape (including a month outside 1..=12).
fn month_of_name(name: &str, stem: &str) -> Option<(i32, u32)> {
    let rest = name
        .strip_prefix(stem)?
        .strip_prefix('-')?
        .strip_suffix(".jsonl")?;
    if rest.len() != 7 {
        return None;
    }
    let (year, month) = rest.split_once('-')?;
    if year.len() != 4
        || month.len() != 2
        || !year.bytes().all(|byte| byte.is_ascii_digit())
        || !month.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let year = year.parse::<i32>().ok()?;
    let month = month.parse::<u32>().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

/// Whether the UTC month `year-month` shares any moment with the range.
fn month_overlaps(year: i32, month: u32, range: &Range) -> bool {
    let Some(start) = month_start(year, month) else {
        return false;
    };
    let Some(next) = next_month_start(year, month) else {
        return false;
    };
    start < range.to && next > range.from
}

fn next_month_start(year: i32, month: u32) -> Option<DateTime<Utc>> {
    if month == 12 {
        month_start(year + 1, 1)
    } else {
        month_start(year, month + 1)
    }
}

pub(super) fn month_start(year: i32, month: u32) -> Option<DateTime<Utc>> {
    NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| Utc.from_utc_datetime(&naive))
}

/// A history file's bytes, or `None` when it is not there (another host may
/// have removed it). Every history read gets its bytes here.
fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, HistoryError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(HistoryError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The lines of a history file: split on `\n`, the empty piece after a final
/// newline dropped. One torn line must not lose the complete ones around it.
fn file_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if bytes.last() == Some(&b'\n') {
        lines.pop();
    }
    lines
}

fn read_file<R: Record>(
    path: &Path,
    host: &str,
    file: &str,
    range: &Range,
    records: &mut Vec<Hosted<R>>,
    warnings: &mut Vec<String>,
) -> Result<(), HistoryError> {
    let Some(bytes) = read_bytes(path)? else {
        return Ok(());
    };

    let lines = file_lines(&bytes);
    let total = lines.len();
    let mut counted = 0usize;
    let mut first_counted: Option<(usize, serde_json::Error)> = None;
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        if line.is_empty() {
            continue;
        }
        let text = match std::str::from_utf8(line) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() => {
                // The line ends inside a multi-byte character: it was cut off
                // while being written.
                warnings.push(format!("{host}/{file} line {number}: {error}"));
                continue;
            }
            Err(error) => {
                warnings.push(format!("{host}/{file} line {number}: {error}"));
                counted += 1;
                if first_counted.is_none() {
                    first_counted = Some((
                        number,
                        serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, error)),
                    ));
                }
                continue;
            }
        };
        match serde_json::from_str::<R>(text) {
            Ok(record) => {
                let at = record.at();
                if at >= range.from && at < range.to {
                    records.push(Hosted {
                        host: host.to_string(),
                        record,
                    });
                }
            }
            Err(source) => {
                warnings.push(format!("{host}/{file} line {number}: {source}"));
                // End of input is a torn line; a data error is valid JSON that
                // is not a record of this kind (a newer event type, or a record
                // before a downgrade). Both are only warnings.
                if !source.is_eof() && !source.is_data() {
                    counted += 1;
                    if first_counted.is_none() {
                        first_counted = Some((number, source));
                    }
                }
            }
        }
    }
    if let Some((line, source)) = first_counted {
        // More than 1 % of the file's lines: counted * 100 > total.
        if counted as u64 * 100 > total as u64 {
            return Err(HistoryError::Json {
                path: path.to_path_buf(),
                line,
                source,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The hosts of a history folder (spec §1.5, §3.7)
// ---------------------------------------------------------------------------

/// One host folder under the history root, as the Settings page shows it.
/// "Since" is the first record of the oldest file of each kind; `last_record`
/// is the newest `up` or `samples` line, which is written while the app runs, so
/// it says when the host was last heard of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub host: String,
    pub this_host: bool,
    pub first_record: Option<DateTime<Utc>>,
    pub last_record: Option<DateTime<Utc>>,
    pub bytes: u64,
}

impl HistoryDir {
    /// One entry per host folder under the root, this host's own folder
    /// included even when it is still empty, sorted by host name.
    pub fn host_infos(&self) -> Result<Vec<HostInfo>, HistoryError> {
        let mut hosts = self.hosts_to_read(&Hosts::All)?;
        hosts.sort();
        let mut infos = Vec::with_capacity(hosts.len());
        for host in hosts {
            infos.push(self.host_info(&host)?);
        }
        Ok(infos)
    }

    fn host_info(&self, host: &str) -> Result<HostInfo, HistoryError> {
        let firsts = [
            self.first_of_kind::<UpRecord>(host)?,
            self.first_of_kind::<Sample>(host)?,
            self.first_of_kind::<WindowRecord>(host)?,
            self.first_of_kind::<Event>(host)?,
            self.first_of_kind::<TokenRecord>(host)?,
            self.first_of_kind::<SessionRecord>(host)?,
            self.first_of_kind::<ProjectsLogRecord>(host)?,
        ];
        let lasts = [
            self.newest_of_kind::<UpRecord>(host)?,
            self.newest_of_kind::<Sample>(host)?,
        ];
        Ok(HostInfo {
            host: host.to_string(),
            this_host: host == self.host(),
            first_record: firsts.into_iter().flatten().min(),
            last_record: lasts.into_iter().flatten().max(),
            bytes: folder_bytes(&self.root().join(host))?,
        })
    }

    /// The first record that parses in the oldest month file of one kind. A
    /// line that does not parse is skipped, as [`HistoryDir::read`] skips a torn
    /// one: the table never fails on a record it does not need.
    fn first_of_kind<R: Record>(&self, host: &str) -> Result<Option<DateTime<Utc>>, HistoryError> {
        let dir = self.root().join(host);
        let Some(month) = months_of_kind(&dir, R::KIND)?.last().copied() else {
            return Ok(None);
        };
        let Some(path) = file_of_month(&dir, R::KIND, &month)? else {
            return Ok(None);
        };
        let Some(bytes) = read_bytes(&path)? else {
            return Ok(None);
        };
        for line in file_lines(&bytes) {
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            if let Ok(record) = serde_json::from_str::<R>(text) {
                return Ok(Some(record.at()));
            }
        }
        Ok(None)
    }

    /// The newest record of one kind in its newest month file.
    fn newest_of_kind<R: Record>(&self, host: &str) -> Result<Option<DateTime<Utc>>, HistoryError> {
        let dir = self.root().join(host);
        let Some(month) = months_of_kind(&dir, R::KIND)?.first().copied() else {
            return Ok(None);
        };
        let Some(path) = file_of_month(&dir, R::KIND, &month)? else {
            return Ok(None);
        };
        let Some(bytes) = read_bytes(&path)? else {
            return Ok(None);
        };
        let mut newest = None;
        for line in file_lines(&bytes) {
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            if let Ok(record) = serde_json::from_str::<R>(text) {
                let at = record.at();
                if newest.is_none_or(|newest| at > newest) {
                    newest = Some(at);
                }
            }
        }
        Ok(newest)
    }

    /// Copy every file of another history folder's host folders that is missing
    /// here or longer there; a file that is longer here is kept. This host's own
    /// folder is never read or written: a host only ever writes its own folder.
    /// A host folder that is not here yet is copied as it is; one that is here
    /// only when both folders' `host.json` name this machine (otherwise it is a
    /// conflict, never a merge). A read or write error stops the whole import;
    /// a conflict does not.
    pub fn import(&self, from: &Path) -> Result<Imported, HistoryError> {
        match fs::metadata(from) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(HistoryError::NoFolder {
                    path: from.to_path_buf(),
                })
            }
            Err(source) => {
                return Err(HistoryError::Io {
                    path: from.to_path_buf(),
                    source,
                })
            }
        }
        if same_folder(from, self.root()) {
            return Err(failed(
                from,
                io::ErrorKind::InvalidInput,
                "that is this history folder",
            ));
        }
        let source = HistoryDir::at(from, self.host());
        let hosts = source.host_folders()?;
        if hosts.is_empty() {
            return Err(failed(
                from,
                io::ErrorKind::NotFound,
                format!("no host folder found in {}", from.display()),
            ));
        }
        let mut imported = Imported {
            copied: Vec::new(),
            conflicts: Vec::new(),
        };
        for host in hosts {
            if host == self.host() {
                continue;
            }
            match self.copy_host(from, &host)? {
                HostImport::Copied => imported.copied.push(host),
                HostImport::Unchanged => {}
                HostImport::Conflict(conflict) => imported.conflicts.push(conflict),
            }
        }
        imported.copied.sort();
        imported.conflicts.sort_by(|a, b| a.host.cmp(&b.host));
        Ok(imported)
    }

    /// Copy the files of one other host's folder in.
    fn copy_host(&self, from: &Path, host: &str) -> Result<HostImport, HistoryError> {
        let from_dir = from.join(host);
        let to_dir = self.root().join(host);
        // A folder here that belongs to another machine is never touched: two
        // machines with the same host name would otherwise merge their files.
        // A folder that is not here yet is copied without asking (an old host
        // may have no marker).
        if folder_exists(&to_dir)? {
            let here = read_marker(&to_dir.join("host.json"))?;
            let there = read_marker(&from_dir.join("host.json"))?;
            match (here, there) {
                (Some(here), Some(there)) if here.machine_id == there.machine_id => {}
                (Some(_), Some(_)) => {
                    return Ok(HostImport::Conflict(HostConflict {
                        host: host.to_string(),
                        reason: format!(
                            "two machines use the host name {host}; give one of them another name \
                             under History and hosts"
                        ),
                    }))
                }
                (None, _) => {
                    return Ok(HostImport::Conflict(HostConflict {
                        host: host.to_string(),
                        reason: cannot_tell(host, from, &to_dir),
                    }))
                }
                (_, None) => {
                    return Ok(HostImport::Conflict(HostConflict {
                        host: host.to_string(),
                        reason: cannot_tell(host, from, &from_dir),
                    }))
                }
            }
        }
        let entries = fs::read_dir(&from_dir).map_err(|source| HistoryError::Io {
            path: from_dir.clone(),
            source,
        })?;
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| HistoryError::Io {
                path: from_dir.clone(),
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
        let mut copied = false;
        for file in files {
            let Some(name) = file.file_name() else {
                continue;
            };
            let target = to_dir.join(name);
            let there = fs::metadata(&file)
                .map_err(|source| HistoryError::Io {
                    path: file.clone(),
                    source,
                })?
                .len();
            let here = match fs::metadata(&target) {
                Ok(metadata) if metadata.is_file() => Some(metadata.len()),
                Ok(_) => {
                    return Err(failed(
                        &target,
                        io::ErrorKind::AlreadyExists,
                        "exists but is not a file",
                    ))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(HistoryError::Io {
                        path: target,
                        source,
                    })
                }
            };
            if here.is_some_and(|length| length >= there) {
                continue;
            }
            let bytes = fs::read(&file).map_err(|source| HistoryError::Io {
                path: file.clone(),
                source,
            })?;
            atomic::ensure_private_dir(&to_dir).map_err(|source| HistoryError::Io {
                path: to_dir.clone(),
                source,
            })?;
            atomic::write_atomic(&target, &bytes, Access::Private).map_err(|source| {
                HistoryError::Io {
                    path: target.clone(),
                    source,
                }
            })?;
            copied = true;
        }
        Ok(if copied {
            HostImport::Copied
        } else {
            HostImport::Unchanged
        })
    }
}

/// What copying one host's folder did.
enum HostImport {
    Copied,
    Unchanged,
    Conflict(HostConflict),
}

/// Whether a host folder is there, as a folder. A path that is there but is not
/// a folder cannot hold a host's files.
fn folder_exists(dir: &Path) -> Result<bool, HistoryError> {
    match fs::metadata(dir) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(failed(
            dir,
            io::ErrorKind::AlreadyExists,
            "exists but is not a folder",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(HistoryError::Io {
            path: dir.to_path_buf(),
            source,
        }),
    }
}

/// The import's conflict reason when one side of a host folder has no
/// `host.json`: nothing proves the two folders are the same machine.
fn cannot_tell(host: &str, from: &Path, folder: &Path) -> String {
    format!(
        "cannot tell whether {host} here and in {} are the same machine: {} has no host.json",
        from.display(),
        folder.display()
    )
}

/// The sum of the sizes of the files directly in one host folder; a folder that
/// is not there yet is 0 bytes.
fn folder_bytes(dir: &Path) -> Result<u64, HistoryError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(HistoryError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    let mut bytes = 0u64;
    for entry in entries {
        let entry = entry.map_err(|source| HistoryError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(HistoryError::Io {
                    path: entry.path(),
                    source,
                })
            }
        };
        if !metadata.is_file() {
            continue;
        }
        bytes = bytes.checked_add(metadata.len()).ok_or_else(|| {
            failed(
                dir,
                io::ErrorKind::InvalidData,
                "the files add up to more than 2^64 bytes",
            )
        })?;
    }
    Ok(bytes)
}

/// Whether two paths name the same folder. Both are canonicalized when they
/// exist, so a trailing slash, `..` or a link does not hide the same folder.
fn same_folder(first: &Path, second: &Path) -> bool {
    match (fs::canonicalize(first), fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => first == second,
    }
}

/// An error about one path with our own text, in the shape the history's other
/// errors have.
fn failed(path: &Path, kind: io::ErrorKind, text: impl Into<String>) -> HistoryError {
    HistoryError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(kind, text.into()),
    }
}

// ---------------------------------------------------------------------------
// The export (spec §3.7; step C1 task 7, decision 3)
// ---------------------------------------------------------------------------

/// The export's columns, in this order: the CSV header, and the fields the JSON
/// objects carry (camelCase there).
const EXPORT_COLUMNS: [&str; 14] = [
    "kind",
    "account",
    "org",
    "limit",
    "model",
    "start",
    "end",
    "percent",
    "day_utc",
    "project",
    "input",
    "output",
    "cache_write",
    "cache_read",
];

/// One `window` line: a finished window as its record holds it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowRow {
    kind: &'static str,
    account: String,
    org: String,
    limit: String,
    model: Option<String>,
    start: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    percent: f64,
}

impl WindowRow {
    fn of(record: WindowRecord) -> WindowRow {
        WindowRow {
            kind: "window",
            account: record.account.acc,
            org: record.account.org,
            limit: record.k.as_api().to_string(),
            model: record.m,
            start: record.start,
            end: record.end,
            percent: record.final_percent,
        }
    }

    /// This row's fields in [`EXPORT_COLUMNS`]' order.
    fn fields(&self) -> [String; 14] {
        [
            self.kind.to_string(),
            self.account.clone(),
            self.org.clone(),
            self.limit.clone(),
            self.model.clone().unwrap_or_default(),
            self.start.map(time_text).unwrap_or_default(),
            time_text(self.end),
            number_text(self.percent),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ]
    }
}

/// One `tokens` line: every token record of one UTC day and one project key,
/// every host, added up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenDayRow {
    kind: &'static str,
    day_utc: NaiveDate,
    project: String,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
}

impl TokenDayRow {
    fn of(day_utc: NaiveDate, project: ProjectKey, counts: [u64; 4]) -> TokenDayRow {
        let [input, output, cache_write, cache_read] = counts;
        TokenDayRow {
            kind: "tokens",
            day_utc,
            project: project_text(&project),
            input,
            output,
            cache_write,
            cache_read,
        }
    }

    /// This row's fields in [`EXPORT_COLUMNS`]' order.
    fn fields(&self) -> [String; 14] {
        [
            self.kind.to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            self.day_utc.to_string(),
            self.project.clone(),
            self.input.to_string(),
            self.output.to_string(),
            self.cache_write.to_string(),
            self.cache_read.to_string(),
        ]
    }
}

/// The months of `kind` that hold records of `range`, every host, in time
/// order and clipped to the range. The files that exist bound the list, so an
/// open end never walks the calendar; each month is read through
/// [`HistoryDir::read`], the one range reader.
fn months_in(history: &HistoryDir, kind: Kind, range: &Range) -> Result<Vec<Range>, HistoryError> {
    if range.from >= range.to {
        return Ok(Vec::new());
    }
    let mut months = Vec::new();
    for host in history.hosts_to_read(&Hosts::All)? {
        let dir = history.root().join(host);
        for month in months_of_kind(&dir, kind)? {
            if month.from < range.to && month.to > range.from {
                months.push(Range {
                    from: month.from.max(range.from),
                    to: month.to.min(range.to),
                });
            }
        }
    }
    months.sort_by_key(|month| month.from);
    months.dedup();
    Ok(months)
}

/// A write error of an export, named with the history folder.
fn export_io(history: &HistoryDir, source: io::Error) -> HistoryError {
    HistoryError::Io {
        path: history.root().to_path_buf(),
        source,
    }
}

/// One month's token rows: the token records of the month and the range, every
/// host, summed per UTC day and project key and sorted by them.
fn token_rows_of_month(
    history: &HistoryDir,
    month: &Range,
) -> Result<Vec<TokenDayRow>, HistoryError> {
    let mut sums: BTreeMap<(NaiveDate, ProjectKey), [u64; 4]> = BTreeMap::new();
    for hosted in history.read::<TokenRecord>(month, &Hosts::All)?.records {
        let record = hosted.record;
        let day = record.t.date_naive();
        let project = project_text(&record.project);
        let total = sums.entry((day, record.project)).or_insert([0; 4]);
        for (sum, value) in total
            .iter_mut()
            .zip([record.input, record.out, record.cw, record.cr])
        {
            *sum = sum.checked_add(value).ok_or_else(|| {
                failed(
                    history.root(),
                    io::ErrorKind::InvalidData,
                    format!("the token counts of {day} in {project} are too large to add up"),
                )
            })?;
        }
    }
    Ok(sums
        .into_iter()
        .map(|((day, project), counts)| TokenDayRow::of(day, project, counts))
        .collect())
}

/// One row of the export, in the order both formats write them: every window
/// first, then the token rows.
enum ExportRow {
    Window(WindowRow),
    Tokens(TokenDayRow),
}

/// The export's rows, made once for both writers: the windows of the range
/// (one per window, the latest `last_sample` winning; spec §1.5), then the
/// token rows month by month, so memory holds one month of token records plus
/// the rows. `emit` writes each row as it arrives.
fn export_rows(
    history: &HistoryDir,
    range: &Range,
    mut emit: impl FnMut(ExportRow) -> Result<(), HistoryError>,
) -> Result<(), HistoryError> {
    for hosted in history.finished_windows(range, &Hosts::All)? {
        emit(ExportRow::Window(WindowRow::of(hosted.record)))?;
    }
    for month in months_in(history, Kind::Tokens, range)? {
        for row in token_rows_of_month(history, &month)? {
            emit(ExportRow::Tokens(row))?;
        }
    }
    Ok(())
}

/// The export as CSV: one header line, one `window` line per window of the
/// range, one `tokens` line per UTC day and project key, every host. Fields are
/// quoted RFC 4180 style; a field that does not apply to a line is empty.
pub fn export_csv(
    history: &HistoryDir,
    range: &Range,
    out: &mut dyn Write,
) -> Result<(), HistoryError> {
    out.write_all(EXPORT_COLUMNS.join(",").as_bytes())
        .map_err(|source| export_io(history, source))?;
    out.write_all(b"\n")
        .map_err(|source| export_io(history, source))?;
    export_rows(history, range, |row| {
        let line = match row {
            ExportRow::Window(row) => csv_line(&row.fields()),
            ExportRow::Tokens(row) => csv_line(&row.fields()),
        };
        out.write_all(line.as_bytes())
            .map_err(|source| export_io(history, source))
    })
}

/// The same rows as [`export_csv`], as one JSON object
/// `{"windows":[…],"dailyTokens":[…]}`. The rows are written as they are made.
pub fn export_json(
    history: &HistoryDir,
    range: &Range,
    out: &mut dyn Write,
) -> Result<(), HistoryError> {
    let encode = |error: serde_json::Error| {
        failed(
            history.root(),
            io::ErrorKind::InvalidData,
            format!("cannot encode the export: {error}"),
        )
    };
    out.write_all(b"{\"windows\":[")
        .map_err(|source| export_io(history, source))?;
    let mut first = true;
    let mut in_tokens = false;
    export_rows(history, range, |row| match row {
        ExportRow::Window(row) => {
            if !first {
                out.write_all(b",")
                    .map_err(|source| export_io(history, source))?;
            }
            first = false;
            serde_json::to_writer(&mut *out, &row).map_err(encode)
        }
        ExportRow::Tokens(row) => {
            if !in_tokens {
                out.write_all(b"],\"dailyTokens\":[")
                    .map_err(|source| export_io(history, source))?;
                in_tokens = true;
                first = true;
            }
            if !first {
                out.write_all(b",")
                    .map_err(|source| export_io(history, source))?;
            }
            first = false;
            serde_json::to_writer(&mut *out, &row).map_err(encode)
        }
    })?;
    if !in_tokens {
        out.write_all(b"],\"dailyTokens\":[")
            .map_err(|source| export_io(history, source))?;
    }
    out.write_all(b"]}\n")
        .map_err(|source| export_io(history, source))
}

/// A project key as the export writes it: the key as stored, `repo:<id>` or
/// `folder:<id>`. A name the projects log gives it is never used: an export is
/// raw data, and a name changes.
fn project_text(key: &ProjectKey) -> String {
    match key {
        ProjectKey::Repo(id) => format!("repo:{id}"),
        ProjectKey::Folder(id) => format!("folder:{id}"),
    }
}

/// One CSV line, its fields quoted RFC 4180 style when they hold a comma, a
/// quote or a line break.
fn csv_line(fields: &[String]) -> String {
    let mut line = String::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        let needs_quotes = field.contains(',')
            || field.contains('"')
            || field.contains('\n')
            || field.contains('\r');
        if needs_quotes {
            line.push('"');
            line.push_str(&field.replace('"', "\"\""));
            line.push('"');
        } else {
            line.push_str(field);
        }
    }
    line.push('\n');
    line
}

/// A moment in the export: UTC, RFC 3339, with the sub-second digits only when
/// the record has them.
fn time_text(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

/// A number in the export, without a trailing `.0` for a whole percentage.
fn number_text(value: f64) -> String {
    format!("{value}")
}
