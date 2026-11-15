//! Claude Code's status line: the payload it writes to standard input (spec
//! §5.2), the numbers seatswap fills in, and the layout it renders (spec §5.4).
//!
//! `parse` is the one place a malformed payload is tolerated: Claude Code may
//! send anything, and the status line must still print. `facts` gathers what
//! the line shows besides the payload; `render` fits it to the terminal.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use seatswap_core::api::Usage;
use seatswap_core::engine::UsageCache;
use seatswap_core::paths;
use seatswap_core::store::{Account, AccountsFile};
use seatswap_core::switcher::LiveState;

pub(crate) mod render;
pub(crate) mod width;

use render::{git_branch, AccountFacts, Facts, Limit2};

/// The parts of the payload seatswap reads. Unknown keys are ignored: Claude
/// Code adds keys with new versions. A key that is missing or `null` is `None`.
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Deserialize, Default)]
pub(crate) struct Payload {
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
/// this shape gives [`Payload::default()`]: the line must print whatever the
/// input was. A present key of the wrong type is an error for the whole
/// payload (the `RateLimit` fields are not optional), so it is defaulted too.
// used by the statusline command (step B, Task 2)
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse(stdin: &[u8]) -> Payload {
    serde_json::from_slice(stdin).unwrap_or_default()
}

/// The 5 h and weekly numbers and the per-model limits the line shows.
///
/// The payload's `rate_limits` win; without them the live account's cache entry
/// fills in (session → five_hour, weekly_all → week, weekly_scoped →
/// model_limits). The per-model limits only exist in the cache.
// replaced by feed::merged in step B, Task 3
fn limits(
    payload: &Payload,
    cached: Option<&Usage>,
    _now: DateTime<Utc>,
) -> (Option<Limit2>, Option<Limit2>, Vec<(String, f64)>) {
    let payload_five_hour = payload
        .rate_limits
        .as_ref()
        .and_then(|limits| limits.five_hour.as_ref());
    let payload_week = payload
        .rate_limits
        .as_ref()
        .and_then(|limits| limits.seven_day.as_ref());
    let five_hour = match payload_five_hour {
        Some(limit) => Some(Limit2 {
            percent: limit.used_percentage,
            resets_at: DateTime::<Utc>::from_timestamp(limit.resets_at, 0),
        }),
        None => cached.and_then(Usage::session).map(cached_limit),
    };
    let week = match payload_week {
        Some(limit) => Some(Limit2 {
            percent: limit.used_percentage,
            resets_at: DateTime::<Utc>::from_timestamp(limit.resets_at, 0),
        }),
        None => cached.and_then(Usage::weekly_all).map(cached_limit),
    };
    let model_limits = cached
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
        .unwrap_or_default();
    (five_hour, week, model_limits)
}

fn cached_limit(limit: &seatswap_core::api::Limit) -> Limit2 {
    Limit2 {
        percent: limit.percent,
        resets_at: limit.resets_at,
    }
}

/// What the line shows besides the payload: who and where, the live account,
/// its limits, and the branch of the session's folder. A live login that
/// matches no stored account shows no account (and no limits from the cache);
/// no live login at all is an error the command prints as its one line.
pub(crate) fn facts(
    payload: &Payload,
    file: &AccountsFile,
    live: &LiveState,
    cache: &UsageCache,
    home: &std::path::Path,
    user: String,
    now: DateTime<Utc>,
) -> Result<Facts, String> {
    let host =
        paths::host_name(file.settings.host_name.as_deref()).map_err(|error| error.to_string())?;
    let (account, cached) = match live {
        LiveState::Known { account_id } => {
            let account: &Account = file
                .find(account_id)
                .ok_or_else(|| format!("the live account {account_id} is not in accounts.json"))?;
            let cached = cache
                .entries
                .get(account_id)
                .and_then(|entry| entry.usage.as_ref());
            (
                Some(AccountFacts {
                    abbr: account.abbr.clone(),
                    color: account.color.clone(),
                    plan: account.plan.clone(),
                }),
                cached,
            )
        }
        LiveState::Unknown { .. } => (None, None),
        LiveState::NoLogin => return Err("no live login; run seatswap add".to_string()),
        LiveState::Ended { .. } => return Err("the live login has ended; log in again".to_string()),
    };
    let (five_hour, week, model_limits) = limits(payload, cached, now);
    let git_branch = payload
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.current_dir.as_deref())
        .and_then(|folder| git_branch(std::path::Path::new(folder)));
    Ok(Facts {
        user,
        host,
        home: home.display().to_string(),
        now,
        account,
        five_hour,
        week,
        model_limits,
        // Auto-burn is built in step B, Tasks 4–8; there is no next account yet.
        burn_next: None,
        git_branch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status line payload of `specs/plan/README.md` §5, whole.
    const README_PAYLOAD: &str = r#"{"workspace":{"current_dir":"/home/sam/projects/atlas"},"model":{"id":"claude-fable-5-2","display_name":"Fable 5.2"},"effort":{"level":"high"},"rate_limits":{"five_hour":{"used_percentage":12,"resets_at":1794490200},"seven_day":{"used_percentage":48,"resets_at":1794790800}}}"#;

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
}
