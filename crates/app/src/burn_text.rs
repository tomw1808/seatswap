//! Every text auto-burn shows (spec §2.5, §2.12): the tray app's line and
//! notifications, the suggestion, its reasons and the regime's "why". The CLI
//! uses the same functions, so the two front ends cannot drift.

#[cfg(feature = "gui")]
use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, Utc};
use serde::Serialize;

use seatswap_core::burn::decide::RegimeWhy;
use seatswap_core::burn::decide::{Decision, Reason};
#[cfg(feature = "gui")]
use seatswap_core::burn::plan::Fill;
use seatswap_core::burn::plan::{Plan, Policy};
use seatswap_core::burn::replay::ReplayResult;
use seatswap_core::engine::AccountView;
use seatswap_core::history::AccountRef;
use seatswap_core::store::Account;

use crate::format;

/// A text that cannot be written from the values at hand. Every variant names
/// the account it is about; the callers put the error into the snapshot or the
/// command instead of guessing a name or a time.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("account {account}: no view")]
    ViewMissing { account: String },
    #[error("account {account}: no weekly reset")]
    ResetMissing { account: String },
    #[cfg(feature = "gui")]
    #[error("account {account}: the plan does not say when it fills")]
    FillMissing { account: String },
    #[error("no active account to name")]
    NoActive,
}

/// A suggested switch: the account to switch to and the whole sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchSuggestion {
    pub to: String,
    pub text: String,
}

/// The account's abbreviation. An id without a view is an error, never the id
/// (or anything else) guessed in its place.
pub fn abbr(views: &[AccountView], id: &str) -> Result<String, TextError> {
    view(views, id)
        .map(|view| view.abbr.clone())
        .ok_or_else(|| TextError::ViewMissing {
            account: id.to_string(),
        })
}

fn view<'a>(views: &'a [AccountView], id: &str) -> Option<&'a AccountView> {
    views.iter().find(|view| view.id == id)
}

/// The line while an account auto-burn may use, or the active one, has a login
/// and no reading yet: its abbreviation, in the order `accounts` gives.
#[cfg(feature = "gui")]
pub fn unread_line(views: &[AccountView], accounts: &[String]) -> Result<String, TextError> {
    let names = accounts
        .iter()
        .map(|id| abbr(views, id))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    Ok(format!("Waiting for a first reading of {names}."))
}

/// The line when no account auto-burn may use (nor the active one) has a
/// working login and a reading: every such login ended, or auto-burn may use
/// no account.
#[cfg(feature = "gui")]
pub fn no_accounts_line() -> &'static str {
    "No account auto-burn may use has a working login."
}

/// The target's weekly reset, in the words a reason needs.
fn weekly_reset(views: &[AccountView], id: &str) -> Result<DateTime<Utc>, TextError> {
    view(views, id)
        .ok_or_else(|| TextError::ViewMissing {
            account: id.to_string(),
        })?
        .usage
        .as_ref()
        .and_then(|usage| usage.weekly_all())
        .and_then(|week| week.resets_at)
        .ok_or_else(|| TextError::ResetMissing {
            account: id.to_string(),
        })
}

/// Why early switching is on or off (spec §2.5).
pub fn why(why: RegimeWhy) -> &'static str {
    match why {
        RegimeWhy::Learning => "learning",
        RegimeWhy::SessionsOff => "reading sessions is off: no rates are learned",
        RegimeWhy::NoCostYet => "no switch measured yet",
        RegimeWhy::NotWorthIt => "switching before 100 % does not pay",
        RegimeWhy::Pays => "switching before 100 % pays",
    }
}

/// One line of `seatswap burn plan`: `HH:MM–HH:MM  AC`, or
/// `HH:MM–HH:MM  nothing usable` for a stretch without an account. The times
/// are local; a time that is not today carries its weekday. An account without
/// a view is an error, never an empty name.
pub fn segment_line(
    account: Option<&str>,
    views: &[AccountView],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Result<String, TextError> {
    let name = match account {
        Some(id) => abbr(views, id)?,
        None => "nothing usable".to_string(),
    };
    Ok(format!(
        "{}–{}  {name}",
        format::clock(from, now, offset),
        format::clock(to, now, offset)
    ))
}

/// The name of a replay's policy: the words `seatswap burn simulate` prints.
pub fn policy_name(policy: Policy) -> &'static str {
    match policy {
        Policy::Lazy => "Lazy",
        Policy::Eager => "Eager",
    }
}

/// A work-unit figure as the hours it takes at `typical_wu` work units per
/// typical working hour. A typical hour of 0 means the hours are not known:
/// they are said so, never divided by zero.
fn hours(wu: f64, typical_wu: f64) -> String {
    match seatswap_core::burn::wu_hours(wu, typical_wu) {
        Some(hours) => format!("{hours:.1} h"),
        None => "not known".to_string(),
    }
}

/// The report of one replay: its policy, the hours worked and blocked, its
/// switches, and the expired week-percent of every account whose reset the
/// replay passed. A replay account without a stored account is an error.
pub fn replay_report(result: &ReplayResult, accounts: &[Account]) -> Result<String, TextError> {
    let mut lines = vec![format!(
        "{}: worked {}, blocked {}, {} switches",
        policy_name(result.policy),
        hours(result.worked_wu, result.typical_wu),
        hours(result.blocked_wu, result.typical_wu),
        result.switches
    )];
    for (account, expired) in &result.expired {
        let stored = accounts
            .iter()
            .find(|stored| AccountRef::of(stored) == *account)
            .ok_or_else(|| TextError::ViewMissing {
                account: format!("{}/{}", account.acc, account.org),
            })?;
        lines.push(format!(
            "  {}: {} % expired",
            stored.abbr,
            format::integer_percent(*expired)
        ));
    }
    Ok(lines.join("\n"))
}

/// The reason of a switch, in the words the notification and the suggestion use.
pub fn reason(
    reason: &Reason,
    target: &str,
    active: Option<&str>,
    views: &[AccountView],
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Result<String, TextError> {
    match reason {
        Reason::NoneActive => Ok("no account was in use".to_string()),
        Reason::ActiveFull { until } => {
            let id = active.ok_or(TextError::NoActive)?;
            let who = abbr(views, id)?;
            match until {
                Some(until) => Ok(format!(
                    "{who} is full until {}",
                    format::clock(*until, now, offset)
                )),
                None => Ok(format!("{who} is full")),
            }
        }
        Reason::WindowOpen => Ok(format!(
            "its 5 h window is open and its week resets on {}",
            format::weekday_clock(weekly_reset(views, target)?, offset)
        )),
        Reason::ResetNear => Ok(format!(
            "its week resets on {}",
            format::weekday_clock(weekly_reset(views, target)?, offset)
        )),
    }
}

#[cfg(feature = "gui")]
/// The line under the auto-burn switch: the account in use, when it fills and
/// which account follows. While learning, the accounts whose rates are missing.
pub fn line(
    plan: &Plan,
    active: Option<&str>,
    views: &[AccountView],
    learning: &[String],
    now: DateTime<Utc>,
) -> Result<Option<String>, TextError> {
    if !learning.is_empty() {
        let names = learning
            .iter()
            .map(|id| abbr(views, id))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        return Ok(Some(format!(
            "Learning how fast {names} are used. Until then: switch only when full."
        )));
    }
    let current = active
        .filter(|id| {
            plan.segments
                .iter()
                .any(|segment| segment.account.as_deref() == Some(*id))
        })
        .or_else(|| {
            plan.segments
                .iter()
                .find_map(|segment| segment.account.as_deref())
        });
    let Some(current) = current else {
        return Ok(None);
    };
    let next = plan
        .segments
        .iter()
        .find(|segment| segment.account.as_deref().is_some_and(|id| id != current));
    match next {
        Some(next) => {
            let Some(next_id) = next.account.as_deref() else {
                return Ok(None);
            };
            let seconds = next.from.signed_duration_since(now).num_seconds();
            let fill = plan
                .full_by
                .get(current)
                .ok_or_else(|| TextError::FillMissing {
                    account: current.to_string(),
                })?;
            let limit = match fill {
                Fill::Week => "week",
                Fill::Window => "5 h window",
            };
            Ok(Some(format!(
                "On {} until its {limit} is full, about {}. Then {}.",
                abbr(views, current)?,
                format::compact_left(seconds, seconds >= 24 * 60 * 60),
                abbr(views, next_id)?
            )))
        }
        None => Ok(Some(format!(
            "On {}. No switch planned this week.",
            abbr(views, current)?
        ))),
    }
}

#[cfg(feature = "gui")]
/// The first future use of every other account, by account id.
pub fn planned(plan: &Plan, active: Option<&str>) -> BTreeMap<String, DateTime<Utc>> {
    let mut planned = BTreeMap::new();
    for segment in &plan.segments {
        let Some(id) = segment.account.as_deref() else {
            continue;
        };
        if active == Some(id) {
            continue;
        }
        planned.entry(id.to_string()).or_insert(segment.from);
    }
    planned
}

#[cfg(feature = "gui")]
/// The notification after an automatic switch.
pub fn switch_notification(
    decision: &Decision,
    active: Option<&str>,
    views: &[AccountView],
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Result<Option<String>, TextError> {
    let Decision::Switch { to, reason: why } = decision else {
        return Ok(None);
    };
    let to_name = abbr(views, to)?;
    match (why, active) {
        (Reason::ActiveFull { until: Some(until) }, Some(from)) => Ok(Some(format!(
            "Auto-burn: now on {to_name}. {} is full until {}.",
            abbr(views, from)?,
            format::clock(*until, now, offset)
        ))),
        _ => Ok(Some(format!(
            "Auto-burn: now on {to_name}: {}.",
            reason(why, to, active, views, now, offset)?
        ))),
    }
}

/// The suggestion when auto-burn is off: "Switch to … now: …", plus what the
/// lazy plan expects to leave unused on the target's week.
pub fn suggestion(
    decision: &Decision,
    plan: &Plan,
    active: Option<&str>,
    views: &[AccountView],
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Result<Option<SwitchSuggestion>, TextError> {
    let (to, why) = match decision {
        Decision::Switch { to, reason } => (to, reason),
        Decision::Wait { to, reason, .. } => (to, reason),
        Decision::Stay | Decision::Stale | Decision::AllFull { .. } => return Ok(None),
    };
    let to_name = abbr(views, to)?;
    let mut text = format!(
        "Switch to {to_name} now: {}.",
        reason(why, to, active, views, now, offset)?
    );
    if let (Some(left), Some(active_name)) = (
        plan.left_at_reset.get(to),
        active.map(|id| abbr(views, id)).transpose()?,
    ) {
        if *left > 1.0 {
            text.push_str(&format!(
                " Staying on {active_name} is expected to let {} % of {to_name} expire.",
                format::integer_percent(*left)
            ));
        }
    }
    Ok(Some(SwitchSuggestion {
        to: to.clone(),
        text,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Duration;
    use std::collections::BTreeMap;

    use seatswap_core::api::{Limit, LimitKind, Severity, Usage};
    use seatswap_core::burn::decide::Decision;
    use seatswap_core::burn::plan::{Fill, Plan, Segment};
    use seatswap_core::store::LoginState;

    const RESET: &str = "2026-11-16T02:00:00Z";

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a test time")
            .with_timezone(&Utc)
    }

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(0).expect("UTC")
    }

    fn view(id: &str, abbr: &str, week: f64, session: bool) -> AccountView {
        let mut limits = vec![Limit {
            kind: LimitKind::WeeklyAll,
            percent: week,
            severity: Severity::Normal,
            resets_at: Some(utc(RESET)),
            model: None,
            is_active: false,
        }];
        if session {
            limits.push(Limit {
                kind: LimitKind::Session,
                percent: 10.0,
                severity: Severity::Normal,
                resets_at: Some(utc(RESET)),
                model: None,
                is_active: false,
            });
        }
        AccountView {
            id: id.to_string(),
            label: abbr.to_string(),
            abbr: abbr.to_string(),
            color: "#3b82f6".to_string(),
            email: None,
            plan: None,
            auto_burn: true,
            is_active: false,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            login_days_left: None,
            usage: Some(Usage {
                limits,
                extra_usage_enabled: false,
            }),
            usable: true,
            blocked_until: None,
            blocker: None,
            fetched_at: Some(utc("2026-11-10T11:59:00Z")),
            last_error: None,
        }
    }

    fn plan() -> Plan {
        let now = utc("2026-11-10T12:00:00Z");
        Plan {
            segments: vec![
                Segment {
                    account: Some("pr".to_string()),
                    from: now - Duration::hours(2),
                    to: now + Duration::hours(2),
                },
                Segment {
                    account: Some("ac".to_string()),
                    from: now + Duration::hours(2),
                    to: now + Duration::hours(4),
                },
            ],
            blocked_wu: 0.0,
            switches: 1,
            left_at_reset: BTreeMap::new(),
            full_at: BTreeMap::new(),
            full_by: BTreeMap::from([
                ("pr".to_string(), Fill::Week),
                ("ac".to_string(), Fill::Window),
            ]),
        }
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_line_names_the_account_in_use_and_the_next() {
        let views = vec![view("pr", "PR", 20.0, false), view("ac", "AC", 10.0, true)];
        let line = line(
            &plan(),
            Some("pr"),
            &views,
            &[],
            utc("2026-11-10T12:00:00Z"),
        )
        .expect("a line")
        .expect("the plan has a next account");
        assert_eq!(line, "On PR until its week is full, about 2h. Then AC.");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_line_says_week_when_the_week_fills_first() {
        let views = vec![view("pr", "PR", 20.0, true), view("ac", "AC", 10.0, true)];
        let line = line(
            &plan(),
            Some("pr"),
            &views,
            &[],
            utc("2026-11-10T12:00:00Z"),
        )
        .expect("a line")
        .expect("the plan has a next account");
        assert_eq!(
            line, "On PR until its week is full, about 2h. Then AC.",
            "PR has a 5 h limit, but the plan records its week as what fills first"
        );
    }

    #[test]
    fn a_suggestion_has_the_exact_reason() {
        let views = vec![view("pr", "PR", 20.0, false), view("ac", "AC", 10.0, true)];
        let decision = Decision::Switch {
            to: "ac".to_string(),
            reason: Reason::WindowOpen,
        };
        let suggestion = suggestion(
            &decision,
            &plan(),
            Some("pr"),
            &views,
            utc("2026-11-10T12:00:00Z"),
            offset(),
        )
        .expect("the text is writable")
        .expect("a switch suggests");
        assert_eq!(
            suggestion.text,
            "Switch to AC now: its 5 h window is open and its week resets on Mon 02:00."
        );
        assert_eq!(suggestion.to, "ac");
    }

    #[test]
    fn a_wait_suggestion_uses_the_decisions_reason() {
        let views = vec![view("pr", "PR", 20.0, false), view("ac", "AC", 10.0, false)];
        let decision = Decision::Wait {
            to: "ac".to_string(),
            latest: utc("2026-11-10T14:00:00Z"),
            tied_busy: 1,
            reason: Reason::WindowOpen,
        };
        let suggestion = suggestion(
            &decision,
            &plan(),
            Some("pr"),
            &views,
            utc("2026-11-10T12:00:00Z"),
            offset(),
        )
        .expect("the text is writable")
        .expect("a wait suggests");
        assert_eq!(
            suggestion.text,
            "Switch to AC now: its 5 h window is open and its week resets on Mon 02:00.",
            "AC has no session limit in its view; the decision's reason wins"
        );
    }

    #[test]
    fn sessions_off_says_why() {
        assert_eq!(
            why(RegimeWhy::SessionsOff),
            "reading sessions is off: no rates are learned"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn the_unread_line_names_the_accounts() {
        let views = vec![view("lb", "LB", 20.0, false), view("ac", "AC", 10.0, true)];
        let line = unread_line(&views, &["lb".to_string(), "ac".to_string()])
            .expect("both accounts have a view");
        assert_eq!(line, "Waiting for a first reading of LB, AC.");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn why_uses_the_plans_words() {
        assert_eq!(why(RegimeWhy::Learning), "learning");
        assert_eq!(why(RegimeWhy::NoCostYet), "no switch measured yet");
        assert_eq!(
            why(RegimeWhy::NotWorthIt),
            "switching before 100 % does not pay"
        );
        assert_eq!(why(RegimeWhy::Pays), "switching before 100 % pays");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_switch_notification_names_both_accounts() {
        let views = vec![view("pr", "PR", 100.0, false), view("ac", "AC", 10.0, true)];
        let decision = Decision::Switch {
            to: "ac".to_string(),
            reason: Reason::ActiveFull {
                until: Some(utc("2026-11-10T14:40:00Z")),
            },
        };
        let text = switch_notification(
            &decision,
            Some("pr"),
            &views,
            utc("2026-11-10T12:00:00Z"),
            offset(),
        )
        .expect("the text is writable")
        .expect("a switch notifies");
        assert_eq!(text, "Auto-burn: now on AC. PR is full until 14:40.");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn no_active_account_has_its_own_reason() {
        let views = vec![view("pr", "PR", 20.0, false), view("ac", "AC", 10.0, true)];
        let decision = Decision::Switch {
            to: "ac".to_string(),
            reason: Reason::NoneActive,
        };
        let text = switch_notification(
            &decision,
            None,
            &views,
            utc("2026-11-10T12:00:00Z"),
            offset(),
        )
        .expect("the text is writable")
        .expect("a switch notifies");
        assert_eq!(text, "Auto-burn: now on AC: no account was in use.");
    }

    #[test]
    fn an_account_without_a_view_is_an_error() {
        let views = vec![view("pr", "PR", 20.0, false)];
        let decision = Decision::Switch {
            to: "ac".to_string(),
            reason: Reason::WindowOpen,
        };
        let error = suggestion(
            &decision,
            &plan(),
            Some("pr"),
            &views,
            utc("2026-11-10T12:00:00Z"),
            offset(),
        )
        .expect_err("AC has no view");
        assert!(
            matches!(error, TextError::ViewMissing { ref account } if account == "ac"),
            "{error}"
        );
    }

    #[test]
    fn an_expiring_target_adds_the_lazy_plans_percent() {
        let views = vec![view("pr", "PR", 20.0, false), view("ac", "AC", 10.0, true)];
        let mut plan = plan();
        plan.left_at_reset.insert("ac".to_string(), 12.4);
        let decision = Decision::Switch {
            to: "ac".to_string(),
            reason: Reason::WindowOpen,
        };
        let suggestion = suggestion(
            &decision,
            &plan,
            Some("pr"),
            &views,
            utc("2026-11-10T12:00:00Z"),
            offset(),
        )
        .expect("the text is writable")
        .expect("a switch suggests");
        assert!(
            suggestion
                .text
                .ends_with(" Staying on PR is expected to let 12 % of AC expire."),
            "{}",
            suggestion.text
        );
    }
}
