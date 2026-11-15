//! Claude Code's status line: the payload it writes to standard input (spec
//! §5.2), the numbers seatswap fills in, and the layout it renders (spec §5.4).
//!
//! `parse` is the one place a malformed payload is tolerated: Claude Code may
//! send anything, and the status line must still print. `facts` gathers what
//! the line shows besides the payload; `render_spans` fits it to the terminal
//! and `ansi` turns the spans into the command's one line.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use seatswap_core::api::Usage;
use seatswap_core::burn::burn_next;
use seatswap_core::engine::UsageCache;
use seatswap_core::feed::{self, Feed, FeedLimit};
use seatswap_core::paths::{self, AppPaths};
use seatswap_core::store::{Account, AccountsFile, SegmentKind, Settings, StatuslineConfig};
use seatswap_core::switcher::LiveState;

use crate::gui::ipc;

pub(crate) mod render;
pub(crate) mod width;

use render::{git_branch, AccountFacts, Facts, Limit2, Span};

/// The status line payload of `specs/plan/README.md` §5, whole. The preview
/// uses this one payload for every scenario.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const README_PAYLOAD: &str = r#"{"workspace":{"current_dir":"/home/sam/projects/atlas"},"model":{"id":"claude-fable-5-2","display_name":"Fable 5.2"},"effort":{"level":"high"},"rate_limits":{"five_hour":{"used_percentage":12,"resets_at":1794490200},"seven_day":{"used_percentage":48,"resets_at":1794790800}}}"#;

/// The parts of the payload seatswap reads. Unknown keys are ignored: Claude
/// Code adds keys with new versions. A key that is missing or `null` is `None`.
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Deserialize, Default)]
pub(crate) struct Payload {
    /// The whole payload as Claude Code sent it, for the fields that are not
    /// typed above. `Null` when standard input was not JSON.
    #[serde(skip)]
    pub raw: serde_json::Value,
    pub workspace: Option<Workspace>,
    pub model: Option<Model>,
    pub effort: Option<Effort>,
    pub rate_limits: Option<RateLimits>,
}

/// `workspace.current_dir`: the session's folder.
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Deserialize)]
pub(crate) struct Workspace {
    pub current_dir: Option<String>,
}

/// The model's id (coloured by family) and display name (printed).
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Deserialize)]
pub(crate) struct Model {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

/// The current effort level (`low`, `medium`, `high`, `xhigh`, `max`).
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Deserialize)]
pub(crate) struct Effort {
    pub level: Option<String>,
}

/// The live limits of the session's account; absent until its first answer.
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Deserialize)]
pub(crate) struct RateLimits {
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
}

/// One limit: a whole percent and the reset in seconds since the epoch.
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Deserialize)]
pub(crate) struct RateLimit {
    pub used_percentage: f64,
    pub resets_at: i64,
}

/// Claude Code's JSON to a [`Payload`]. Anything that is not a JSON object of
/// this shape gives the default typed fields: the line must print whatever the
/// input was. `raw` keeps what was parsed, or `null` when it was not JSON. A
/// present key of the wrong type is an error for the whole typed payload (the
/// `RateLimit` fields are not optional), so it is defaulted too.
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse(stdin: &[u8]) -> Payload {
    let raw: serde_json::Value = serde_json::from_slice(stdin).unwrap_or_default();
    let mut payload: Payload = serde_json::from_value(raw.clone()).unwrap_or_default();
    payload.raw = raw;
    payload
}

/// The feed a payload carries: its 5 h and weekly numbers and its model, at
/// `now`. `None` when the payload has no `rate_limits` at all (before a
/// session's first answer there is nothing to feed), or when it has no
/// `seven_day`: a feed without a week cannot be matched to an account and would
/// replace a complete `feed.json` with a partial one.
pub(crate) fn feed_from_payload(payload: &Payload, now: DateTime<Utc>) -> Option<Feed> {
    let limits = payload.rate_limits.as_ref()?;
    let seven_day = limits.seven_day.as_ref().and_then(feed_limit)?;
    let five_hour = limits.five_hour.as_ref().and_then(feed_limit);
    Some(Feed {
        at: now,
        five_hour,
        seven_day: Some(seven_day),
        model: payload.model.as_ref().and_then(|model| model.id.clone()),
    })
}

/// One payload limit as a feed limit. A reset time chrono cannot represent is
/// left out, never replaced by another time.
fn feed_limit(limit: &RateLimit) -> Option<FeedLimit> {
    Some(FeedLimit {
        percent: limit.used_percentage,
        resets_at: DateTime::<Utc>::from_timestamp(limit.resets_at, 0)?,
    })
}

/// Write the payload's live numbers to `feed.json` when the setting is on, and
/// tell a running tray app when it wrote. The status line calls this before it
/// renders, so a failure becomes the line's `feed: <reason>`.
pub(crate) fn publish_feed(
    app: &AppPaths,
    settings: &Settings,
    payload: &Payload,
    now: DateTime<Utc>,
) -> Result<(), String> {
    if !settings.statusline_feed {
        return Ok(());
    }
    let Some(feed) = feed_from_payload(payload, now) else {
        return Ok(());
    };
    if !feed::write_if_due(app, &feed)
        .map_err(|error| format!("cannot write {}: {error}", app.feed_file().display()))?
    {
        return Ok(());
    }
    ipc::send_feed(app).map_err(|error| error.to_string())
}

fn cached_limit(limit: &seatswap_core::api::Limit) -> Limit2 {
    Limit2 {
        percent: limit.percent,
        resets_at: limit.resets_at,
    }
}

/// The next account the tray app wrote to `burn-next.json`, named through the
/// caller's lookup: the command looks in its accounts file, the preview in the
/// snapshot's views. The tray app writes the file only while auto-burn is on
/// and unpaused; a missing or stale file is no value. An id the lookup does not
/// know is no value either, never a guessed abbreviation.
pub(crate) fn burn_next_account(
    app: &AppPaths,
    now: DateTime<Utc>,
    abbr_of: impl Fn(&str) -> Option<String>,
) -> Result<Option<(String, DateTime<Utc>)>, String> {
    match burn_next::read(app, now) {
        Ok(Some(next)) => Ok(abbr_of(&next.account).map(|abbr| (abbr, next.at))),
        Ok(None) => Ok(None),
        Err(error) => Err(format!(
            "cannot read {}: {error}",
            app.burn_next_file().display()
        )),
    }
}

/// Where the numbers besides the payload come from: the status line command
/// reads `accounts.json` and `usage-cache.json`; the window preview reads the
/// current snapshot. Both go through [`facts_from_source`].
pub(crate) trait FactsSource {
    fn host_name(&self) -> Option<&str>;
    fn account(&self, account_id: &str) -> Option<AccountFacts>;
    fn usage(&self, account_id: &str, feed: Option<&Feed>) -> Option<Usage>;
    fn model_limits(&self, account_id: &str) -> Vec<(String, f64)>;
    fn burn_next(&self, now: DateTime<Utc>) -> Result<Option<(String, DateTime<Utc>)>, String>;
}

/// The command's source: the files it read without a lock.
struct AccountsFacts<'a> {
    app: &'a AppPaths,
    file: &'a AccountsFile,
    cache: &'a UsageCache,
}

impl FactsSource for AccountsFacts<'_> {
    fn host_name(&self) -> Option<&str> {
        self.file.settings.host_name.as_deref()
    }

    fn account(&self, account_id: &str) -> Option<AccountFacts> {
        let account: &Account = self.file.find(account_id)?;
        Some(AccountFacts {
            abbr: account.abbr.clone(),
            color: account.color.clone(),
            plan: account.plan.clone(),
        })
    }

    fn usage(&self, account_id: &str, feed: Option<&Feed>) -> Option<Usage> {
        let cached = self
            .cache
            .entries
            .get(account_id)
            .and_then(|entry| entry.usage.as_ref())?;
        // The payload's numbers are shown only when they belong to the live
        // account. A fresh session that has not sent a request since a switch
        // carries the old account's numbers; they are never printed under the
        // new account's letters. Without a cache entry there is nothing to
        // compare.
        let belongs_to_live = feed.is_some_and(|feed| {
            feed::account_for(feed, self.cache, &self.file.accounts).as_deref() == Some(account_id)
        });
        Some(match (feed, belongs_to_live) {
            (Some(feed), true) => feed::merged(cached, feed),
            _ => cached.clone(),
        })
    }

    fn model_limits(&self, account_id: &str) -> Vec<(String, f64)> {
        self.cache
            .entries
            .get(account_id)
            .and_then(|entry| entry.usage.as_ref())
            .map(|usage| {
                usage
                    .weekly_scoped()
                    .filter_map(|limit| {
                        limit
                            .model
                            .as_ref()
                            .map(|model| (model.clone(), limit.percent))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn burn_next(&self, now: DateTime<Utc>) -> Result<Option<(String, DateTime<Utc>)>, String> {
        // A file the tray app wrote names an account in this same accounts.json;
        // an id that is no longer there is no value, never a guessed
        // abbreviation.
        burn_next_account(self.app, now, |account_id| {
            self.file
                .accounts
                .iter()
                .find(|account| account.id == account_id)
                .map(|account| account.abbr.clone())
        })
    }
}

/// What the line shows besides the payload: who and where, the live account,
/// its limits, and the branch of the session's folder. A live login that
/// matches no stored account shows no account (and no limits from the cache);
/// no live login at all is an error the command prints as its one line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn facts(
    app: &AppPaths,
    payload: &Payload,
    file: &AccountsFile,
    live: &LiveState,
    cache: &UsageCache,
    home: &std::path::Path,
    user: String,
    now: DateTime<Utc>,
) -> Result<Facts, String> {
    let source = AccountsFacts { app, file, cache };
    facts_from_source(&source, payload, live, home, user, now)
}

/// The one builder of [`Facts`]. The command and the preview differ only in
/// their [`FactsSource`].
pub(crate) fn facts_from_source<S: FactsSource>(
    source: &S,
    payload: &Payload,
    live: &LiveState,
    home: &std::path::Path,
    user: String,
    now: DateTime<Utc>,
) -> Result<Facts, String> {
    let host = paths::host_name(source.host_name()).map_err(|error| error.to_string())?;
    let feed = feed_from_payload(payload, now);
    let (account, shown, live_id) = match live {
        LiveState::Known { account_id } => {
            let account = source
                .account(account_id)
                .ok_or_else(|| format!("the live account {account_id} is not in accounts.json"))?;
            let shown = source.usage(account_id, feed.as_ref());
            (Some(account), shown, Some(account_id.as_str()))
        }
        LiveState::Unknown { .. } => (None, None, None),
        LiveState::NoLogin => return Err("no live login; run seatswap add".to_string()),
        LiveState::Ended { .. } => return Err("the live login has ended; log in again".to_string()),
    };
    let five_hour = shown.as_ref().and_then(Usage::session).map(cached_limit);
    let week = shown.as_ref().and_then(Usage::weekly_all).map(cached_limit);
    // The per-model limits only exist in the cache.
    let model_limits = match live_id {
        Some(account_id) => source.model_limits(account_id),
        None => Vec::new(),
    };
    let git_branch = payload
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.current_dir.as_deref())
        .and_then(|folder| git_branch(std::path::Path::new(folder)));
    let burn_next = source.burn_next(now)?;
    Ok(Facts {
        user,
        host,
        home: home.display().to_string(),
        now,
        account,
        five_hour,
        week,
        model_limits,
        burn_next,
        git_branch,
    })
}

/// One test case of the preview: the live data, or one number changed.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Scenario {
    Live,
    NearFull,
    Blocked,
    NoSessionLimit,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Scenario {
    /// This scenario's numbers. Each case changes only the one value it names.
    fn facts(self, mut facts: Facts) -> Facts {
        match self {
            Scenario::Live => facts,
            Scenario::NearFull => {
                let resets_at = facts.week.as_ref().and_then(|week| week.resets_at);
                facts.week = Some(Limit2 {
                    percent: 97.0,
                    resets_at,
                });
                facts
            }
            Scenario::Blocked => {
                let resets_at = facts.week.as_ref().and_then(|week| week.resets_at);
                facts.week = Some(Limit2 {
                    percent: 100.0,
                    resets_at,
                });
                facts
            }
            Scenario::NoSessionLimit => {
                facts.five_hour = None;
                facts
            }
        }
    }
}

/// One width of the preview: the fitted spans, the drop numbers removed, and
/// the sentence about them.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewLine {
    pub width: u16,
    pub spans: Vec<Span>,
    pub dropped: Vec<u8>,
    pub note: String,
}

/// Render `config` (never the stored one) once per width. A width of 0 or an
/// empty list is an error: the preview must show a line, not silently skip it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn preview_lines<Tz: TimeZone>(
    config: &StatuslineConfig,
    payload: &Payload,
    facts: &Facts,
    widths: &[u16],
    scenario: Scenario,
    tz: &Tz,
) -> Result<Vec<PreviewLine>, String>
where
    Tz::Offset: std::fmt::Display,
{
    if widths.is_empty() {
        return Err("the preview needs at least one width".to_string());
    }
    if widths.contains(&0) {
        return Err("a preview width must be at least 1 column; 0 cannot show a line".to_string());
    }
    let facts = scenario.facts(facts.clone());
    let mut lines = Vec::with_capacity(widths.len());
    for &width in widths {
        let fitted = render::fitted_line(config, payload, &facts, Some(width), tz);
        lines.push(PreviewLine {
            width,
            note: dropped_note(&fitted.dropped_kinds),
            spans: fitted.spans,
            dropped: fitted.dropped,
        });
    }
    Ok(lines)
}

/// What a preview says about the segments the fitting dropped, one name per
/// kind.
#[cfg_attr(not(test), allow(dead_code))]
fn dropped_note(dropped: &[SegmentKind]) -> String {
    let mut names: Vec<&'static str> = Vec::new();
    for kind in dropped {
        let name = kind_name(*kind);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let Some((last, rest)) = names.split_last() else {
        return "everything fits".to_string();
    };
    let list = if rest.is_empty() {
        (*last).to_string()
    } else {
        format!("{} and {last}", rest.join(", "))
    };
    let is = if names.len() == 1 { "is" } else { "are" };
    format!("{list} {is} dropped")
}

/// The name a dropped segment is called in the preview's note.
#[cfg_attr(not(test), allow(dead_code))]
fn kind_name(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Account => "account",
        SegmentKind::Plan => "plan",
        SegmentKind::FiveHourPercent => "5 h percent",
        SegmentKind::FiveHourReset => "5 h reset",
        SegmentKind::WeekPercent => "week percent",
        SegmentKind::WeekReset => "week reset",
        SegmentKind::ModelPercent => "per-model percent",
        SegmentKind::AutoBurn => "auto-burn",
        SegmentKind::Model => "model",
        SegmentKind::Effort => "effort",
        SegmentKind::Folder => "folder",
        SegmentKind::GitBranch => "branch",
        SegmentKind::UserHost => "user@host",
        SegmentKind::Clock => "clock",
        SegmentKind::Text => "text",
        SegmentKind::Separator => "separator",
        SegmentKind::ContextUsed => "context used",
        SegmentKind::SessionCost => "session cost",
        SegmentKind::LinesChanged => "lines changed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_of_the_readme_parses() {
        let payload = parse(README_PAYLOAD.as_bytes());
        assert_eq!(
            payload
                .workspace
                .and_then(|workspace| workspace.current_dir),
            Some("/home/sam/projects/atlas".to_string())
        );
        let model = payload.model.expect("the payload has a model");
        assert_eq!(model.id.as_deref(), Some("claude-fable-5-2"));
        assert_eq!(model.display_name.as_deref(), Some("Fable 5.2"));
        assert_eq!(
            payload.effort.and_then(|effort| effort.level),
            Some("high".to_string())
        );
        let rate_limits = payload.rate_limits.expect("the payload has rate limits");
        let five_hour = rate_limits.five_hour.expect("the payload has a 5 h limit");
        assert_eq!(five_hour.used_percentage, 12.0);
        assert_eq!(five_hour.resets_at, 1794490200);
        let seven_day = rate_limits.seven_day.expect("the payload has a week limit");
        assert_eq!(seven_day.used_percentage, 48.0);
        assert_eq!(seven_day.resets_at, 1794790800);
    }

    #[test]
    fn garbage_is_an_empty_payload() {
        for stdin in [&b"not json"[..], &b""[..]] {
            let payload = parse(stdin);
            assert!(payload.workspace.is_none());
            assert!(payload.model.is_none());
            assert!(payload.effort.is_none());
            assert!(payload.rate_limits.is_none());
        }
    }

    /// A field of the wrong type is an error for the whole payload: the
    /// fields of `RateLimit` are not optional, so serde cannot make only that
    /// one `None`. The payload of the task itself has a whole number for
    /// `used_percentage`; that is a `f64` of 12.0.
    #[test]
    fn a_payload_that_does_not_parse_gives_the_default_payload() {
        for stdin in [
            &br#"{"workspace":{"current_dir":"/home/sam/projects/atlas"},"model":{"id":"claude-fable-5-2"},"rate_limits":{"five_hour":{"used_percentage":"12","resets_at":1794490200}}}"#[..],
            &br#"{"model":{"id":"claude-fable-5-2"},"rate_limits":{"seven_day":{"used_percentage":48,"resets_at":99999999999999999999}}}"#[..],
            &br#"{"rate_limits":{"five_hour":{"used_percentage":48}}}"#[..],
        ] {
            let payload = parse(stdin);
            assert!(payload.workspace.is_none());
            assert!(payload.model.is_none());
            assert!(payload.effort.is_none());
            assert!(payload.rate_limits.is_none());
        }
    }

    use std::fs;

    use seatswap_core::api::{Limit, LimitKind, Severity};
    use seatswap_core::engine::CacheEntry;
    use seatswap_core::store::{
        ColorRule, LoginState, Segment, SegmentKind, StatuslineConfig, TimeFormat,
    };
    use seatswap_core::types::OAuthAccount;

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
            price: None,
            price_currency: None,
        }
    }

    fn usage(session: f64, week: f64, reset: DateTime<Utc>) -> Usage {
        Usage {
            limits: vec![
                Limit {
                    kind: LimitKind::Session,
                    percent: session,
                    severity: Severity::Normal,
                    resets_at: Some(at("2026-11-14T19:30:00.336716Z")),
                    model: None,
                    is_active: true,
                },
                Limit {
                    kind: LimitKind::WeeklyAll,
                    percent: week,
                    severity: Severity::Normal,
                    resets_at: Some(reset),
                    model: None,
                    is_active: false,
                },
            ],
            extra_usage_enabled: false,
        }
    }

    #[test]
    fn the_numbers_of_another_account_are_not_shown() {
        let now = at("2026-11-14T14:00:00.336716Z");
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let app = AppPaths::at(&data);
        let lb_reset = at("2026-11-16T17:00:00.336716Z");
        let ac_reset = at("2026-11-16T02:00:00.336716Z");
        let mut file = AccountsFile::default();
        file.accounts.push(account(LB_ID, "LB", LB_UUID));
        file.accounts.push(account(AC_ID, "AC", AC_UUID));
        let mut cache = UsageCache::default();
        cache.entries.insert(
            LB_ID.to_string(),
            CacheEntry {
                usage: Some(usage(10.0, 40.0, lb_reset)),
                fetched_at: Some(now),
                ..CacheEntry::default()
            },
        );
        cache.entries.insert(
            AC_ID.to_string(),
            CacheEntry {
                usage: Some(usage(30.0, 70.0, ac_reset)),
                fetched_at: Some(now),
                ..CacheEntry::default()
            },
        );
        // The session still carries AC's numbers although LB is live.
        let payload = parse(
            format!(
                r#"{{"workspace":{{"current_dir":"/home/sam/projects/atlas"}},"model":{{"id":"claude-fable-5-2","display_name":"Fable 5.2"}},"effort":{{"level":"high"}},"rate_limits":{{"five_hour":{{"used_percentage":99,"resets_at":{}}},"seven_day":{{"used_percentage":99,"resets_at":{}}}}}}}"#,
                ac_reset.timestamp(),
                ac_reset.timestamp()
            )
            .as_bytes(),
        );

        let facts = facts(
            &app,
            &payload,
            &file,
            &LiveState::Known {
                account_id: LB_ID.to_string(),
            },
            &cache,
            std::path::Path::new("/home/sam"),
            "sam".to_string(),
            now,
        )
        .expect("the facts of the live account");

        assert_eq!(facts.account.expect("the live account").abbr, "LB");
        assert_eq!(facts.five_hour.expect("LB's session").percent, 10.0);
        assert_eq!(facts.week.expect("LB's week").percent, 40.0);
    }

    #[test]
    fn a_payload_without_a_week_is_not_a_feed() {
        let payload = parse(
            br#"{"rate_limits":{"five_hour":{"used_percentage":12,"resets_at":1794490200}}}"#,
        );

        assert!(
            feed_from_payload(&payload, at("2026-11-14T14:00:00.336716Z")).is_none(),
            "a feed without a week cannot be placed on an account"
        );
    }

    #[test]
    fn the_live_accounts_feed_is_shown_merged() {
        let now = at("2026-11-14T14:00:00.336716Z");
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let app = AppPaths::at(&data);
        let ac_reset = at("2026-11-16T02:00:00.336716Z");
        let session_reset = at("2026-11-14T19:30:00.336716Z");
        let mut file = AccountsFile::default();
        file.accounts.push(account(AC_ID, "AC", AC_UUID));
        let mut cache = UsageCache::default();
        cache.entries.insert(
            AC_ID.to_string(),
            CacheEntry {
                usage: Some(usage(10.0, 40.0, ac_reset)),
                fetched_at: Some(now),
                ..CacheEntry::default()
            },
        );
        let payload = parse(
            format!(
                r#"{{"workspace":{{"current_dir":"/home/sam/projects/atlas"}},"model":{{"id":"claude-fable-5-2","display_name":"Fable 5.2"}},"effort":{{"level":"high"}},"rate_limits":{{"five_hour":{{"used_percentage":12,"resets_at":{}}},"seven_day":{{"used_percentage":48,"resets_at":{}}}}}}}"#,
                session_reset.timestamp(),
                ac_reset.timestamp()
            )
            .as_bytes(),
        );

        let facts = facts(
            &app,
            &payload,
            &file,
            &LiveState::Known {
                account_id: AC_ID.to_string(),
            },
            &cache,
            std::path::Path::new("/home/sam"),
            "sam".to_string(),
            now,
        )
        .expect("the facts of the live account");

        assert_eq!(facts.week.as_ref().expect("AC's week").percent, 48.0);
        let line = render::ansi(
            &render::render_spans(
                &StatuslineConfig::current_line(),
                &payload,
                &facts,
                None,
                &Utc,
            )
            .0,
        );
        assert!(line.contains("AC"), "{line}");
        assert!(
            line.contains("48%"),
            "the newer week is on the line: {line}"
        );
        assert!(!line.contains("40%"), "the cached week is replaced: {line}");
    }

    #[test]
    fn the_auto_burn_segment_shows_the_next_account() {
        let now = at("2026-11-14T14:00:00Z");
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let app = AppPaths::at(&data);
        let mut file = AccountsFile::default();
        file.accounts.push(account(AC_ID, "AC", AC_UUID));
        let switch_at = at("2026-11-14T16:05:00Z");
        burn_next::write(
            &app,
            &burn_next::BurnNext {
                account: AC_ID.to_string(),
                at: switch_at,
                written: now,
            },
        )
        .expect("burn-next.json");

        let payload = parse(README_PAYLOAD.as_bytes());
        let facts = facts(
            &app,
            &payload,
            &file,
            &LiveState::Known {
                account_id: AC_ID.to_string(),
            },
            &UsageCache::default(),
            std::path::Path::new("/home/sam"),
            "sam".to_string(),
            now,
        )
        .expect("the facts of the live account");
        assert_eq!(facts.burn_next, Some(("AC".to_string(), switch_at)));

        let config = StatuslineConfig {
            left: Vec::new(),
            right: vec![Segment {
                kind: SegmentKind::AutoBurn,
                color: ColorRule::None,
                prefix: String::new(),
                suffix: String::new(),
                drop: None,
                time_format: Some(TimeFormat::Seatswap),
                text: None,
            }],
            margin: 0,
        };
        assert_eq!(
            render::ansi(&render::render_spans(&config, &payload, &facts, None, &Utc).0),
            "AC 2h05m"
        );
    }

    #[test]
    fn an_old_next_file_shows_nothing() {
        let now = at("2026-11-14T14:00:00Z");
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let app = AppPaths::at(&data);
        let mut file = AccountsFile::default();
        file.accounts.push(account(AC_ID, "AC", AC_UUID));
        burn_next::write(
            &app,
            &burn_next::BurnNext {
                account: AC_ID.to_string(),
                at: now + chrono::Duration::hours(2),
                written: now - chrono::Duration::minutes(21),
            },
        )
        .expect("burn-next.json");

        let payload = parse(README_PAYLOAD.as_bytes());
        let facts = facts(
            &app,
            &payload,
            &file,
            &LiveState::Known {
                account_id: AC_ID.to_string(),
            },
            &UsageCache::default(),
            std::path::Path::new("/home/sam"),
            "sam".to_string(),
            now,
        )
        .expect("the facts of the live account");
        assert_eq!(facts.burn_next, None, "the tray app is not running");
    }

    #[test]
    fn no_feed_file_while_the_setting_is_off() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = AppPaths::at(&tmp.path().join("data"));
        let settings = Settings {
            statusline_feed: false,
            ..Settings::default()
        };

        publish_feed(
            &app,
            &settings,
            &parse(README_PAYLOAD.as_bytes()),
            at("2026-11-14T14:00:00.336716Z"),
        )
        .expect("the setting simply writes nothing");

        assert!(!app.feed_file().exists(), "no feed file was written");
    }

    #[test]
    fn a_feed_that_cannot_be_written_is_the_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).expect("the data folder");
        fs::write(
            data.join("accounts.json"),
            serde_json::to_vec(&AccountsFile::default()).expect("accounts.json"),
        )
        .expect("the account file");
        // A folder where feed.json should be: the write must be reported, not
        // skipped and not hidden behind a rendered line.
        fs::create_dir_all(data.join("feed.json")).expect("the blocking folder");
        let app = AppPaths::at(&data);
        let claude =
            seatswap_core::paths::ClaudePaths::for_config_dir(&tmp.path().join("cfg"), tmp.path());

        let line = crate::cli::tests::statusline_output_at(
            &app,
            &claude,
            README_PAYLOAD.as_bytes(),
            at("2026-11-14T14:00:00.336716Z"),
        );

        assert!(line.starts_with("seatswap: feed: "), "{line}");
    }
}
