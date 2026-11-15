//! The feed: the live 5 h and weekly numbers Claude Code hands to the status
//! line, and how they reach the usage cache (spec §5.3, step B, Task 3).
//!
//! The status line command writes them to `<data>/feed.json`; the tray app's
//! engine reads that file and merges it into the cache entry of the one account
//! whose weekly reset the numbers belong to. A session that has not sent a
//! request since a switch still carries the old account's numbers, so matching
//! by the weekly reset is what keeps them off the new account.

use std::fs;
use std::io;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::{LimitKind, Usage};
use crate::atomic::{self, write_atomic, Access};
use crate::engine::UsageCache;
use crate::history::same_reset;
use crate::paths::AppPaths;
use crate::store::Account;

/// A feed with unchanged numbers is rewritten after this long, so the file's
/// own `at` stays a recent reading while the numbers do not move.
const REWRITE_AFTER_SECS: i64 = 10;

/// One limit in the feed: a percent and its reset time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedLimit {
    pub percent: f64,
    pub resets_at: DateTime<Utc>,
}

/// What Claude Code's status line payload says about the session's account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    pub at: DateTime<Utc>,
    pub five_hour: Option<FeedLimit>,
    pub seven_day: Option<FeedLimit>,
    pub model: Option<String>,
}

/// Write `<data>/feed.json` when its numbers differ from the file's or the
/// file is at least 10 s older than `feed.at`. Returns whether it wrote.
///
/// The file is seatswap's own cache of the last numbers, never a source of
/// truth: a file that does not parse is simply overwritten by this call.
pub fn write_if_due(app: &AppPaths, feed: &Feed) -> io::Result<bool> {
    let write = match read(app) {
        Ok(Some(stored)) => {
            let same = stored.five_hour == feed.five_hour && stored.seven_day == feed.seven_day;
            let old_enough = feed.at.signed_duration_since(stored.at)
                >= chrono::Duration::seconds(REWRITE_AFTER_SECS);
            // A stored `at` later than this reading means the clock was set
            // back; that must not block every write until it catches up.
            let clock_went_back = stored.at > feed.at;
            !same || old_enough || clock_went_back
        }
        Ok(None) => true,
        // A file that is ours but does not parse: overwrite it.
        Err(error) if error.kind() == io::ErrorKind::InvalidData => true,
        Err(error) => return Err(error),
    };
    if !write {
        return Ok(false);
    }
    atomic::ensure_private_dir(&app.data_dir)?;
    let path = app.feed_file();
    let bytes = serde_json::to_vec_pretty(feed)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(&path, &bytes, Access::Private)?;
    Ok(true)
}

/// Read `<data>/feed.json`. A missing file is `None`; a file that does not
/// parse is [`io::ErrorKind::InvalidData`], which [`write_if_due`] replaces.
pub fn read(app: &AppPaths) -> io::Result<Option<Feed>> {
    let path = app.feed_file();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The stored account these numbers belong to: the one account whose cached
/// `weekly_all` reset names the same window as `feed.seven_day.resets_at`
/// ([`same_reset`]). `None` when no account or more than one matches, or the
/// feed has no week.
pub fn account_for(feed: &Feed, cache: &UsageCache, accounts: &[Account]) -> Option<String> {
    let resets_at = feed.seven_day.as_ref()?.resets_at;
    let mut found: Option<&str> = None;
    for account in accounts {
        let Some(reset) = cache
            .entries
            .get(&account.id)
            .and_then(|entry| entry.usage.as_ref())
            .and_then(Usage::weekly_all)
            .and_then(|limit| limit.resets_at)
        else {
            continue;
        };
        if !same_reset(reset, resets_at) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(account.id.as_str());
    }
    found.map(str::to_string)
}

/// `usage` with the session and weekly-all numbers taken from the feed when
/// the feed is a newer reading, and everything else as it was.
///
/// The feed never replaces a newer reading with an older one:
/// - **Session:** taken when the cache has no reset, when the feed's reset is
///   later than the cached one (a new window), or when the two name the same
///   window ([`same_reset`]) and the feed's percent is at least the cached
///   percent. A feed reset earlier than the cached one is an ended window and
///   is ignored; a lower percent in the same window is an older reading and is
///   ignored.
/// - **Week:** the percent is taken only when it is at least the cached one;
///   `account_for` has already made it the same weekly window. The reset stays
///   the cached one.
pub fn merged(usage: &Usage, feed: &Feed) -> Usage {
    let mut merged = usage.clone();
    for limit in &mut merged.limits {
        match limit.kind {
            LimitKind::Session => {
                let Some(five_hour) = &feed.five_hour else {
                    continue;
                };
                let take = match limit.resets_at {
                    None => true,
                    Some(cached_reset) => {
                        if same_reset(cached_reset, five_hour.resets_at) {
                            five_hour.percent >= limit.percent
                        } else {
                            five_hour.resets_at > cached_reset
                        }
                    }
                };
                if take {
                    limit.percent = five_hour.percent;
                    limit.resets_at = Some(five_hour.resets_at);
                }
            }
            LimitKind::WeeklyAll => {
                if let Some(seven_day) = &feed.seven_day {
                    if seven_day.percent >= limit.percent {
                        limit.percent = seven_day.percent;
                    }
                }
            }
            LimitKind::WeeklyScoped | LimitKind::Other(_) => {}
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::TimeZone;

    use crate::api::{Limit, Severity};
    use crate::engine::CacheEntry;
    use crate::store::LoginState;
    use crate::types::OAuthAccount;

    const LB_ID: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
    const AC_ID: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
    const LB_UUID: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const AC_UUID: &str = "8e1d5b7a-6c3f-4a2b-9d8e-1f2a3b4c5d6e";
    const ORG: &str = "e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62";

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("an RFC 3339 test time")
            .with_timezone(&Utc)
    }

    fn weekday(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, hour, minute, 0)
            .single()
            .expect("a unique local time")
    }

    fn account(id: &str, abbr: &str, uuid: &str) -> Account {
        Account {
            id: id.to_string(),
            account_uuid: uuid.to_string(),
            organization_uuid: ORG.to_string(),
            label: format!("{abbr} team"),
            abbr: abbr.to_string(),
            color: "#3b82f6".to_string(),
            email: None,
            org_name: None,
            plan: Some("Team".to_string()),
            oauth_account: OAuthAccount(serde_json::Map::new()),
            added_at: at("2026-11-01T00:00:00Z"),
            last_used_at: None,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            auto_burn: true,
        }
    }

    fn weekly(percent: f64, resets_at: DateTime<Utc>) -> Usage {
        Usage {
            limits: vec![Limit {
                kind: LimitKind::WeeklyAll,
                percent,
                severity: Severity::Normal,
                resets_at: Some(resets_at),
                model: None,
                is_active: true,
            }],
            extra_usage_enabled: false,
        }
    }

    fn cached(id: &str, usage: Usage) -> UsageCache {
        let mut cache = UsageCache::default();
        cache.entries.insert(
            id.to_string(),
            CacheEntry {
                usage: Some(usage),
                fetched_at: Some(at("2026-11-14T12:00:00Z")),
                ..CacheEntry::default()
            },
        );
        cache
    }

    fn feed(seven_day: Option<FeedLimit>, five_hour: Option<FeedLimit>) -> Feed {
        Feed {
            at: at("2026-11-14T12:00:00Z"),
            five_hour,
            seven_day,
            model: Some("claude-fable-5-2".to_string()),
        }
    }

    #[test]
    fn the_feed_goes_to_the_account_with_that_weekly_reset() {
        let accounts = vec![account(LB_ID, "LB", LB_UUID), account(AC_ID, "AC", AC_UUID)];
        let ac = weekly(
            30.0,
            weekday(16, 2, 0) + chrono::Duration::microseconds(336_716),
        );
        let mut cache = cached(LB_ID, weekly(40.0, weekday(16, 17, 0)));
        cache.entries.insert(
            AC_ID.to_string(),
            CacheEntry {
                usage: Some(ac),
                fetched_at: Some(at("2026-11-14T12:00:00Z")),
                ..CacheEntry::default()
            },
        );

        let numbers = feed(
            Some(FeedLimit {
                percent: 48.0,
                resets_at: weekday(16, 2, 0)
                    + chrono::Duration::seconds(20)
                    + chrono::Duration::microseconds(336_716),
            }),
            None,
        );

        assert_eq!(
            account_for(&numbers, &cache, &accounts).as_deref(),
            Some(AC_ID)
        );
    }

    #[test]
    fn no_match_is_none() {
        let accounts = vec![account(LB_ID, "LB", LB_UUID)];
        let cache = cached(LB_ID, weekly(40.0, weekday(16, 17, 0)));
        let numbers = feed(
            Some(FeedLimit {
                percent: 48.0,
                resets_at: weekday(17, 9, 0),
            }),
            None,
        );

        assert_eq!(account_for(&numbers, &cache, &accounts), None);
    }

    #[test]
    fn two_matches_are_none() {
        let reset = weekday(16, 2, 0);
        let accounts = vec![account(LB_ID, "LB", LB_UUID), account(AC_ID, "AC", AC_UUID)];
        let mut cache = cached(LB_ID, weekly(40.0, reset));
        cache.entries.insert(
            AC_ID.to_string(),
            CacheEntry {
                usage: Some(weekly(30.0, reset)),
                fetched_at: Some(at("2026-11-14T12:00:00Z")),
                ..CacheEntry::default()
            },
        );
        let numbers = feed(
            Some(FeedLimit {
                percent: 48.0,
                resets_at: reset,
            }),
            None,
        );

        assert_eq!(account_for(&numbers, &cache, &accounts), None);
    }

    #[test]
    fn a_feed_without_seven_day_is_none() {
        let accounts = vec![account(LB_ID, "LB", LB_UUID)];
        let cache = cached(LB_ID, weekly(40.0, weekday(16, 17, 0)));
        let numbers = feed(
            None,
            Some(FeedLimit {
                percent: 12.0,
                resets_at: at("2026-11-14T19:30:00.336716Z"),
            }),
        );

        assert_eq!(account_for(&numbers, &cache, &accounts), None);
    }

    #[test]
    fn merged_replaces_only_session_and_week() {
        let usage = Usage {
            limits: vec![
                Limit {
                    kind: LimitKind::Session,
                    percent: 10.0,
                    severity: Severity::Normal,
                    resets_at: Some(at("2026-11-14T19:30:00Z")),
                    model: None,
                    is_active: true,
                },
                Limit {
                    kind: LimitKind::WeeklyAll,
                    percent: 40.0,
                    severity: Severity::Normal,
                    resets_at: Some(weekday(16, 2, 0)),
                    model: None,
                    is_active: false,
                },
                Limit {
                    kind: LimitKind::WeeklyScoped,
                    percent: 21.0,
                    severity: Severity::Warning,
                    resets_at: Some(weekday(16, 2, 0)),
                    model: Some("Fable".to_string()),
                    is_active: true,
                },
            ],
            extra_usage_enabled: true,
        };
        let numbers = feed(
            Some(FeedLimit {
                percent: 48.0,
                resets_at: weekday(16, 2, 0),
            }),
            Some(FeedLimit {
                percent: 12.0,
                resets_at: at("2026-11-14T19:30:00Z"),
            }),
        );

        let merged = merged(&usage, &numbers);

        assert_eq!(merged.session().map(|limit| limit.percent), Some(12.0));
        assert_eq!(merged.weekly_all().map(|limit| limit.percent), Some(48.0));
        let scoped: Vec<&Limit> = merged.weekly_scoped().collect();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].percent, 21.0);
        assert_eq!(scoped[0].model.as_deref(), Some("Fable"));
        assert!(scoped[0].is_active);
        assert!(!merged.weekly_all().expect("the week limit").is_active);
        assert!(merged.extra_usage_enabled);
    }

    #[test]
    fn a_feed_of_a_new_session_window_is_taken() {
        let cached_reset = at("2026-11-14T19:30:00Z");
        let new_reset = at("2026-11-15T01:30:00Z");
        let usage = Usage {
            limits: vec![
                Limit {
                    kind: LimitKind::Session,
                    percent: 10.0,
                    severity: Severity::Normal,
                    resets_at: Some(cached_reset),
                    model: None,
                    is_active: true,
                },
                Limit {
                    kind: LimitKind::WeeklyAll,
                    percent: 40.0,
                    severity: Severity::Normal,
                    resets_at: Some(weekday(16, 2, 0)),
                    model: None,
                    is_active: false,
                },
            ],
            extra_usage_enabled: false,
        };
        let numbers = feed(
            Some(FeedLimit {
                percent: 40.0,
                resets_at: weekday(16, 2, 0),
            }),
            Some(FeedLimit {
                percent: 5.0,
                resets_at: new_reset,
            }),
        );

        let merged = merged(&usage, &numbers);

        let session = merged.session().expect("the session limit");
        assert_eq!(session.percent, 5.0);
        assert_eq!(session.resets_at, Some(new_reset));
    }

    #[test]
    fn write_if_due_skips_an_unchanged_feed_within_ten_seconds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = AppPaths::at(&tmp.path().join("data"));
        let numbers = feed(
            Some(FeedLimit {
                percent: 48.0,
                resets_at: weekday(16, 2, 0),
            }),
            None,
        );

        assert!(write_if_due(&app, &numbers).expect("the first write"));

        let mut same = numbers.clone();
        same.at += chrono::Duration::seconds(3);
        assert!(!write_if_due(&app, &same).expect("the same numbers at three seconds"));

        let mut later = numbers.clone();
        later.at += chrono::Duration::seconds(10);
        assert!(write_if_due(&app, &later).expect("the same numbers at ten seconds"));

        // Other numbers one second after a file written at +0 s are written at
        // once: the numbers, not the due time, decide.
        let other_app = AppPaths::at(&tmp.path().join("other-data"));
        assert!(write_if_due(&other_app, &numbers).expect("the first write"));
        let mut other = numbers.clone();
        other.at += chrono::Duration::seconds(1);
        other.seven_day = Some(FeedLimit {
            percent: 49.0,
            resets_at: weekday(16, 2, 0),
        });
        assert!(write_if_due(&other_app, &other).expect("different numbers"));
    }

    #[test]
    fn a_stored_feed_from_the_future_does_not_block_writing_forever() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = AppPaths::at(&tmp.path().join("data"));
        let numbers = feed(
            Some(FeedLimit {
                percent: 48.0,
                resets_at: weekday(16, 2, 0),
            }),
            None,
        );

        let mut future = numbers.clone();
        future.at += chrono::Duration::hours(1);
        assert!(write_if_due(&app, &future).expect("the future feed is written"));

        let mut earlier = numbers.clone();
        earlier.at += chrono::Duration::minutes(30);
        assert!(
            write_if_due(&app, &earlier).expect("a clock set back writes again"),
            "a stored feed later than this one must not block the write"
        );
        let stored = read(&app).expect("the feed file").expect("the feed");
        assert_eq!(stored.at, earlier.at);
    }
}
