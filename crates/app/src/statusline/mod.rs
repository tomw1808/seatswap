//! Claude Code's status line: the payload it writes to standard input (spec
//! §5.2) and the layout seatswap renders from it (spec §5.4).
//!
//! `parse` is the one place a malformed payload is tolerated: Claude Code may
//! send anything, and the status line must still print. Rendering is step B,
//! Task 2; until then the test build is the only user.

use serde::Deserialize;

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
