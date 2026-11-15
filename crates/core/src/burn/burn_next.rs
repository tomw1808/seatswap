//! `burn-next.json`: the account auto-burn plans to switch to next and when,
//! for the status line (spec §7.6). The tray app writes it; the status line
//! reads it without a lock.

use std::fs;
use std::io;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::atomic::{self, write_atomic, Access};
use crate::paths::AppPaths;

use super::FRESH_MIN;

/// The tray app rewrites the file at least this often, so a `written` older
/// than [`FRESH_MIN`] means the tray app is not running. Half the limit leaves
/// room for one missed tick.
pub const REWRITE_AFTER_MIN: i64 = FRESH_MIN / 2;

/// What the status line shows of auto-burn: the next account and the time its
/// switch is planned, plus the moment the tray app wrote this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BurnNext {
    /// The stored account's id.
    pub account: String,
    /// When the switch is planned, UTC.
    pub at: DateTime<Utc>,
    /// When the tray app wrote this, UTC.
    pub written: DateTime<Utc>,
}

/// Write `<data>/burn-next.json` atomically, mode 0600.
pub fn write(app: &AppPaths, next: &BurnNext) -> io::Result<()> {
    atomic::ensure_private_dir(&app.data_dir)?;
    let bytes = serde_json::to_vec_pretty(next)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(&app.burn_next_file(), &bytes, Access::Private)
}

/// Read `<data>/burn-next.json` without a lock. A missing file is `None`; a
/// file whose `written` is older than [`FRESH_MIN`] is `None` too, because the
/// tray app that writes it is not running. A file that does not parse is
/// [`io::ErrorKind::InvalidData`].
pub fn read(app: &AppPaths, now: DateTime<Utc>) -> io::Result<Option<BurnNext>> {
    let bytes = match fs::read(app.burn_next_file()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let next: BurnNext = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if now.signed_duration_since(next.written) > Duration::minutes(FRESH_MIN) {
        return Ok(None);
    }
    Ok(Some(next))
}

/// Remove `<data>/burn-next.json`. A missing file is not an error.
pub fn remove(app: &AppPaths) -> io::Result<()> {
    match fs::remove_file(app.burn_next_file()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
