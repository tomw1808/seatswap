//! Writing the history (spec §1.1, §1.2): the recorder that turns usage
//! answers into samples, finished windows and blocked events, plus the
//! heartbeat, token and session lines.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Datelike, Utc};

use crate::api::{LimitKind, Usage};

use super::read::{month_start, months_of_kind};
use super::{
    reset_minute, AccountRef, Event, EventKind, HistoryDir, HistoryError, Hosts, Kind, LimitSample,
    Sample, SessionRecord, Source, TokenRecord, UpRecord, WindowRecord, RESET_JITTER,
};

/// Turns usage answers into history lines. Per account it keeps the last written
/// line (which decides when a sample is written again) and every limit it has
/// seen with the time of the reading that carried it (which decides finished
/// windows and the 100 % crossings). A new recorder reads both back from this
/// host's own files. Finished windows are remembered by (account, kind, model,
/// end) so one window is written once even while the server still sends its old
/// reset time.
pub struct Recorder {
    dir: HistoryDir,
    state: HashMap<AccountRef, AccountState>,
    windows: HashSet<WindowKey>,
    last_up: Option<DateTime<Utc>>,
    /// Warnings from the lines this recorder read back (spec §1.2), each
    /// returned once by [`take_warnings`](Self::take_warnings).
    warnings: Vec<String>,
}

/// What one account's readings have shown so far.
#[derive(Default)]
struct AccountState {
    /// The latest sample line written for the account; None = no line found.
    last_written: Option<Sample>,
    /// Every (kind, model) seen, with its latest reading and that reading's time.
    limits: HashMap<LimitKey, RememberedLimit>,
}

impl AccountState {
    /// Remember every limit of `line` that is not remembered yet, as read at
    /// `line.t`. The seeding walks lines newest first, so the newest reading of
    /// a limit wins.
    fn remember_newest(&mut self, line: &Sample) {
        for limit in &line.l {
            self.limits
                .entry(LimitKey::of(limit))
                .or_insert_with(|| RememberedLimit {
                    limit: limit.clone(),
                    t: line.t,
                });
        }
    }
}

/// A limit is matched between readings by its kind and model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LimitKey {
    k: LimitKind,
    m: Option<String>,
}

impl LimitKey {
    fn of(limit: &LimitSample) -> LimitKey {
        LimitKey {
            k: limit.k.clone(),
            m: limit.m.clone(),
        }
    }
}

/// A limit's latest reading, and when that reading happened.
#[derive(Debug, Clone)]
struct RememberedLimit {
    limit: LimitSample,
    t: DateTime<Utc>,
}

/// A finished window, as remembered by the recorder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WindowKey {
    account: AccountRef,
    k: LimitKind,
    m: Option<String>,
    end: DateTime<Utc>,
}

impl WindowKey {
    fn of(record: &WindowRecord) -> WindowKey {
        WindowKey {
            account: record.account.clone(),
            k: record.k.clone(),
            m: record.m.clone(),
            end: record.end,
        }
    }
}

impl Recorder {
    pub fn new(dir: HistoryDir) -> Recorder {
        Recorder {
            dir,
            state: HashMap::new(),
            windows: HashSet::new(),
            last_up: None,
            warnings: Vec::new(),
        }
    }

    pub fn dir(&self) -> &HistoryDir {
        &self.dir
    }

    /// The warnings of the lines the recorder read back since the last call
    /// (spec §1.2): a torn last line, a line of a shape this version does not
    /// know. Each one is returned once, so the app can print it once.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// One usage answer. Appends a Sample if a number or a reset time differs
    /// from the account's last line, or the last line is at least 1 h old.
    /// Before that, appends a WindowRecord for every window that ended
    /// (closed_windows), and a Blocked / Unblocked event for every limit that
    /// crossed 100 %. Returns the events it wrote. The account's last line and
    /// limits are read from the files on first use.
    pub fn reading(
        &mut self,
        account: &AccountRef,
        plan: Option<&str>,
        usage: &Usage,
        at: DateTime<Utc>,
        src: Source,
    ) -> Result<Vec<EventKind>, HistoryError> {
        self.load_once(account)?;
        let next = sample_of(account, usage, at, src);
        let Some(state) = self.state.get_mut(account) else {
            return Err(HistoryError::Io {
                path: PathBuf::from("<recorder>"),
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "the account's state was not loaded",
                ),
            });
        };

        // Every remembered limit is measured against this reading on its own:
        // a limit the reading does not carry ends nothing by itself.
        let mut closed: HashMap<LimitKey, DateTime<Utc>> = HashMap::new();
        let mut windows: Vec<(WindowKey, WindowRecord)> = Vec::new();
        let mut unblocked: Vec<EventKind> = Vec::new();
        for (key, remembered) in &state.limits {
            let prev = Sample {
                t: remembered.t,
                account: account.clone(),
                src: next.src,
                l: vec![remembered.limit.clone()],
            };
            let ended = closed_windows(&prev, &next, plan);
            if ended.is_empty() {
                continue;
            }
            // A full limit whose window ends while no reading carries it was
            // never seen below 100 %, so nobody else can say it is free again.
            // A window that is already written (a restart seeded the old
            // reading) was announced then.
            let carried = next.l.iter().any(|limit| LimitKey::of(limit) == *key);
            let is_new = ended
                .iter()
                .any(|window| !self.windows.contains(&WindowKey::of(window)));
            if !carried && is_new && remembered.limit.p >= 100.0 {
                unblocked.push(EventKind::Unblocked {
                    account: account.clone(),
                    k: key.k.clone(),
                    m: key.m.clone(),
                });
            }
            for window in ended {
                let window_key = WindowKey::of(&window);
                closed.insert(key.clone(), window.end);
                if !self.windows.contains(&window_key) {
                    windows.push((window_key, window));
                }
            }
        }
        for (key, window) in windows {
            self.dir.append(&window)?;
            self.windows.insert(key);
        }

        let mut events = Vec::new();
        for limit in &next.l {
            let before = state
                .limits
                .get(&LimitKey::of(limit))
                .map(|remembered| remembered.limit.p);
            let now_blocked = limit.p >= 100.0;
            let was_blocked = before.map(|percent| percent >= 100.0);
            match (was_blocked, now_blocked) {
                (None, true) | (Some(false), true) => events.push(EventKind::Blocked {
                    account: next.account.clone(),
                    k: limit.k.clone(),
                    m: limit.m.clone(),
                }),
                (Some(true), false) => events.push(EventKind::Unblocked {
                    account: next.account.clone(),
                    k: limit.k.clone(),
                    m: limit.m.clone(),
                }),
                _ => {}
            }
        }
        events.extend(unblocked);
        for kind in &events {
            self.dir.append(&Event {
                t: at,
                kind: kind.clone(),
            })?;
        }

        // A limit in this reading is remembered as read now; one that is
        // missing stays as it was, unless its window just ended.
        // A reset time that is not later than the end just written (server
        // jitter, a stale answer) is that same window: it keeps the written
        // end, or the window would be written a second time with this
        // reading's numbers.
        for limit in &next.l {
            let key = LimitKey::of(limit);
            let mut limit = limit.clone();
            if let (Some(end), Some(r)) = (closed.get(&key), limit.r) {
                if r <= *end + chrono::Duration::seconds(RESET_JITTER) {
                    limit.r = Some(*end);
                }
            }
            state.limits.insert(key, RememberedLimit { limit, t: at });
        }
        for key in closed.keys() {
            let present = next.l.iter().any(|limit| LimitKey::of(limit) == *key);
            if !present {
                state.limits.remove(key);
            }
        }

        let write = match &state.last_written {
            None => true,
            Some(prev) => {
                prev.l != next.l || at.signed_duration_since(prev.t) >= chrono::Duration::hours(1)
            }
        };
        if write {
            self.dir.append(&next)?;
            state.last_written = Some(next);
        }
        Ok(events)
    }

    pub fn event(&self, at: DateTime<Utc>, kind: EventKind) -> Result<(), HistoryError> {
        self.dir.append(&Event { t: at, kind })
    }

    /// At most one UpRecord per 5 minutes.
    pub fn up(&mut self, at: DateTime<Utc>) -> Result<(), HistoryError> {
        let due = match self.last_up {
            None => true,
            Some(last) => at.signed_duration_since(last) >= chrono::Duration::minutes(5),
        };
        if due {
            self.dir.append(&UpRecord { t: at })?;
            self.last_up = Some(at);
        }
        Ok(())
    }

    pub fn tokens(&self, records: &[TokenRecord]) -> Result<(), HistoryError> {
        self.dir.append_many(records)
    }

    pub fn session(&self, record: &SessionRecord) -> Result<(), HistoryError> {
        self.dir.append(record)
    }

    /// On first use of an account: the account's lines on this host, walked
    /// newest first over this host's samples month files that exist, newest
    /// month first. The newest line is the last written one; every other line
    /// adds the limits it carries that no newer line already gave, until the
    /// first `Poll` line has been taken in (it carries every limit) or the files
    /// run out. Then the windows already written for the months of those
    /// limits' ends.
    fn load_once(&mut self, account: &AccountRef) -> Result<(), HistoryError> {
        if self.state.contains_key(account) {
            return Ok(());
        }
        let host_dir = self.dir.root().join(self.dir.host());
        let own = Hosts::One(self.dir.host().to_string());

        let mut state = AccountState::default();
        let mut holds_every_limit = false;
        for range in months_of_kind(&host_dir, Kind::Samples)? {
            let read = self.dir.read::<Sample>(&range, &own)?;
            self.warnings.extend(read.warnings);
            let mut lines: Vec<Sample> = read
                .records
                .into_iter()
                .filter(|hosted| &hosted.record.account == account)
                .map(|hosted| hosted.record)
                .collect();
            lines.sort_by_key(|line| std::cmp::Reverse(line.t));
            for line in lines {
                if state.last_written.is_none() {
                    state.last_written = Some(line.clone());
                }
                state.remember_newest(&line);
                if line.src == Source::Poll {
                    holds_every_limit = true;
                    break;
                }
            }
            if holds_every_limit {
                break;
            }
        }

        let ends: Vec<DateTime<Utc>> = state
            .limits
            .values()
            .filter_map(|remembered| remembered.limit.r)
            .collect();
        if !ends.is_empty() {
            for range in months_of_kind(&host_dir, Kind::Windows)? {
                let holds_end = ends
                    .iter()
                    .any(|end| month_start(end.year(), end.month()) == Some(range.from));
                if !holds_end {
                    continue;
                }
                let read = self.dir.read::<WindowRecord>(&range, &own)?;
                self.warnings.extend(read.warnings);
                for hosted in read.records {
                    if &hosted.record.account == account {
                        self.windows.insert(WindowKey::of(&hosted.record));
                    }
                }
            }
        }

        self.state.insert(account.clone(), state);
        Ok(())
    }
}

/// One usage answer as a history sample, with reset times through
/// [`reset_minute`]. Limits keep the order of `usage.limits`.
pub fn sample_of(account: &AccountRef, usage: &Usage, at: DateTime<Utc>, src: Source) -> Sample {
    Sample {
        t: at,
        account: account.clone(),
        src,
        l: usage
            .limits
            .iter()
            .map(|limit| LimitSample {
                k: limit.kind.clone(),
                m: limit.model.clone(),
                p: limit.percent,
                r: limit.resets_at.map(reset_minute),
                b: limit.is_active,
            })
            .collect(),
    }
}

/// The windows of `prev` that have ended by `next`. A limit (matched by `k` and
/// `m`) has ended when `prev.r` is `Some(end)` and: `next.t >= end`, or next has
/// the limit with `next.r > end + 60 s`. A limit missing from `next`, or with
/// `r` None, ends nothing by itself. `end = prev.r`; `final = prev.p`;
/// `lastSample = prev.t`; `uncertain = end - prev.t > 1 h`.
/// `start = end - 7 d` for weekly_all and weekly_scoped, `end - 5 h` for
/// session, `None` otherwise. `lost = max(0, 100 - final)` for the two weekly
/// kinds, `None` otherwise.
pub fn closed_windows(prev: &Sample, next: &Sample, plan: Option<&str>) -> Vec<WindowRecord> {
    let mut windows = Vec::new();
    for limit in &prev.l {
        let Some(end) = limit.r else { continue };
        let same_later = find_limit(next, limit)
            .and_then(|current| current.r)
            .map(|new_end| new_end > end + chrono::Duration::seconds(RESET_JITTER))
            .unwrap_or(false);
        if next.t >= end || same_later {
            windows.push(window_of(prev, limit, end, plan));
        }
    }
    windows
}

fn find_limit<'a>(sample: &'a Sample, limit: &LimitSample) -> Option<&'a LimitSample> {
    sample
        .l
        .iter()
        .find(|other| other.k == limit.k && other.m == limit.m)
}

fn window_of(
    prev: &Sample,
    limit: &LimitSample,
    end: DateTime<Utc>,
    plan: Option<&str>,
) -> WindowRecord {
    let (start, lost) = match &limit.k {
        LimitKind::WeeklyAll | LimitKind::WeeklyScoped => (
            Some(end - chrono::Duration::days(7)),
            Some((100.0 - limit.p).max(0.0)),
        ),
        LimitKind::Session => (Some(end - chrono::Duration::hours(5)), None),
        LimitKind::Other(_) => (None, None),
    };
    WindowRecord {
        account: prev.account.clone(),
        k: limit.k.clone(),
        m: limit.m.clone(),
        start,
        end,
        final_percent: limit.p,
        lost,
        last_sample: prev.t,
        plan: plan.map(str::to_string),
        uncertain: end.signed_duration_since(prev.t) > chrono::Duration::hours(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::io;
    use std::io::Write;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::api::{Limit, Severity};

    use super::super::Range;

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse::<DateTime<Utc>>().expect("test date")
    }

    fn atlas_account() -> AccountRef {
        AccountRef {
            acc: "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04".to_string(),
            org: "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20".to_string(),
        }
    }

    fn history(root: &Path) -> HistoryDir {
        HistoryDir::at(root, "atlas")
    }

    fn limit(kind: LimitKind, percent: f64, resets_at: Option<DateTime<Utc>>) -> Limit {
        Limit {
            kind,
            percent,
            severity: Severity::Normal,
            resets_at,
            model: None,
            is_active: false,
        }
    }

    fn weekly(percent: f64, resets_at: Option<DateTime<Utc>>) -> Usage {
        Usage {
            limits: vec![limit(LimitKind::WeeklyAll, percent, resets_at)],
            extra_usage_enabled: false,
            warnings: Vec::new(),
        }
    }

    fn scoped_weekly(model: &str, percent: f64, resets_at: Option<DateTime<Utc>>) -> Usage {
        let mut scoped = limit(LimitKind::WeeklyScoped, percent, resets_at);
        scoped.model = Some(model.to_string());
        Usage {
            limits: vec![scoped],
            extra_usage_enabled: false,
            warnings: Vec::new(),
        }
    }

    fn all_and_scoped(
        all: f64,
        all_reset: Option<DateTime<Utc>>,
        model: &str,
        scoped: f64,
        scoped_reset: Option<DateTime<Utc>>,
    ) -> Usage {
        let mut model_limit = limit(LimitKind::WeeklyScoped, scoped, scoped_reset);
        model_limit.model = Some(model.to_string());
        Usage {
            limits: vec![limit(LimitKind::WeeklyAll, all, all_reset), model_limit],
            extra_usage_enabled: false,
            warnings: Vec::new(),
        }
    }

    fn windows_of(root: &Path) -> Vec<WindowRecord> {
        let text = fs::read_to_string(root.join("atlas/windows-2026-11.jsonl")).unwrap();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn wide_range() -> Range {
        Range {
            from: DateTime::<Utc>::MIN_UTC,
            to: DateTime::<Utc>::MAX_UTC,
        }
    }

    fn limit_sample(
        kind: LimitKind,
        model: Option<&str>,
        percent: f64,
        resets_at: Option<DateTime<Utc>>,
    ) -> LimitSample {
        LimitSample {
            k: kind,
            m: model.map(str::to_string),
            p: percent,
            r: resets_at,
            b: false,
        }
    }

    fn sample(t: DateTime<Utc>, limits: Vec<LimitSample>) -> Sample {
        Sample {
            t,
            account: atlas_account(),
            src: Source::Poll,
            l: limits,
        }
    }

    fn line_count(path: &Path) -> usize {
        match fs::read_to_string(path) {
            Ok(text) => text.lines().count(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }

    fn sample_lines(root: &Path) -> usize {
        line_count(&root.join("atlas/samples-2026-11.jsonl"))
    }

    fn window_lines(root: &Path) -> usize {
        line_count(&root.join("atlas/windows-2026-11.jsonl"))
    }

    #[test]
    fn the_first_reading_writes_a_sample() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));

        recorder
            .reading(
                &atlas_account(),
                None,
                &weekly(75.0, Some(utc("2026-11-16T16:00:00Z"))),
                utc("2026-11-14T14:02:11Z"),
                Source::Poll,
            )
            .unwrap();

        assert_eq!(sample_lines(dir.path()), 1);
    }

    #[test]
    fn an_unchanged_reading_within_the_hour_writes_nothing() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let usage = weekly(75.0, Some(utc("2026-11-16T16:00:00Z")));

        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-14T14:02:11Z"),
                Source::Poll,
            )
            .unwrap();
        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-14T14:04:11Z"),
                Source::Poll,
            )
            .unwrap();

        assert_eq!(sample_lines(dir.path()), 1);
    }

    #[test]
    fn an_unchanged_reading_after_an_hour_writes_again() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let usage = weekly(75.0, Some(utc("2026-11-16T16:00:00Z")));

        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-14T14:02:11Z"),
                Source::Poll,
            )
            .unwrap();
        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-14T15:02:11Z"),
                Source::Poll,
            )
            .unwrap();

        assert_eq!(sample_lines(dir.path()), 2);
    }

    #[test]
    fn a_changed_percent_writes_at_once() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let reset = Some(utc("2026-11-16T16:00:00Z"));

        recorder
            .reading(
                &atlas_account(),
                None,
                &weekly(75.0, reset),
                utc("2026-11-14T14:02:11Z"),
                Source::Poll,
            )
            .unwrap();
        recorder
            .reading(
                &atlas_account(),
                None,
                &weekly(76.0, reset),
                utc("2026-11-14T14:04:11Z"),
                Source::Poll,
            )
            .unwrap();

        assert_eq!(sample_lines(dir.path()), 2);
    }

    #[test]
    fn a_weekly_reset_closes_the_window() {
        let prev = sample(
            utc("2026-11-16T15:58:10Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                87.0,
                Some(utc("2026-11-16T16:00:00Z")),
            )],
        );
        let next = sample(
            utc("2026-11-16T16:02:00Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                1.0,
                Some(utc("2026-11-23T16:00:00Z")),
            )],
        );

        let windows = closed_windows(&prev, &next, None);

        assert_eq!(
            windows,
            vec![WindowRecord {
                account: atlas_account(),
                k: LimitKind::WeeklyAll,
                m: None,
                start: Some(utc("2026-11-09T16:00:00Z")),
                end: utc("2026-11-16T16:00:00Z"),
                final_percent: 87.0,
                lost: Some(13.0),
                last_sample: utc("2026-11-16T15:58:10Z"),
                plan: None,
                uncertain: false,
            }]
        );
    }

    #[test]
    fn a_reset_seen_late_is_uncertain() {
        let prev = sample(
            utc("2026-11-15T09:00:00Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                87.0,
                Some(utc("2026-11-16T16:00:00Z")),
            )],
        );
        let next = sample(
            utc("2026-11-17T08:00:00Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                1.0,
                Some(utc("2026-11-23T16:00:00Z")),
            )],
        );

        let windows = closed_windows(&prev, &next, None);

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].final_percent, 87.0);
        assert_eq!(windows[0].end, utc("2026-11-16T16:00:00Z"));
        assert!(windows[0].uncertain);
    }

    #[test]
    fn a_session_window_that_ends_has_no_lost() {
        let prev = sample(
            utc("2026-11-14T19:10:00Z"),
            vec![limit_sample(
                LimitKind::Session,
                None,
                64.0,
                Some(utc("2026-11-14T19:30:00Z")),
            )],
        );
        let next = sample(
            utc("2026-11-14T19:31:00Z"),
            vec![limit_sample(LimitKind::Session, None, 0.0, None)],
        );

        let windows = closed_windows(&prev, &next, None);

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].k, LimitKind::Session);
        assert_eq!(windows[0].start, Some(utc("2026-11-14T14:30:00Z")));
        assert_eq!(windows[0].end, utc("2026-11-14T19:30:00Z"));
        assert_eq!(windows[0].final_percent, 64.0);
        assert_eq!(windows[0].lost, None);
    }

    #[test]
    fn a_session_without_a_reset_time_closes_nothing() {
        let prev = sample(
            utc("2026-11-14T19:10:00Z"),
            vec![limit_sample(LimitKind::Session, None, 64.0, None)],
        );
        let next = sample(
            utc("2026-11-14T19:12:00Z"),
            vec![limit_sample(LimitKind::Session, None, 65.0, None)],
        );

        assert!(closed_windows(&prev, &next, None).is_empty());
    }

    #[test]
    fn a_reset_time_that_moves_by_seconds_is_the_same_window() {
        let prev = sample(
            utc("2026-11-16T15:59:00Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                87.0,
                Some(utc("2026-11-16T16:00:00Z")),
            )],
        );
        let next = sample(
            utc("2026-11-16T15:59:30Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                87.0,
                Some(utc("2026-11-16T16:00:30Z")),
            )],
        );

        assert!(closed_windows(&prev, &next, None).is_empty());
    }

    #[test]
    fn over_100_percent_loses_nothing() {
        let prev = sample(
            utc("2026-11-16T15:58:10Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                103.0,
                Some(utc("2026-11-16T16:00:00Z")),
            )],
        );
        let next = sample(
            utc("2026-11-16T16:02:00Z"),
            vec![limit_sample(
                LimitKind::WeeklyAll,
                None,
                0.0,
                Some(utc("2026-11-23T16:00:00Z")),
            )],
        );

        let windows = closed_windows(&prev, &next, None);

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].lost, Some(0.0));
    }

    #[test]
    fn crossing_100_writes_blocked_and_back_unblocked() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let account = atlas_account();
        let start = utc("2026-11-14T10:00:00Z");
        let reset = Some(utc("2026-11-16T16:00:00Z"));
        let later = Some(utc("2026-11-23T16:00:00Z"));

        let first = recorder
            .reading(&account, None, &weekly(99.0, reset), start, Source::Poll)
            .unwrap();
        assert!(first.is_empty());

        let blocked = recorder
            .reading(
                &account,
                None,
                &weekly(100.0, reset),
                start + chrono::Duration::minutes(2),
                Source::Poll,
            )
            .unwrap();
        assert_eq!(
            blocked,
            vec![EventKind::Blocked {
                account: account.clone(),
                k: LimitKind::WeeklyAll,
                m: None,
            }]
        );

        let still = recorder
            .reading(
                &account,
                None,
                &weekly(100.0, reset),
                start + chrono::Duration::minutes(3),
                Source::Poll,
            )
            .unwrap();
        assert!(still.is_empty());

        let unblocked = recorder
            .reading(
                &account,
                None,
                &weekly(3.0, later),
                start + chrono::Duration::minutes(4),
                Source::Poll,
            )
            .unwrap();
        assert_eq!(
            unblocked,
            vec![EventKind::Unblocked {
                account,
                k: LimitKind::WeeklyAll,
                m: None,
            }]
        );
    }

    #[test]
    fn the_last_line_is_found_again_after_a_restart() {
        let dir = tempdir().unwrap();
        let usage = weekly(75.0, Some(utc("2026-11-16T16:00:00Z")));

        let mut recorder = Recorder::new(history(dir.path()));
        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-14T14:02:11Z"),
                Source::Poll,
            )
            .unwrap();
        drop(recorder);

        let mut restarted = Recorder::new(history(dir.path()));
        restarted
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-14T14:04:11Z"),
                Source::Poll,
            )
            .unwrap();

        assert_eq!(sample_lines(dir.path()), 1);
    }

    #[test]
    fn load_once_warnings_reach_the_caller() {
        let dir = tempdir().unwrap();
        let mut first = Recorder::new(history(dir.path()));
        first
            .reading(
                &atlas_account(),
                None,
                &weekly(75.0, Some(utc("2026-11-16T16:00:00Z"))),
                utc("2026-11-14T14:02:11Z"),
                Source::Poll,
            )
            .unwrap();
        drop(first);

        // A crash left a torn last line (spec §1.2): reading names it.
        let path = dir.path().join("atlas/samples-2026-11.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"t\":\"2026-11-14T15").unwrap();
        drop(file);

        let mut restarted = Recorder::new(history(dir.path()));
        restarted
            .reading(
                &atlas_account(),
                None,
                &weekly(76.0, Some(utc("2026-11-16T16:00:00Z"))),
                utc("2026-11-14T14:04:11Z"),
                Source::Poll,
            )
            .unwrap();

        let warnings = restarted.take_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("samples-2026-11.jsonl"),
            "the warning names the file: {warnings:?}"
        );
        assert!(
            restarted.take_warnings().is_empty(),
            "a warning is returned once"
        );
    }

    #[test]
    fn up_writes_once_per_five_minutes() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));

        recorder.up(utc("2026-11-14T10:00:00Z")).unwrap();
        recorder.up(utc("2026-11-14T10:02:00Z")).unwrap();
        recorder.up(utc("2026-11-14T10:05:00Z")).unwrap();

        assert_eq!(line_count(&dir.path().join("atlas/up-2026-11.jsonl")), 2);
    }

    #[test]
    fn a_window_is_written_once_while_the_server_still_sends_the_old_reset() {
        let dir = tempdir().unwrap();
        let usage = weekly(87.0, Some(utc("2026-11-16T16:00:00Z")));

        let mut recorder = Recorder::new(history(dir.path()));
        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-16T15:58:10Z"),
                Source::Poll,
            )
            .unwrap();
        assert_eq!(window_lines(dir.path()), 0);
        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-16T16:01:00Z"),
                Source::Poll,
            )
            .unwrap();
        assert_eq!(window_lines(dir.path()), 1);
        recorder
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-16T16:03:00Z"),
                Source::Poll,
            )
            .unwrap();
        assert_eq!(window_lines(dir.path()), 1);
        drop(recorder);

        let mut restarted = Recorder::new(history(dir.path()));
        restarted
            .reading(
                &atlas_account(),
                None,
                &usage,
                utc("2026-11-16T16:05:00Z"),
                Source::Poll,
            )
            .unwrap();
        assert_eq!(window_lines(dir.path()), 1);
    }

    #[test]
    fn a_limit_missing_once_does_not_close_its_window() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let account = atlas_account();
        let reset = Some(utc("2026-11-16T16:00:00Z"));

        recorder
            .reading(
                &account,
                None,
                &scoped_weekly("Fable", 50.0, reset),
                utc("2026-11-14T10:00:00Z"),
                Source::Poll,
            )
            .unwrap();
        let session_only = Usage {
            limits: vec![limit(
                LimitKind::Session,
                20.0,
                Some(utc("2026-11-14T14:00:00Z")),
            )],
            extra_usage_enabled: false,
            warnings: Vec::new(),
        };
        recorder
            .reading(
                &account,
                None,
                &session_only,
                utc("2026-11-14T10:02:00Z"),
                Source::Statusline,
            )
            .unwrap();
        recorder
            .reading(
                &account,
                None,
                &scoped_weekly("Fable", 87.0, reset),
                utc("2026-11-14T10:04:00Z"),
                Source::Poll,
            )
            .unwrap();
        recorder
            .reading(
                &account,
                None,
                &scoped_weekly("Fable", 87.0, reset),
                utc("2026-11-16T15:58:00Z"),
                Source::Poll,
            )
            .unwrap();
        recorder
            .reading(
                &account,
                None,
                &scoped_weekly("Fable", 1.0, Some(utc("2026-11-23T16:00:00Z"))),
                utc("2026-11-16T16:02:00Z"),
                Source::Poll,
            )
            .unwrap();

        let read = history(dir.path())
            .read::<WindowRecord>(&wide_range(), &Hosts::All)
            .unwrap();
        let scoped: Vec<&WindowRecord> = read
            .records
            .iter()
            .map(|hosted| &hosted.record)
            .filter(|window| {
                window.k == LimitKind::WeeklyScoped && window.m.as_deref() == Some("Fable")
            })
            .collect();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].final_percent, 87.0);
        assert!(!scoped[0].uncertain);
        assert_eq!(scoped[0].last_sample, utc("2026-11-16T15:58:00Z"));
    }

    #[test]
    fn blocked_is_not_repeated_after_a_limit_blinks() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let account = atlas_account();
        let start = utc("2026-11-14T10:00:00Z");
        let usage = weekly(100.0, Some(utc("2026-11-16T16:00:00Z")));

        let blocked = recorder
            .reading(&account, None, &usage, start, Source::Poll)
            .unwrap();
        assert_eq!(
            blocked,
            vec![EventKind::Blocked {
                account: account.clone(),
                k: LimitKind::WeeklyAll,
                m: None,
            }]
        );

        let no_limits = Usage {
            limits: vec![],
            extra_usage_enabled: false,
            warnings: Vec::new(),
        };
        let nothing = recorder
            .reading(
                &account,
                None,
                &no_limits,
                start + chrono::Duration::minutes(2),
                Source::Poll,
            )
            .unwrap();
        assert!(nothing.is_empty());

        let again = recorder
            .reading(
                &account,
                None,
                &usage,
                start + chrono::Duration::minutes(4),
                Source::Poll,
            )
            .unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn last_sample_is_the_last_reading() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let account = atlas_account();
        let reset = Some(utc("2026-11-16T16:00:00Z"));

        recorder
            .reading(
                &account,
                None,
                &weekly(87.0, reset),
                utc("2026-11-16T14:59:00Z"),
                Source::Poll,
            )
            .unwrap();
        let mut at = utc("2026-11-16T15:00:50Z");
        let last = utc("2026-11-16T15:58:50Z");
        while at <= last {
            recorder
                .reading(&account, None, &weekly(87.0, reset), at, Source::Poll)
                .unwrap();
            at += chrono::Duration::minutes(2);
        }
        recorder
            .reading(
                &account,
                None,
                &weekly(1.0, Some(utc("2026-11-23T16:00:00Z"))),
                utc("2026-11-16T16:00:50Z"),
                Source::Poll,
            )
            .unwrap();

        let read = history(dir.path())
            .read::<WindowRecord>(&wide_range(), &Hosts::All)
            .unwrap();
        assert_eq!(read.records.len(), 1);
        assert!(!read.records[0].record.uncertain);
        assert_eq!(
            read.records[0].record.last_sample,
            utc("2026-11-16T15:58:50Z")
        );
    }

    #[test]
    fn a_window_is_found_when_the_last_line_is_two_months_back() {
        let dir = tempdir().unwrap();
        let account = atlas_account();

        let mut first = Recorder::new(history(dir.path()));
        first
            .reading(
                &account,
                None,
                &weekly(87.0, Some(utc("2026-11-02T16:00:00Z"))),
                utc("2026-10-31T23:00:00Z"),
                Source::Poll,
            )
            .unwrap();
        drop(first);

        let mut second = Recorder::new(history(dir.path()));
        second
            .reading(
                &account,
                None,
                &weekly(1.0, Some(utc("2026-12-07T16:00:00Z"))),
                utc("2026-12-01T00:05:00Z"),
                Source::Poll,
            )
            .unwrap();

        let text = fs::read_to_string(dir.path().join("atlas/windows-2026-11.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        let window: WindowRecord = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(window.final_percent, 87.0);
        assert!(window.uncertain);
    }

    #[test]
    fn blocked_events_are_in_the_events_file() {
        let dir = tempdir().unwrap();
        let mut recorder = Recorder::new(history(dir.path()));
        let account = atlas_account();
        let start = utc("2026-11-14T10:00:00Z");
        let reset = Some(utc("2026-11-16T16:00:00Z"));

        recorder
            .reading(&account, None, &weekly(99.0, reset), start, Source::Poll)
            .unwrap();
        recorder
            .reading(
                &account,
                None,
                &weekly(100.0, reset),
                start + chrono::Duration::minutes(2),
                Source::Poll,
            )
            .unwrap();

        let text = fs::read_to_string(dir.path().join("atlas/events-2026-11.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("\"type\":\"blocked\""));
    }

    #[test]
    fn a_scoped_limit_survives_a_restart_after_a_status_line_sample() {
        let dir = tempdir().unwrap();
        let account = atlas_account();

        let mut recorder = Recorder::new(history(dir.path()));
        recorder
            .reading(
                &account,
                None,
                &all_and_scoped(
                    40.0,
                    Some(utc("2026-11-20T18:00:00Z")),
                    "Fable",
                    87.0,
                    Some(utc("2026-11-16T16:00:00Z")),
                ),
                utc("2026-11-14T10:00:00Z"),
                Source::Poll,
            )
            .unwrap();
        recorder
            .reading(
                &account,
                None,
                &weekly(41.0, None),
                utc("2026-11-14T10:05:00Z"),
                Source::Statusline,
            )
            .unwrap();
        drop(recorder);

        let mut restarted = Recorder::new(history(dir.path()));
        restarted
            .reading(
                &account,
                None,
                &all_and_scoped(50.0, None, "Fable", 1.0, Some(utc("2026-11-23T16:00:00Z"))),
                utc("2026-11-16T16:02:00Z"),
                Source::Poll,
            )
            .unwrap();

        let scoped: Vec<WindowRecord> = windows_of(dir.path())
            .into_iter()
            .filter(|window| {
                window.k == LimitKind::WeeklyScoped && window.m.as_deref() == Some("Fable")
            })
            .collect();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].final_percent, 87.0);
        assert_eq!(scoped[0].last_sample, utc("2026-11-14T10:00:00Z"));
    }

    #[test]
    fn a_full_limit_that_ends_unseen_writes_unblocked() {
        let dir = tempdir().unwrap();
        let account = atlas_account();

        let mut recorder = Recorder::new(history(dir.path()));
        let blocked = recorder
            .reading(
                &account,
                None,
                &all_and_scoped(
                    40.0,
                    None,
                    "Fable",
                    100.0,
                    Some(utc("2026-11-16T16:00:00Z")),
                ),
                utc("2026-11-14T10:00:00Z"),
                Source::Poll,
            )
            .unwrap();
        assert_eq!(
            blocked,
            vec![EventKind::Blocked {
                account: account.clone(),
                k: LimitKind::WeeklyScoped,
                m: Some("Fable".to_string()),
            }]
        );

        let quiet = recorder
            .reading(
                &account,
                None,
                &weekly(41.0, None),
                utc("2026-11-15T10:00:00Z"),
                Source::Statusline,
            )
            .unwrap();
        assert!(quiet.is_empty());

        let unblocked = recorder
            .reading(
                &account,
                None,
                &weekly(42.0, None),
                utc("2026-11-16T16:01:00Z"),
                Source::Statusline,
            )
            .unwrap();
        assert_eq!(
            unblocked,
            vec![EventKind::Unblocked {
                account: account.clone(),
                k: LimitKind::WeeklyScoped,
                m: Some("Fable".to_string()),
            }]
        );

        let events = fs::read_to_string(dir.path().join("atlas/events-2026-11.jsonl")).unwrap();
        assert_eq!(
            events
                .lines()
                .filter(|line| line.contains("\"type\":\"unblocked\""))
                .count(),
            1
        );

        let scoped: Vec<WindowRecord> = windows_of(dir.path())
            .into_iter()
            .filter(|window| {
                window.k == LimitKind::WeeklyScoped && window.m.as_deref() == Some("Fable")
            })
            .collect();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].final_percent, 100.0);
    }

    #[test]
    fn a_restart_does_not_repeat_unblocked() {
        let dir = tempdir().unwrap();
        let account = atlas_account();
        let fable_reset = Some(utc("2026-11-16T16:00:00Z"));

        let mut recorder = Recorder::new(history(dir.path()));
        recorder
            .reading(
                &account,
                None,
                &all_and_scoped(40.0, None, "Fable", 100.0, fable_reset),
                utc("2026-11-14T10:00:00Z"),
                Source::Poll,
            )
            .unwrap();
        let first = recorder
            .reading(
                &account,
                None,
                &weekly(42.0, None),
                utc("2026-11-16T16:01:00Z"),
                Source::Statusline,
            )
            .unwrap();
        assert_eq!(first.len(), 1);
        drop(recorder);

        let mut restarted = Recorder::new(history(dir.path()));
        let again = restarted
            .reading(
                &account,
                None,
                &weekly(43.0, None),
                utc("2026-11-16T16:03:00Z"),
                Source::Statusline,
            )
            .unwrap();

        assert!(again.is_empty());
        let events = fs::read_to_string(dir.path().join("atlas/events-2026-11.jsonl")).unwrap();
        assert_eq!(
            events
                .lines()
                .filter(|line| line.contains("\"type\":\"unblocked\""))
                .count(),
            1
        );
        assert_eq!(window_lines(dir.path()), 1);
    }

    #[test]
    fn a_reset_time_that_jitters_after_the_end_writes_the_window_once() {
        let dir = tempdir().unwrap();
        let account = atlas_account();
        let mut recorder = Recorder::new(history(dir.path()));
        for (at, percent, reset) in [
            ("2026-11-16T15:58:00Z", 87.0, "2026-11-16T16:00:00Z"),
            ("2026-11-16T16:00:30Z", 88.0, "2026-11-16T16:01:00Z"),
            ("2026-11-16T16:02:00Z", 1.0, "2026-11-23T16:00:00Z"),
        ] {
            recorder
                .reading(
                    &account,
                    None,
                    &weekly(percent, Some(utc(reset))),
                    utc(at),
                    Source::Poll,
                )
                .unwrap();
        }

        let windows = windows_of(dir.path());
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].end, utc("2026-11-16T16:00:00Z"));
        assert_eq!(windows[0].final_percent, 87.0);
    }
}
