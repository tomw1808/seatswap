//! Reading a range from the history folder: the hosts asked for, the month
//! files that overlap the range, and a bad line skipped with a warning. A torn
//! line is always only a warning; other broken lines are an error only when they
//! are more than 1 % of the file's lines.

use std::fs;
use std::io;
use std::path::Path;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use super::{HistoryDir, HistoryError, Kind, Record};
use crate::paths;

/// A time range: `from` inclusive, `to` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
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

fn read_file<R: Record>(
    path: &Path,
    host: &str,
    file: &str,
    range: &Range,
    records: &mut Vec<Hosted<R>>,
    warnings: &mut Vec<String>,
) -> Result<(), HistoryError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(HistoryError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };

    // Split the bytes on '\n' and decode every line on its own: one torn line
    // must not lose the complete ones around it.
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if bytes.last() == Some(&b'\n') {
        lines.pop();
    }
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
