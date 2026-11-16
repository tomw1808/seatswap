//! Auto-burn (spec §2): what a session's tokens cost in work units, the weekly
//! rhythm of work, and each account's rates. The plans, the decision, the cost
//! of a switch and the replay join these types in the tasks after this one.

use chrono::{DateTime, FixedOffset, Utc};

pub mod burn_next;
pub mod cost;
pub mod decide;
pub mod live;
pub mod plan;
pub mod rates;
pub mod replay;
pub mod tied;

/// An account's rates are known from this much work on it (spec §2.3, "while
/// learning").
pub const LEARN_MIN_WU: f64 = 20.0;
/// The rhythm averages this many weeks; the newer two count double.
pub const RHYTHM_WEEKS: i64 = 4;
/// Rates are learned from the samples of this many days.
pub const RATES_DAYS: i64 = 14;
/// The plan's step (spec §2.4).
pub const STEP_MIN: i64 = 15;
/// The plan's horizon (spec §2.4).
pub const HORIZON_H: i64 = 168;

/// A plan's switches as switches per day of its [`HORIZON_H`] hours: the
/// "about N a day" of Settings → Auto-burn and of the Forecast page.
pub fn switches_per_day(switches: u32) -> f64 {
    f64::from(switches) / (HORIZON_H as f64 / 24.0)
}
/// The shortest time between two switches (spec §2.5).
pub const MIN_DWELL_MIN: i64 = 30;
/// The margin of S2, in typical working hours (spec §2.5).
pub const SLACK_MARGIN_H: f64 = 2.0;
/// A switch made by hand holds the early rule back this many hours (spec §2.5).
pub const MANUAL_HOLD_H: i64 = 4;
/// Early switching must save at least this much, in typical working hours
/// (spec §2.5).
pub const MIN_GAIN_H: f64 = 0.5;
/// Early switching must also save at least this many times what its extra
/// switches cost (spec §2.5).
pub const GAIN_FACTOR: f64 = 2.0;
/// A measured switch cost needs at least this many switches (spec §2.5).
pub const MIN_COST_SAMPLES: u32 = 3;
/// A decision needs readings at most this many minutes old for every account
/// auto-burn may use (spec §2.5).
pub const FRESH_MIN: i64 = 20;
/// A switch event that lands after the files it names is learned again after at
/// most this many minutes (spec §5.3: the CLI writes the files before the event).
pub const SWITCH_RELEARN_MIN: i64 = 1;
/// A decision asks for a poll of old readings at most this often (spec §7.4.6).
pub const STALE_ASK_SECS: i64 = 60;
/// A failed history read is tried again after this many minutes (spec §7.4.6).
pub const LEARN_RETRY_MIN: i64 = 5;
/// The same switch decision is sent again after this many minutes (spec §2.5).
pub const SWITCH_RETRY_MIN: i64 = 10;

/// One output token against one input token (spec §2.3).
const OUTPUT_WEIGHT: f64 = 5.0;
/// One token written to the cache against one input token.
const CACHE_WRITE_WEIGHT: f64 = 1.25;
/// One token read from the cache against one input token.
const CACHE_READ_WEIGHT: f64 = 0.1;
/// The hours of the week.
const HOURS: usize = 168;

/// How much one of the [`RHYTHM_WEEKS`] weeks weighs in the rhythm: the two
/// newer ones weigh 2, the two older ones 1.
fn week_weight(week: i64) -> f64 {
    if week < 2 {
        2.0
    } else {
        1.0
    }
}

/// Work in work units: a million weighted tokens (spec §2.3). The one place the
/// four kinds of token are weighed.
pub fn work_units(input: u64, output: u64, cache_write: u64, cache_read: u64) -> f64 {
    (input as f64
        + OUTPUT_WEIGHT * output as f64
        + CACHE_WRITE_WEIGHT * cache_write as f64
        + CACHE_READ_WEIGHT * cache_read as f64)
        / 1_000_000.0
}

/// Work units as hours at `typical_wu` work units per typical working hour;
/// `None` when `typical_wu` is 0.0, because no hours can be computed then. The
/// one work-units-to-hours conversion (spec §2.3, §2.4).
pub fn wu_hours(wu: f64, typical_wu: f64) -> Option<f64> {
    (typical_wu > 0.0).then(|| wu / typical_wu)
}

/// Work units per clock hour of the week, in the local time of the offset the
/// rhythm was learned in; index 0 is Monday 00:00.
#[derive(Debug, Clone, PartialEq)]
pub struct Rhythm {
    pub wu: [f64; HOURS],
}

impl Rhythm {
    /// The work units of the hour that holds `t`.
    pub fn at(&self, t: DateTime<Utc>, offset: FixedOffset) -> f64 {
        self.wu[crate::stats::local_hour_of_week(t, &offset)]
    }

    /// The mean over the hours that have work, 0.0 when there is none: the
    /// "work of a typical working hour" of spec §2.3.
    pub fn typical_hour(&self) -> f64 {
        mean_over_hours(self.wu.iter().copied())
    }
}

/// The mean over the values that have work, 0.0 when there is none: the "work
/// of a typical working hour" of spec §2.3. The one function behind the learned
/// rhythm's [`Rhythm::typical_hour`] and the recorded hours of a replay.
pub(crate) fn mean_over_hours(hours: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for wu in hours {
        if wu > 0.0 {
            sum += wu;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// One account's rates (spec §2.3): `c` is the percent of its week one work
/// unit costs, `s` the percent of one 5 h window. `s` is `None` for an account
/// without a session limit, or until enough work is known for it. Neither rate
/// is ever 0 or below: a rate that cannot be positive is unknown, not a number.
#[derive(Debug, Clone, Copy)]
pub struct Rates {
    pub c: f64,
    pub s: Option<f64>,
}
