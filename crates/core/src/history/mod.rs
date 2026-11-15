//! The history folder (spec §1): the record types, appending them, reading a
//! range and merging several hosts. One subfolder per host, one JSON Lines
//! file per kind and month; [`read`](HistoryDir::read) owns the reading side.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::api::LimitKind;
use crate::atomic::{self, Access};
use crate::paths::{self, AppPaths, PathsError};
use crate::project::ProjectKey;
use crate::store::{Account, Settings};

mod read;
mod write;

pub use read::{Hosted, Hosts, Range, Read};
pub use write::{closed_windows, sample_of, Recorder};

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} line {line}: {source}")]
    Json {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "host {host}: {folder}/host.json names another machine id than this machine's ({machine_id}).\n\
         If another computer writes there, set a different hostName in the settings; if this computer lost its id, delete that host.json."
    )]
    HostTaken {
        host: String,
        folder: PathBuf,
        machine_id: PathBuf,
    },
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error("a {kind:?} record is too long: its line with the newline is more than 4096 bytes")]
    LineTooLong { kind: Kind },
    #[error("a {kind:?} record holds a number that is not finite")]
    NotFinite { kind: Kind },
}

/// `LimitKind` as the history writes it: the plain text the server sent
/// (`session`, `weekly_all`, `weekly_scoped`, or an unknown kind as itself),
/// never the object that `api::LimitKind`'s own serde form makes of
/// `LimitKind::Other` for `usage-cache.json`.
mod limit_kind {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::api::LimitKind;

    pub fn serialize<S: Serializer>(kind: &LimitKind, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(kind.as_api())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<LimitKind, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(LimitKind::from_api(&text))
    }
}

/// The six kinds of history line, each with its own file per month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Samples,
    Windows,
    Events,
    Tokens,
    Sessions,
    Up,
}

impl Kind {
    fn stem(self) -> &'static str {
        match self {
            Kind::Samples => "samples",
            Kind::Windows => "windows",
            Kind::Events => "events",
            Kind::Tokens => "tokens",
            Kind::Sessions => "sessions",
            Kind::Up => "up",
        }
    }
}

/// One line of one history file.
pub trait Record: Serialize + DeserializeOwned {
    const KIND: Kind;
    fn at(&self) -> DateTime<Utc>;
}

/// An account by the ids the server uses, never by seatswap's local id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountRef {
    pub acc: String,
    pub org: String,
}

impl AccountRef {
    /// The one way to name a stored account in the history.
    pub fn of(account: &Account) -> AccountRef {
        AccountRef {
            acc: account.account_uuid.clone(),
            org: account.organization_uuid.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Poll,
    Statusline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitSample {
    #[serde(with = "limit_kind")]
    pub k: LimitKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub m: Option<String>,
    pub p: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub b: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub t: DateTime<Utc>,
    #[serde(flatten)]
    pub account: AccountRef,
    pub src: Source,
    pub l: Vec<LimitSample>,
}

impl Record for Sample {
    const KIND: Kind = Kind::Samples;

    fn at(&self) -> DateTime<Utc> {
        self.t
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowRecord {
    #[serde(flatten)]
    pub account: AccountRef,
    #[serde(with = "limit_kind")]
    pub k: LimitKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub m: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<DateTime<Utc>>,
    pub end: DateTime<Utc>,
    #[serde(rename = "final")]
    pub final_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost: Option<f64>,
    #[serde(rename = "lastSample")]
    pub last_sample: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub uncertain: bool,
}

impl Record for WindowRecord {
    const KIND: Kind = Kind::Windows;

    fn at(&self) -> DateTime<Utc> {
        self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum By {
    User,
    Cli,
    Auto,
    Agent,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCounts {
    pub running: u32,
    pub busy: u32,
    /// How many sessions run on a model whose thinking is tied to the account
    /// (spec §2.11). Step B measures it; until then it is `None` and left out
    /// of the file. An old line with `"tied":0` still reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tied: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EventKind {
    Switch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<AccountRef>,
        to: AccountRef,
        by: By,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sessions: Option<SessionCounts>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waited_secs: Option<u64>,
    },
    Blocked {
        account: AccountRef,
        #[serde(with = "limit_kind")]
        k: LimitKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        m: Option<String>,
    },
    Unblocked {
        account: AccountRef,
        #[serde(with = "limit_kind")]
        k: LimitKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        m: Option<String>,
    },
    AllBlocked,
    AnyUsable,
    AccountAdded {
        account: AccountRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<String>,
    },
    AccountRemoved {
        account: AccountRef,
    },
    PlanChanged {
        account: AccountRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
    LoginEnded {
        account: AccountRef,
    },
    LoginRenewed {
        account: AccountRef,
    },
    Live {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<AccountRef>,
    },
    AgentCall {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        allowed: bool,
    },
    Waited {
        session: String,
        project: ProjectKey,
        secs: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub t: DateTime<Utc>,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Record for Event {
    const KIND: Kind = Kind::Events;

    fn at(&self) -> DateTime<Utc> {
        self.t
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRecord {
    pub t: DateTime<Utc>,
    pub session: String,
    pub cwd: String,
    pub project: ProjectKey,
    pub model: String,
    #[serde(rename = "in")]
    pub input: u64,
    pub out: u64,
    pub cw: u64,
    pub cr: u64,
    pub answers: u32,
    pub agent_answers: u32,
}

impl Record for TokenRecord {
    const KIND: Kind = Kind::Tokens;

    fn at(&self) -> DateTime<Utc> {
        self.t
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub t: DateTime<Utc>,
    pub session: String,
    pub cwd: String,
    pub project: ProjectKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub agents_started: u32,
    pub agents_finished: u32,
    pub closed: bool,
}

impl Record for SessionRecord {
    const KIND: Kind = Kind::Sessions;

    fn at(&self) -> DateTime<Utc> {
        self.t
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpRecord {
    pub t: DateTime<Utc>,
}

impl Record for UpRecord {
    const KIND: Kind = Kind::Up;

    fn at(&self) -> DateTime<Utc> {
        self.t
    }
}

/// One host's history folder: `<root>/<host>/`, shared with other hosts by
/// pointing `historyDir` at one synced folder on every machine.
pub struct HistoryDir {
    root: PathBuf,
    host: String,
}

impl HistoryDir {
    /// Root = `settings.historyDir` or `app.history_dir()`; host =
    /// `paths::host_name(settings.host_name)`. Creates `<root>/<host>/` and its
    /// `host.json` marker with this machine's id (created on first use) and
    /// refuses a folder marked with another machine's id.
    pub fn open(app: &AppPaths, settings: &Settings) -> Result<HistoryDir, HistoryError> {
        let root = paths::history_root(settings.history_dir.as_deref(), app)?;
        let host = paths::host_name(settings.host_name.as_deref())?;
        let history = HistoryDir { root, host };

        let host_dir = history.root.join(&history.host);
        atomic::ensure_private_dir(&host_dir).map_err(|source| HistoryError::Io {
            path: host_dir.clone(),
            source,
        })?;
        let machine_id_file = app.machine_id_file();
        let machine_id = read_or_create_machine_id(app)?;
        history.check_or_write_marker(
            &host_dir.join("host.json"),
            &machine_id,
            &machine_id_file,
        )?;
        Ok(history)
    }

    /// A history folder without the marker check (tests).
    pub fn at(root: &Path, host: &str) -> HistoryDir {
        HistoryDir {
            root: root.to_path_buf(),
            host: host.to_string(),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The subfolders that hold a `host.json`, sorted. A name that is not a
    /// valid, already-lower-case host name (a sync service's conflict copy) is
    /// not a host.
    pub fn hosts(&self) -> Result<Vec<String>, HistoryError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(HistoryError::Io {
                    path: self.root.clone(),
                    source,
                })
            }
        };
        let mut hosts = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| HistoryError::Io {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            let is_dir = match fs::metadata(&path) {
                Ok(metadata) => metadata.is_dir(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(HistoryError::Io {
                        path: path.clone(),
                        source,
                    })
                }
            };
            if !is_dir {
                continue;
            }
            let marker = path.join("host.json");
            let has_marker = match fs::metadata(&marker) {
                Ok(metadata) => metadata.is_file(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(HistoryError::Io {
                        path: marker,
                        source,
                    })
                }
            };
            if !has_marker {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            // The name of a host folder follows `paths::host_name`'s rule and is
            // already in the lower case that rule produces; a sync service's
            // "vega (conflicted copy)" folder is not a host.
            if matches!(paths::host_name(Some(name)), Ok(known) if known == name) {
                hosts.push(name.to_string());
            }
        }
        hosts.sort();
        Ok(hosts)
    }

    /// Append one record to `<root>/<own host>/<stem>-YYYY-MM.jsonl`, the month
    /// of `record.at()` in UTC. A line over [`atomic::MAX_LINE_BYTES`] is an
    /// error before the folder or file is made.
    pub fn append<R: Record>(&self, record: &R) -> Result<(), HistoryError> {
        let host_dir = self.root.join(&self.host);
        let name = format!("{}-{}.jsonl", R::KIND.stem(), record.at().format("%Y-%m"));
        let path = host_dir.join(&name);
        let line = serde_json::to_string(record).map_err(|e| HistoryError::Io {
            path: path.clone(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot encode a {} record: {e}", R::KIND.stem()),
            ),
        })?;
        // Every optional field is left out when it is `None`, so a `null` in the
        // serialized record can only be a non-finite number, which would read
        // back as a wrong value.
        let value: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| HistoryError::Io {
                path: path.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "cannot read back the encoded {} record: {e}",
                        R::KIND.stem()
                    ),
                ),
            })?;
        if holds_null(&value) {
            return Err(HistoryError::NotFinite { kind: R::KIND });
        }
        if line.len() + 1 > atomic::MAX_LINE_BYTES {
            return Err(HistoryError::LineTooLong { kind: R::KIND });
        }
        atomic::ensure_private_dir(&host_dir).map_err(|source| HistoryError::Io {
            path: host_dir,
            source,
        })?;
        atomic::append_line(&path, &line, Access::Private)
            .map_err(|source| HistoryError::Io { path, source })
    }

    fn check_or_write_marker(
        &self,
        marker: &Path,
        machine_id: &str,
        machine_id_file: &Path,
    ) -> Result<(), HistoryError> {
        let text = match fs::read_to_string(marker) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let text = format!("{{\"machineId\":\"{machine_id}\"}}");
                return atomic::write_atomic(marker, text.as_bytes(), Access::Private).map_err(
                    |source| HistoryError::Io {
                        path: marker.to_path_buf(),
                        source,
                    },
                );
            }
            Err(source) => {
                return Err(HistoryError::Io {
                    path: marker.to_path_buf(),
                    source,
                })
            }
        };
        let held: HostMarker =
            serde_json::from_str(&text).map_err(|source| HistoryError::Json {
                path: marker.to_path_buf(),
                line: source.line(),
                source,
            })?;
        if held.machine_id.trim() != machine_id {
            return Err(HistoryError::HostTaken {
                host: self.host.clone(),
                folder: self.root.join(&self.host),
                machine_id: machine_id_file.to_path_buf(),
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct HostMarker {
    #[serde(rename = "machineId")]
    machine_id: String,
}

fn read_or_create_machine_id(app: &AppPaths) -> Result<String, HistoryError> {
    let path = app.machine_id_file();
    match fs::read_to_string(&path) {
        Ok(text) => parse_machine_id(&path, text.trim()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            atomic::ensure_private_dir(&app.data_dir).map_err(|source| HistoryError::Io {
                path: app.data_dir.clone(),
                source,
            })?;
            let id = uuid::Uuid::new_v4().to_string();
            atomic::write_atomic(&path, id.as_bytes(), Access::Private).map_err(|source| {
                HistoryError::Io {
                    path: path.clone(),
                    source,
                }
            })?;
            // Two programs starting for the first time can each write an id;
            // the file holds the one that won. Read it back and use that.
            let text = fs::read_to_string(&path).map_err(|source| HistoryError::Io {
                path: path.clone(),
                source,
            })?;
            parse_machine_id(&path, text.trim())
        }
        Err(source) => Err(HistoryError::Io { path, source }),
    }
}

fn parse_machine_id(path: &Path, text: &str) -> Result<String, HistoryError> {
    match uuid::Uuid::parse_str(text) {
        Ok(id) => Ok(id.to_string()),
        Err(_) => Err(HistoryError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{text:?} is not a uuid; delete {} and seatswap makes a new one",
                    path.display()
                ),
            ),
        }),
    }
}

/// A reset time rounded up to the full minute: the server sends a moment just
/// before the boundary (`00:59:59.59`), which would otherwise read `00:59`
/// instead of `01:00`. An exact minute stays as it is.
pub fn reset_minute(resets_at: DateTime<Utc>) -> DateTime<Utc> {
    let into_minute = resets_at.timestamp().rem_euclid(60) * 1_000_000_000
        + i64::from(resets_at.timestamp_subsec_nanos());
    if into_minute == 0 {
        resets_at
    } else {
        resets_at + chrono::Duration::nanoseconds(60_000_000_000 - into_minute)
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Whether a serialized record holds a `null` anywhere. All optional fields of
/// a record are omitted when they are `None`, so the only `null` serde_json can
/// put into a record is a non-finite `f64` (`NaN`, infinity).
fn holds_null(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Array(items) => items.iter().any(holds_null),
        serde_json::Value::Object(fields) => fields.values().any(holds_null),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::io::Write;

    use chrono::{DateTime, Utc};
    use serde_json::Value;
    use tempfile::tempdir;

    use crate::api::LimitKind;
    use crate::paths::AppPaths;
    use crate::store::Settings;

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse::<DateTime<Utc>>().expect("test date")
    }

    fn atlas_account() -> AccountRef {
        AccountRef {
            acc: "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04".to_string(),
            org: "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20".to_string(),
        }
    }

    fn a_sample(t: DateTime<Utc>, percent: f64) -> Sample {
        Sample {
            t,
            account: atlas_account(),
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

    fn wide_range() -> Range {
        Range {
            from: utc("2026-01-01T00:00:00Z"),
            to: utc("2027-01-01T00:00:00Z"),
        }
    }

    #[test]
    fn a_sample_round_trips_with_the_short_keys() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        let sample = Sample {
            t: utc("2026-11-14T14:02:11Z"),
            account: atlas_account(),
            src: Source::Poll,
            l: vec![
                LimitSample {
                    k: LimitKind::Session,
                    m: None,
                    p: 38.0,
                    r: Some(utc("2026-11-14T19:30:00Z")),
                    b: false,
                },
                LimitSample {
                    k: LimitKind::WeeklyAll,
                    m: None,
                    p: 75.0,
                    r: Some(utc("2026-11-16T16:00:00Z")),
                    b: true,
                },
                LimitSample {
                    k: LimitKind::WeeklyScoped,
                    m: Some("Fable".to_string()),
                    p: 21.0,
                    r: Some(utc("2026-11-16T16:00:00Z")),
                    b: false,
                },
            ],
        };
        history.append(&sample).unwrap();

        let text = fs::read_to_string(dir.path().join("atlas/samples-2026-11.jsonl")).unwrap();
        let line = text.lines().next().unwrap();
        let value: Value = serde_json::from_str(line).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["t", "acc", "org", "src", "l"]);
        assert_eq!(value["l"][1]["b"], Value::Bool(true));
        assert!(value["l"][0].get("b").is_none());
        assert!(value["l"][0].get("m").is_none());

        let read = history.read::<Sample>(&wide_range(), &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.records[0].record, sample);
    }

    #[test]
    fn an_event_has_its_type_inline() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        let event = Event {
            t: utc("2026-11-14T14:02:11Z"),
            kind: EventKind::Switch {
                from: None,
                to: atlas_account(),
                by: By::Agent,
                reason: None,
                cwd: Some("/home/sam/projects/ledger".to_string()),
                sessions: Some(SessionCounts {
                    running: 2,
                    busy: 1,
                    tied: None,
                }),
                waited_secs: None,
            },
        };
        history.append(&event).unwrap();

        let text = fs::read_to_string(dir.path().join("atlas/events-2026-11.jsonl")).unwrap();
        assert!(text.contains("\"type\":\"switch\""));
        assert!(text.contains("\"by\":\"agent\""));

        let read = history.read::<Event>(&wide_range(), &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 1);
    }

    #[test]
    fn a_tied_count_is_left_out_until_it_is_known() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        let event = Event {
            t: utc("2026-11-14T14:02:11Z"),
            kind: EventKind::Switch {
                from: None,
                to: atlas_account(),
                by: By::User,
                reason: None,
                cwd: None,
                sessions: Some(SessionCounts {
                    running: 2,
                    busy: 1,
                    tied: None,
                }),
                waited_secs: None,
            },
        };
        history.append(&event).unwrap();
        let path = dir.path().join("atlas/events-2026-11.jsonl");
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("\"tied\""),
            "a count step B has not measured is not written: {text}"
        );

        // A line written by an older seatswap with `"tied":0` still reads.
        let old = format!(
            "{{\"t\":\"2026-11-14T14:03:11Z\",\"type\":\"switch\",\"to\":{},\"by\":\"user\",\"sessions\":{{\"running\":2,\"busy\":1,\"tied\":0}}}}\n",
            serde_json::to_string(&atlas_account()).unwrap()
        );
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(old.as_bytes()).unwrap();
        drop(file);

        let read = history.read::<Event>(&wide_range(), &Hosts::All).unwrap();
        let tied: Vec<Option<u32>> = read
            .records
            .iter()
            .filter_map(|hosted| match &hosted.record.kind {
                EventKind::Switch {
                    sessions: Some(counts),
                    ..
                } => Some(counts.tied),
                _ => None,
            })
            .collect();
        assert_eq!(tied, vec![None, Some(0)]);
    }

    #[test]
    fn records_go_to_the_month_of_their_time() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&a_sample(utc("2026-10-31T23:59:59Z"), 10.0))
            .unwrap();
        history
            .append(&a_sample(utc("2026-11-01T00:00:00Z"), 11.0))
            .unwrap();

        let october = dir.path().join("atlas/samples-2026-10.jsonl");
        let november = dir.path().join("atlas/samples-2026-11.jsonl");
        assert_eq!(fs::read_to_string(&october).unwrap().lines().count(), 1);
        assert_eq!(fs::read_to_string(&november).unwrap().lines().count(), 1);
    }

    #[test]
    fn read_returns_only_the_range() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        for hour in 10..=12 {
            history
                .append(&a_sample(
                    utc(&format!("2026-11-14T{hour}:00:00Z")),
                    hour as f64,
                ))
                .unwrap();
        }

        let range = Range {
            from: utc("2026-11-14T10:30:00Z"),
            to: utc("2026-11-14T12:00:00Z"),
        };
        let read = history.read::<Sample>(&range, &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.records[0].record.t, utc("2026-11-14T11:00:00Z"));
    }

    #[test]
    fn read_merges_hosts_in_time_order() {
        let dir = tempdir().unwrap();
        let atlas = HistoryDir::at(dir.path(), "atlas");
        atlas
            .append(&a_sample(utc("2026-11-14T10:00:00Z"), 10.0))
            .unwrap();
        atlas
            .append(&a_sample(utc("2026-11-14T12:00:00Z"), 12.0))
            .unwrap();
        let vega = HistoryDir::at(dir.path(), "vega");
        vega.append(&a_sample(utc("2026-11-14T11:00:00Z"), 11.0))
            .unwrap();
        fs::write(
            dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .unwrap();

        let all = atlas.read::<Sample>(&wide_range(), &Hosts::All).unwrap();
        let hosts: Vec<&str> = all.records.iter().map(|h| h.host.as_str()).collect();
        assert_eq!(hosts, vec!["atlas", "vega", "atlas"]);

        let one = atlas
            .read::<Sample>(&wide_range(), &Hosts::One("vega".to_string()))
            .unwrap();
        assert_eq!(one.records.len(), 1);
        assert_eq!(one.records[0].host, "vega");
    }

    #[test]
    fn a_torn_last_line_is_skipped_with_a_warning() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&a_sample(utc("2026-11-14T10:00:00Z"), 10.0))
            .unwrap();
        history
            .append(&a_sample(utc("2026-11-14T11:00:00Z"), 11.0))
            .unwrap();
        let path = dir.path().join("atlas/samples-2026-11.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"t\":\"2026-11-14T1").unwrap();
        drop(file);

        let read = history.read::<Sample>(&wide_range(), &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 2);
        assert_eq!(read.warnings.len(), 1);
    }

    #[test]
    fn open_refuses_a_host_folder_of_another_machine() {
        let app_dir = tempdir().unwrap();
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("atlas")).unwrap();
        fs::write(
            root.path().join("atlas/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .unwrap();

        let settings = Settings {
            history_dir: Some(root.path().to_path_buf()),
            host_name: Some("atlas".to_string()),
            ..Settings::default()
        };
        let app = AppPaths::at(app_dir.path());
        assert!(matches!(
            HistoryDir::open(&app, &settings),
            Err(HistoryError::HostTaken { .. })
        ));
    }

    #[test]
    fn open_creates_the_marker_once() {
        let app_dir = tempdir().unwrap();
        let root = tempdir().unwrap();
        let settings = Settings {
            history_dir: Some(root.path().to_path_buf()),
            host_name: Some("atlas".to_string()),
            ..Settings::default()
        };
        let app = AppPaths::at(app_dir.path());

        HistoryDir::open(&app, &settings).unwrap();
        let marker = fs::read(root.path().join("atlas/host.json")).unwrap();
        let machine = fs::read(app.machine_id_file()).unwrap();

        HistoryDir::open(&app, &settings).unwrap();
        assert_eq!(
            fs::read(root.path().join("atlas/host.json")).unwrap(),
            marker
        );
        assert_eq!(fs::read(app.machine_id_file()).unwrap(), machine);
    }

    #[test]
    fn append_rejects_a_record_over_the_line_limit() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        let event = Event {
            t: utc("2026-11-14T14:02:11Z"),
            kind: EventKind::AgentCall {
                command: "x".repeat(5000),
                cwd: None,
                allowed: true,
            },
        };
        assert!(matches!(
            history.append(&event),
            Err(HistoryError::LineTooLong { kind: Kind::Events })
        ));
        assert!(!dir.path().join("atlas").exists());
    }

    #[test]
    fn reset_minute_rounds_up() {
        assert_eq!(
            reset_minute(utc("2026-11-14T00:59:59.59Z")),
            utc("2026-11-14T01:00:00Z")
        );
        assert_eq!(
            reset_minute(utc("2026-11-14T01:00:00Z")),
            utc("2026-11-14T01:00:00Z")
        );
    }

    #[test]
    fn a_torn_line_inside_the_file_is_only_a_warning() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        for hour in 10..=12 {
            history
                .append(&a_sample(
                    utc(&format!("2026-11-14T{hour}:00:00Z")),
                    hour as f64,
                ))
                .unwrap();
        }
        let path = dir.path().join("atlas/samples-2026-11.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"t\":\"2026-11-14T1").unwrap();
        drop(file);
        history
            .append(&a_sample(utc("2026-11-14T13:00:00Z"), 13.0))
            .unwrap();

        let read = history.read::<Sample>(&wide_range(), &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 4);
        assert_eq!(read.warnings.len(), 1);
    }

    #[test]
    fn a_line_torn_inside_a_character_is_only_a_warning() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&a_sample(utc("2026-11-14T10:00:00Z"), 10.0))
            .unwrap();
        let path = dir.path().join("atlas/samples-2026-11.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"t\":\"x\",\"cwd\":\"/home/sam/M\xc3")
            .unwrap();
        drop(file);

        let read = history.read::<Sample>(&wide_range(), &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.warnings.len(), 1);
    }

    #[test]
    fn an_unknown_event_type_is_only_a_warning() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        fs::create_dir_all(dir.path().join("atlas")).unwrap();
        let lines = [
            "{\"t\":\"2026-11-14T10:00:00Z\",\"type\":\"priceChanged\",\"amount\":180.0}",
            "{\"t\":\"2026-11-14T10:00:00Z\",\"type\":\"priceChanged\",\"amount\":180.0}",
            "{\"t\":\"2026-11-14T10:00:00Z\",\"type\":\"priceChanged\",\"amount\":180.0}",
            "{\"t\":\"2026-11-14T10:00:00Z\",\"type\":\"allBlocked\"}",
        ];
        fs::write(
            dir.path().join("atlas/events-2026-11.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .unwrap();

        let read = history.read::<Event>(&wide_range(), &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.warnings.len(), 3);
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        fs::create_dir_all(dir.path().join("atlas")).unwrap();
        let good = serde_json::to_string(&a_sample(utc("2026-11-14T10:00:00Z"), 10.0)).unwrap();
        let mut lines = vec![good; 10];
        lines[3] = "{\"t\":]".to_string();
        lines[7] = "{\"t\":]".to_string();
        fs::write(
            dir.path().join("atlas/samples-2026-11.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .unwrap();

        assert!(matches!(
            history.read::<Sample>(&wide_range(), &Hosts::All),
            Err(HistoryError::Json { line: 4, .. })
        ));
    }

    #[test]
    fn append_refuses_a_number_that_is_not_finite() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");

        let mut sample = a_sample(utc("2026-11-14T10:00:00Z"), 10.0);
        sample.l[0].p = f64::NAN;
        assert!(matches!(
            history.append(&sample),
            Err(HistoryError::NotFinite {
                kind: Kind::Samples
            })
        ));
        assert!(!dir.path().join("atlas").exists());

        let window = WindowRecord {
            account: atlas_account(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(utc("2026-11-09T16:00:00Z")),
            end: utc("2026-11-16T16:00:00Z"),
            final_percent: 87.0,
            lost: Some(f64::INFINITY),
            last_sample: utc("2026-11-16T15:58:10Z"),
            plan: None,
            uncertain: false,
        };
        assert!(matches!(
            history.append(&window),
            Err(HistoryError::NotFinite {
                kind: Kind::Windows
            })
        ));
        assert!(!dir.path().join("atlas").exists());
    }

    #[test]
    fn an_unknown_limit_kind_is_a_plain_string() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        let mut sample = a_sample(utc("2026-11-14T10:00:00Z"), 5.0);
        sample.l[0].k = LimitKind::Other("weekly_opus".to_string());
        history.append(&sample).unwrap();

        let text = fs::read_to_string(dir.path().join("atlas/samples-2026-11.jsonl")).unwrap();
        assert!(text.contains("\"k\":\"weekly_opus\""));

        let read = history.read::<Sample>(&wide_range(), &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.records[0].record, sample);
    }

    #[test]
    fn host_taken_says_how_to_fix_it() {
        let app_dir = tempdir().unwrap();
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("atlas")).unwrap();
        fs::write(
            root.path().join("atlas/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .unwrap();

        let settings = Settings {
            history_dir: Some(root.path().to_path_buf()),
            host_name: Some("atlas".to_string()),
            ..Settings::default()
        };
        let app = AppPaths::at(app_dir.path());
        let error = match HistoryDir::open(&app, &settings) {
            Ok(_) => panic!("the host folder belongs to another machine"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(text.contains("hostName"), "{text}");
        assert!(text.contains("host.json"), "{text}");
    }

    #[test]
    fn read_ignores_conflict_copies_and_reads_a_far_range_quickly() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&a_sample(utc("2026-11-14T10:00:00Z"), 10.0))
            .unwrap();

        let conflict_host = dir.path().join("vega (conflicted copy)");
        fs::create_dir_all(&conflict_host).unwrap();
        fs::write(
            conflict_host.join("host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .unwrap();
        fs::write(
            conflict_host.join("samples-2026-11.jsonl"),
            format!(
                "{}\n",
                serde_json::to_string(&a_sample(utc("2026-11-14T11:00:00Z"), 11.0)).unwrap()
            ),
        )
        .unwrap();
        fs::write(
            dir.path()
                .join("atlas/samples-2026-11 (conflicted copy).jsonl"),
            format!(
                "{}\n",
                serde_json::to_string(&a_sample(utc("2026-11-14T12:00:00Z"), 12.0)).unwrap()
            ),
        )
        .unwrap();

        let range = Range {
            from: utc("1970-01-01T00:00:00Z"),
            to: DateTime::<Utc>::MAX_UTC,
        };
        let read = history.read::<Sample>(&range, &Hosts::All).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.records[0].record.t, utc("2026-11-14T10:00:00Z"));
        assert!(read.warnings.is_empty());
    }

    #[test]
    fn a_host_name_of_dots_is_refused() {
        let dir = tempdir().unwrap();
        let history = HistoryDir::at(dir.path(), "atlas");
        assert!(matches!(
            history.read::<Sample>(&wide_range(), &Hosts::One("..".to_string())),
            Err(HistoryError::Paths(_))
        ));
    }

    #[test]
    fn a_machine_id_file_without_a_uuid_says_how_to_fix_it() {
        let app_dir = tempdir().unwrap();
        let root = tempdir().unwrap();
        let settings = Settings {
            history_dir: Some(root.path().to_path_buf()),
            host_name: Some("atlas".to_string()),
            ..Settings::default()
        };
        let app = AppPaths::at(app_dir.path());
        fs::write(app.machine_id_file(), "not-a-uuid\n").unwrap();

        let error = match HistoryDir::open(&app, &settings) {
            Ok(_) => panic!("the machine-id file does not hold a uuid"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(text.contains("delete"), "{text}");
        assert!(
            text.contains(&app.machine_id_file().display().to_string()),
            "{text}"
        );
    }

    #[test]
    fn the_machine_id_used_is_the_one_in_the_file() {
        let app_dir = tempdir().unwrap();
        let root = tempdir().unwrap();
        let settings = Settings {
            history_dir: Some(root.path().to_path_buf()),
            host_name: Some("atlas".to_string()),
            ..Settings::default()
        };
        let app = AppPaths::at(app_dir.path());
        let id = "0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60";
        fs::write(app.machine_id_file(), format!("{id}\n")).unwrap();

        HistoryDir::open(&app, &settings).unwrap();
        let marker: Value =
            serde_json::from_str(&fs::read_to_string(root.path().join("atlas/host.json")).unwrap())
                .unwrap();
        assert_eq!(marker["machineId"], Value::String(id.to_string()));
    }
}
