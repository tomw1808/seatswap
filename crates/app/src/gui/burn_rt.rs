//! The auto-burn runtime of the tray app (spec §2.5, §2.12): when to learn,
//! when to decide, what to send, and the `BurnView` the pop-up shows. All the
//! words come from [`crate::burn_text`]; the reading and the inputs come from
//! `seatswap_core::burn::live`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::Serialize;

use seatswap_core::burn::decide::{decide, regime, Decision, Reason, Regime, Situation};
use seatswap_core::burn::live::{self, Learned, Readiness};
use seatswap_core::burn::plan::{follow, rates_known, Plan, Policy};
use seatswap_core::engine::{AccountView, PollReason};
use seatswap_core::history::{By, HistoryDir};
use seatswap_core::store::{Account, Settings};
use seatswap_core::switcher::UnsavedPolicy;

use super::worker::{ActionCommand, ActionReply, EngineCommand};
use crate::burn_text::{self, SwitchSuggestion};

/// A switch that is waiting for a quiet moment; the panel shows its latest
/// time (spec §2.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WaitingSwitch {
    pub to: String,
    pub latest: DateTime<Utc>,
    pub tied_busy: u32,
}

/// What the pop-up shows of auto-burn (spec §2.5).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BurnView {
    pub on: bool,
    pub paused_until: Option<DateTime<Utc>>,
    pub early: bool,
    pub why: String,
    pub learning: Vec<String>,
    pub line: Option<String>,
    /// The first future use of each account auto-burn may switch to.
    pub planned: BTreeMap<String, DateTime<Utc>>,
    pub waiting: Option<WaitingSwitch>,
    pub suggestion: Option<SwitchSuggestion>,
}

/// One snapshot's worth of inputs for [`BurnRuntime::after_snapshot`].
pub(crate) struct BurnTick<'a> {
    pub history: Option<&'a HistoryDir>,
    pub views: &'a [AccountView],
    pub accounts: &'a [Account],
    pub settings: &'a Settings,
    pub active: Option<&'a str>,
    pub now: DateTime<Utc>,
    pub offset: FixedOffset,
    pub tied_busy: u32,
}

/// What one tick produced. The worker adds `error` to the snapshot's error,
/// prints `warnings` once, and sends `notification` after the switch landed.
#[derive(Debug, Default)]
pub(crate) struct BurnReport {
    pub view: Option<BurnView>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub notification: Option<String>,
}

/// A switch that was sent and whose reply has not been read yet.
struct SentSwitch {
    to: String,
    reason: Reason,
    /// The active account when the switch was sent; a refusal leaves it.
    from: Option<String>,
    sent_at: DateTime<Utc>,
    /// The notification to show once the switch landed.
    notification: Option<String>,
    reply: Option<Receiver<ActionReply>>,
    /// The reply's error while the same decision still stands.
    error: Option<String>,
}

/// The longest age of a reading that still allows a decision (spec §2.5).
const STALE_ASK_SECS: i64 = 60;
/// A failed history read is tried again this many minutes later.
const LEARN_RETRY_MIN: i64 = 5;
/// The same switch decision is sent again after this many minutes.
const SWITCH_RETRY_MIN: i64 = 10;

pub(crate) struct BurnRuntime {
    learned: Option<Learned>,
    /// The last learn attempt and whether it failed; a failure is retried after
    /// [`LEARN_RETRY_MIN`] and its error stays in every snapshot until a learn
    /// succeeds.
    learn_attempted_at: Option<DateTime<Utc>>,
    learn_error: Option<String>,
    regime: Option<Regime>,
    regime_at: Option<DateTime<Utc>>,
    last_active: Option<String>,
    /// The last switch decision sent, its reply, and when it was sent.
    last_sent: Option<SentSwitch>,
    /// The time of the last stale poll ask (at most one per [`STALE_ASK_SECS`]).
    last_stale_ask: Option<DateTime<Utc>>,
    view: Option<BurnView>,
    warned: BTreeSet<String>,
    force_learn: bool,
    action: Sender<ActionCommand>,
    engine: Sender<EngineCommand>,
}

impl BurnRuntime {
    /// The runtime needs both senders from the start: the first snapshot the
    /// engine builds already decides and may switch (spec §2.5).
    pub(crate) fn new(action: Sender<ActionCommand>, engine: Sender<EngineCommand>) -> BurnRuntime {
        BurnRuntime {
            learned: None,
            learn_attempted_at: None,
            learn_error: None,
            regime: None,
            regime_at: None,
            last_active: None,
            last_sent: None,
            last_stale_ask: None,
            view: None,
            warned: BTreeSet::new(),
            force_learn: false,
            action,
            engine,
        }
    }

    /// Re-learn at the next snapshot, after a switch or a pause was written.
    pub(crate) fn force_learn(&mut self) {
        self.force_learn = true;
    }

    pub(crate) fn after_snapshot(&mut self, tick: &BurnTick<'_>) -> BurnReport {
        let report = self.tick(tick);
        self.last_active = tick.active.map(str::to_string);
        report
    }

    fn tick(&mut self, tick: &BurnTick<'_>) -> BurnReport {
        let mut report = BurnReport::default();
        self.read_switch_reply(tick.views, &mut report);

        let failed_due = self.learn_error.is_some()
            && self
                .learn_attempted_at
                .is_some_and(|at| tick.now - at >= Duration::minutes(LEARN_RETRY_MIN));
        let due = self.force_learn
            || self.learn_attempted_at.is_none()
            || (self.learn_error.is_none()
                && self
                    .learn_attempted_at
                    .is_some_and(|at| tick.now - at >= Duration::hours(1)))
            || failed_due
            || tick.active != self.last_active.as_deref();
        if due {
            self.force_learn = false;
            self.learn_attempted_at = Some(tick.now);
            match tick.history {
                Some(history) => match live::learn(history, tick.now, tick.offset, tick.accounts) {
                    Ok(learned) => {
                        for warning in &learned.warnings {
                            if self.warned.insert(warning.clone()) {
                                report.warnings.push(warning.clone());
                            }
                        }
                        self.learned = Some(learned);
                        self.learn_error = None;
                    }
                    Err(error) => {
                        self.learn_error =
                            Some(format!("auto-burn: cannot read the history: {error}"));
                    }
                },
                None => {
                    self.learn_error = Some("auto-burn: the history is not open".to_string());
                }
            }
        }
        if let Some(error) = &self.learn_error {
            report.error = Some(match report.error.take() {
                Some(previous) => format!("{previous}; {error}"),
                None => error.clone(),
            });
        }

        let Some(learned) = &self.learned else {
            return report;
        };
        let readiness = match live::situation(
            learned,
            tick.views,
            tick.accounts,
            tick.active,
            tick.now,
            tick.offset,
            tick.tied_busy,
        ) {
            Ok(readiness) => readiness,
            Err(error) => {
                report.error = Some(format!("auto-burn: {error}"));
                return report;
            }
        };
        let situation = match readiness {
            Readiness::Ready(situation) => situation,
            Readiness::Unread { accounts } => {
                // The same behaviour as `Decision::Stale`: poll, never switch.
                let _ = accounts;
                self.ask_stale(tick.now);
                return report;
            }
            Readiness::NoAccounts => {
                // Nothing to decide from: no view at all.
                self.view = None;
                return report;
            }
        };
        let regime_due = self.regime.is_none()
            || self
                .regime_at
                .is_none_or(|at| tick.now - at >= Duration::hours(1));
        if regime_due {
            self.regime = Some(regime(&situation));
            self.regime_at = Some(tick.now);
        }
        let Some(regime) = self.regime.clone() else {
            return report;
        };
        let decision = decide(&situation, &regime);
        let plan = follow(&situation.inputs, Policy::Lazy);
        let outcome = self.apply_decision(
            decision,
            &situation,
            &plan,
            &regime,
            tick.views,
            tick.settings,
            tick.now,
            tick.offset,
        );
        merge(report, outcome)
    }

    /// Read the switch's reply, if one arrived: the notification after it
    /// landed, or the error of a refused or failed switch.
    fn read_switch_reply(&mut self, views: &[AccountView], report: &mut BurnReport) {
        let Some(sent) = self.last_sent.as_mut() else {
            return;
        };
        let received = match sent.reply.as_ref() {
            Some(reply) => reply.try_recv(),
            None => return,
        };
        match received {
            Ok(ActionReply::Switched(_)) => {
                sent.reply = None;
                sent.error = None;
                report.notification = sent.notification.take();
            }
            Ok(ActionReply::NotSaved(_)) => {
                sent.reply = None;
                sent.error = Some(match sent.from.as_deref() {
                    Some(from) => match burn_text::abbr(views, from) {
                        Ok(from) => format!(
                            "auto-burn could not leave {from}: the current login is not saved"
                        ),
                        Err(error) => format!("auto-burn: {error}"),
                    },
                    None => "auto-burn: no active account to name".to_string(),
                });
            }
            Ok(ActionReply::Failed(message)) => {
                sent.reply = None;
                sent.error = Some(match burn_text::abbr(views, &sent.to) {
                    Ok(to) => format!("auto-burn: the switch to {to} failed: {message}"),
                    Err(error) => format!("auto-burn: {error}"),
                });
            }
            Ok(ActionReply::Done) => sent.reply = None,
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                sent.reply = None;
                sent.error = Some("auto-burn: the switch action is not connected".to_string());
            }
        }
    }

    /// Ask for a poll of the old readings, at most once per [`STALE_ASK_SECS`].
    fn ask_stale(&mut self, now: DateTime<Utc>) {
        if self
            .last_stale_ask
            .is_some_and(|at| now - at < Duration::seconds(STALE_ASK_SECS))
        {
            return;
        }
        if self
            .engine
            .send(EngineCommand::Poll(PollReason::Stale))
            .is_ok()
        {
            self.last_stale_ask = Some(now);
        }
    }

    #[cfg(test)]
    fn decision_report(
        &mut self,
        situation: &Situation,
        regime: &Regime,
        views: &[AccountView],
        settings: &Settings,
        now: DateTime<Utc>,
        offset: FixedOffset,
    ) -> BurnReport {
        let decision = decide(situation, regime);
        let plan = follow(&situation.inputs, Policy::Lazy);
        self.apply_decision(
            decision, situation, &plan, regime, views, settings, now, offset,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_decision(
        &mut self,
        decision: Decision,
        situation: &Situation,
        plan: &Plan,
        regime: &Regime,
        views: &[AccountView],
        settings: &Settings,
        now: DateTime<Utc>,
        offset: FixedOffset,
    ) -> BurnReport {
        let active = situation.inputs.active.as_deref();
        let learning: Vec<String> = situation
            .inputs
            .accounts
            .iter()
            .filter(|account| account.may_use && !rates_known(account))
            .map(|account| account.id.clone())
            .collect();
        let mut report = BurnReport::default();
        let line = match burn_text::line(plan, active, views, &learning, now) {
            Ok(line) => line,
            Err(error) => {
                report.error = Some(text_error(error));
                None
            }
        };
        let mut view = BurnView {
            on: settings.auto_burn,
            paused_until: situation.manual_hold_until.filter(|until| *until > now),
            early: regime.early,
            why: burn_text::why(regime.why).to_string(),
            learning: learning.clone(),
            line,
            planned: burn_text::planned(plan, active),
            waiting: None,
            suggestion: None,
        };

        match &decision {
            Decision::Switch { to, reason } => {
                if settings.auto_burn {
                    let retry_due = match &self.last_sent {
                        Some(sent) if sent.to == *to && sent.reason == *reason => {
                            now - sent.sent_at >= Duration::minutes(SWITCH_RETRY_MIN)
                        }
                        _ => true,
                    };
                    if retry_due {
                        match (
                            burn_text::reason(reason, to, active, views, now, offset),
                            burn_text::switch_notification(&decision, active, views, now, offset),
                        ) {
                            (Ok(reason_text), Ok(notification)) => {
                                let (reply, reply_rx) = mpsc::channel();
                                let sent = self
                                    .action
                                    .send(ActionCommand::Switch {
                                        key: to.clone(),
                                        unsaved: UnsavedPolicy::Refuse,
                                        label: None,
                                        by: By::Auto,
                                        reason: Some(reason_text),
                                        reply,
                                    })
                                    .is_ok();
                                if sent {
                                    self.last_sent = Some(SentSwitch {
                                        to: to.clone(),
                                        reason: reason.clone(),
                                        from: active.map(str::to_string),
                                        sent_at: now,
                                        notification,
                                        reply: Some(reply_rx),
                                        error: None,
                                    });
                                    self.force_learn = true;
                                } else {
                                    report.error = Some(
                                        "auto-burn: the switch action is not connected".to_string(),
                                    );
                                }
                            }
                            (Err(error), _) | (_, Err(error)) => {
                                report.error = Some(text_error(error));
                            }
                        }
                    } else if let Some(error) =
                        self.last_sent.as_ref().and_then(|sent| sent.error.as_ref())
                    {
                        report.error = Some(error.clone());
                    }
                } else if settings.suggest_switches {
                    match burn_text::suggestion(&decision, plan, active, views, now, offset) {
                        Ok(suggestion) => view.suggestion = suggestion,
                        Err(error) => report.error = Some(text_error(error)),
                    }
                }
            }
            Decision::Wait {
                to,
                latest,
                tied_busy,
                ..
            } => {
                if settings.auto_burn {
                    view.waiting = Some(WaitingSwitch {
                        to: to.clone(),
                        latest: *latest,
                        tied_busy: *tied_busy,
                    });
                } else if settings.suggest_switches {
                    match burn_text::suggestion(&decision, plan, active, views, now, offset) {
                        Ok(suggestion) => view.suggestion = suggestion,
                        Err(error) => report.error = Some(text_error(error)),
                    }
                }
            }
            Decision::Stale => self.ask_stale(now),
            Decision::Stay | Decision::AllFull { .. } => {}
        }

        if !settings.auto_burn || !matches!(decision, Decision::Switch { .. }) {
            self.last_sent = None;
        }
        self.view = Some(view.clone());
        report.view = Some(view);
        report
    }
}

fn text_error(error: burn_text::TextError) -> String {
    format!("auto-burn: {error}")
}

fn merge(mut report: BurnReport, next: BurnReport) -> BurnReport {
    if next.view.is_some() {
        report.view = next.view;
    }
    if let Some(error) = next.error {
        report.error = Some(match report.error {
            Some(previous) => format!("{previous}; {error}"),
            None => error,
        });
    }
    report.warnings.extend(next.warnings);
    if next.notification.is_some() {
        report.notification = next.notification;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::FixedOffset;

    use seatswap_core::api::{Limit, LimitKind, Severity, Usage};
    use seatswap_core::burn::decide::{RegimeWhy, Situation, SwitchCost};
    use seatswap_core::burn::plan::{AccountState, Inputs, Plan, Segment, SessionState};
    use seatswap_core::burn::{Rates, Rhythm};
    use seatswap_core::history::{AccountRef, Event, EventKind, Hosted};
    use seatswap_core::store::LoginState;
    use seatswap_core::types::OAuthAccount;

    use super::super::worker::PendingSwitch;

    const ORG: &str = "e92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20";
    const PR_ACC: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const AC_ACC: &str = "8e1d5b7a-6c3f-4a2b-9d8e-1f2a3b4c5d6e";
    const RESET: &str = "2026-11-16T02:00:00Z";

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a test time")
            .with_timezone(&Utc)
    }

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(0).expect("UTC")
    }

    fn account(id: &str, abbr: &str, acc: &str, auto_burn: bool) -> Account {
        Account {
            id: id.to_string(),
            account_uuid: acc.to_string(),
            organization_uuid: ORG.to_string(),
            label: abbr.to_string(),
            abbr: abbr.to_string(),
            color: "#3b82f6".to_string(),
            email: None,
            org_name: None,
            plan: Some("Team".to_string()),
            oauth_account: OAuthAccount(serde_json::Map::new()),
            added_at: utc("2026-11-01T00:00:00Z"),
            last_used_at: None,
            login_state: LoginState::Ok,
            login_ends_at_ms: None,
            auto_burn,
        }
    }

    fn limit(kind: LimitKind, percent: f64) -> Limit {
        Limit {
            kind,
            percent,
            severity: Severity::Normal,
            resets_at: Some(utc(RESET)),
            model: None,
            is_active: false,
        }
    }

    fn usage(week: f64, session: bool) -> Usage {
        let mut limits = vec![limit(LimitKind::WeeklyAll, week)];
        if session {
            limits.push(limit(LimitKind::Session, 10.0));
        }
        Usage {
            limits,
            extra_usage_enabled: false,
        }
    }

    fn view(
        id: &str,
        abbr: &str,
        week: f64,
        session: bool,
        fetched_at: DateTime<Utc>,
    ) -> AccountView {
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
            usage: Some(usage(week, session)),
            usable: true,
            blocked_until: None,
            blocker: None,
            fetched_at: Some(fetched_at),
            last_error: None,
        }
    }

    fn state(id: &str, may_use: bool, week: f64, session: bool) -> AccountState {
        AccountState {
            id: id.to_string(),
            may_use,
            login_ok: true,
            week_percent: week,
            week_resets_at: utc(RESET),
            session: if session {
                Some(SessionState {
                    percent: 10.0,
                    resets_at: Some(utc(RESET)),
                })
            } else {
                None
            },
            model_blocked: false,
            rates: if session {
                Some(Rates {
                    c: 4.0,
                    s: Some(100.0),
                })
            } else {
                Some(Rates { c: 2.0, s: None })
            },
        }
    }

    fn situation(now: DateTime<Utc>, active: Option<&str>) -> Situation {
        Situation {
            inputs: Inputs {
                now,
                offset: offset(),
                accounts: vec![
                    state("pr", true, 20.0, false),
                    state("ac", true, 80.0, true),
                ],
                active: active.map(str::to_string),
                rhythm: Rhythm { wu: [1.0; 168] },
            },
            cost: Some(SwitchCost {
                wu: 0.05,
                samples: 3,
            }),
            last_switch_at: None,
            manual_hold_until: None,
            tied_busy: 0,
            oldest_reading: now - Duration::minutes(1),
        }
    }

    fn regime(early: bool) -> Regime {
        Regime {
            early,
            why: if early {
                RegimeWhy::Pays
            } else {
                RegimeWhy::NoCostYet
            },
            lazy_blocked_h: 1.0,
            eager_blocked_h: 0.0,
            extra_switches: 1,
            cost_h: Some(0.1),
        }
    }

    fn plan(now: DateTime<Utc>) -> Plan {
        Plan {
            segments: vec![
                Segment {
                    account: Some("pr".to_string()),
                    from: now,
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
                ("pr".to_string(), seatswap_core::burn::plan::Fill::Week),
                ("ac".to_string(), seatswap_core::burn::plan::Fill::Window),
            ]),
        }
    }

    fn window_open() -> Decision {
        Decision::Switch {
            to: "ac".to_string(),
            reason: Reason::WindowOpen,
        }
    }

    fn runtime() -> (
        BurnRuntime,
        mpsc::Receiver<ActionCommand>,
        mpsc::Receiver<EngineCommand>,
    ) {
        let (action_tx, action_rx) = mpsc::channel();
        let (engine_tx, engine_rx) = mpsc::channel();
        (BurnRuntime::new(action_tx, engine_tx), action_rx, engine_rx)
    }

    /// The engine builds its first snapshot before it starts `run`, so the
    /// runtime must own both senders from `new`: that first tick already
    /// decides and may switch (the tray bench showed "auto-burn: the switch
    /// action is not connected" before).
    #[test]
    fn the_first_snapshot_can_switch() {
        let now = utc("2026-11-10T12:00:00Z");
        let tmp = tempfile::tempdir().expect("a temp folder");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let (action_tx, action_rx) = mpsc::channel();
        let (engine_tx, _engine_rx) = mpsc::channel();
        let mut runtime = BurnRuntime::new(action_tx, engine_tx);
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
        ];
        let views = vec![
            view("pr", "PR", 100.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, true, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let tick = BurnTick {
            history: Some(&history),
            views: &views,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now,
            offset: offset(),
            tied_busy: 0,
        };
        let report = runtime.after_snapshot(&tick);
        assert!(report.view.is_some(), "the first snapshot has a view");
        match action_rx.try_recv().expect("the first tick sends a switch") {
            ActionCommand::Switch { key, by, .. } => {
                assert_eq!(key, "ac");
                assert_eq!(by, By::Auto);
            }
            _ => panic!("not a switch"),
        }
        assert!(action_rx.try_recv().is_err(), "only one command");
    }

    #[test]
    fn auto_burn_off_never_sends_a_switch() {
        let now = utc("2026-11-10T12:00:00Z");
        let (mut runtime, action, _engine) = runtime();
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(1)),
        ];
        let settings = Settings::default();
        let report = runtime.apply_decision(
            window_open(),
            &situation(now, Some("pr")),
            &plan(now),
            &regime(true),
            &views,
            &settings,
            now,
            offset(),
        );
        assert!(action.try_recv().is_err(), "auto-burn is off");
        let suggestion = report
            .view
            .expect("a view")
            .suggestion
            .expect("a suggestion");
        assert_eq!(
            suggestion.text,
            "Switch to AC now: its 5 h window is open and its week resets on Mon 02:00."
        );
    }

    #[test]
    fn suggestions_off_shows_nothing() {
        let now = utc("2026-11-10T12:00:00Z");
        let (mut runtime, action, _engine) = runtime();
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            suggest_switches: false,
            ..Settings::default()
        };
        let report = runtime.apply_decision(
            window_open(),
            &situation(now, Some("pr")),
            &plan(now),
            &regime(true),
            &views,
            &settings,
            now,
            offset(),
        );
        assert!(action.try_recv().is_err());
        assert!(report.view.expect("a view").suggestion.is_none());
    }

    #[test]
    fn auto_burn_on_sends_one_switch_by_auto() {
        let now = utc("2026-11-10T12:00:00Z");
        let (mut runtime, action, _engine) = runtime();
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        runtime.apply_decision(
            window_open(),
            &situation(now, Some("pr")),
            &plan(now),
            &regime(true),
            &views,
            &settings,
            now,
            offset(),
        );
        match action.try_recv().expect("one switch") {
            ActionCommand::Switch {
                key, by, reason, ..
            } => {
                assert_eq!(key, "ac");
                assert_eq!(by, By::Auto);
                assert_eq!(
                    reason.as_deref(),
                    Some("its 5 h window is open and its week resets on Mon 02:00")
                );
            }
            _ => panic!("not a switch"),
        }
        assert!(action.try_recv().is_err(), "only one command");
    }

    #[test]
    fn one_decision_sends_one_switch() {
        let now = utc("2026-11-10T12:00:00Z");
        let (mut runtime, action, _engine) = runtime();
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let situation = situation(now, Some("pr"));
        let plan = plan(now);
        let regime = regime(true);
        runtime.apply_decision(
            window_open(),
            &situation,
            &plan,
            &regime,
            &views,
            &settings,
            now,
            offset(),
        );
        runtime.apply_decision(
            window_open(),
            &situation,
            &plan,
            &regime,
            &views,
            &settings,
            now,
            offset(),
        );
        assert!(action.try_recv().is_ok(), "the first decision switches");
        assert!(
            action.try_recv().is_err(),
            "the same decision before the switch landed sends nothing"
        );
    }

    #[test]
    fn a_logged_out_account_cannot_hold_back_leaving_a_full_account() {
        let now = utc("2026-11-10T12:00:00Z");
        let tmp = tempfile::tempdir().expect("a temp folder");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let (mut runtime, action, _engine) = runtime();
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
            account("lb", "LB", "2b7e9c4d-1a3f-4c5e-8b6d-7f0a1c2e3d4b", true),
        ];
        let mut logged_out = view("ac", "AC", 10.0, false, now - Duration::hours(3));
        logged_out.login_state = LoginState::LoggedOut;
        let views = vec![
            view("pr", "PR", 100.0, false, now - Duration::minutes(1)),
            logged_out,
            view("lb", "LB", 10.0, false, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let tick = BurnTick {
            history: Some(&history),
            views: &views,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now,
            offset: offset(),
            tied_busy: 0,
        };
        runtime.after_snapshot(&tick);
        match action.try_recv().expect("one switch") {
            ActionCommand::Switch { key, by, .. } => {
                assert_eq!(key, "lb");
                assert_eq!(by, By::Auto);
            }
            _ => panic!("not a switch"),
        }
        assert!(action.try_recv().is_err(), "only one command");
    }

    #[test]
    fn a_stale_situation_asks_for_one_poll_a_minute() {
        let now = utc("2026-11-10T12:00:00Z");
        let tmp = tempfile::tempdir().expect("a temp folder");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let (mut runtime, _action, engine) = runtime();
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
        ];
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(25)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(25)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let tick = |at: DateTime<Utc>| BurnTick {
            history: Some(&history),
            views: &views,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now: at,
            offset: offset(),
            tied_busy: 0,
        };
        runtime.after_snapshot(&tick(now));
        assert!(matches!(
            engine.try_recv().expect("the first poll"),
            EngineCommand::Poll(PollReason::Stale)
        ));
        runtime.after_snapshot(&tick(now + Duration::seconds(10)));
        assert!(
            engine.try_recv().is_err(),
            "ten seconds later is not a new ask"
        );
        runtime.after_snapshot(&tick(now + Duration::seconds(71)));
        assert!(matches!(
            engine.try_recv().expect("the second poll"),
            EngineCommand::Poll(PollReason::Stale)
        ));
    }

    #[test]
    fn a_first_tick_without_readings_does_not_stop_decisions() {
        let now = utc("2026-11-10T12:00:00Z");
        let tmp = tempfile::tempdir().expect("a temp folder");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let (mut runtime, action, engine) = runtime();
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let mut no_reading = view("ac", "AC", 10.0, false, now - Duration::minutes(1));
        no_reading.usage = None;
        no_reading.fetched_at = None;
        let first = vec![
            view("pr", "PR", 100.0, false, now - Duration::minutes(1)),
            no_reading,
        ];
        let first_tick = BurnTick {
            history: Some(&history),
            views: &first,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now,
            offset: offset(),
            tied_busy: 0,
        };
        let first_report = runtime.after_snapshot(&first_tick);
        assert!(first_report.view.is_none(), "unread shows no view");
        assert!(matches!(
            engine.try_recv().expect("a poll"),
            EngineCommand::Poll(PollReason::Stale)
        ));

        let second = vec![
            view("pr", "PR", 100.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, false, now),
        ];
        let second_tick = BurnTick {
            history: Some(&history),
            views: &second,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now: now + Duration::minutes(1),
            offset: offset(),
            tied_busy: 0,
        };
        let second_report = runtime.after_snapshot(&second_tick);
        assert!(second_report.view.is_some(), "the regime is computed now");
        match action.try_recv().expect("one switch") {
            ActionCommand::Switch { key, by, .. } => {
                assert_eq!(key, "ac");
                assert_eq!(by, By::Auto);
            }
            _ => panic!("not a switch"),
        }
    }

    #[test]
    fn a_learn_error_stays_until_a_learn_succeeds() {
        let now = utc("2026-11-10T12:00:00Z");
        let tmp = tempfile::tempdir().expect("a temp folder");
        let root = tmp.path().join("history");
        std::fs::write(&root, b"not a history folder").expect("a file where the root belongs");
        let history = HistoryDir::at(&root, "atlas");
        let (mut runtime, _action, _engine) = runtime();
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
        ];
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let tick = |at: DateTime<Utc>| BurnTick {
            history: Some(&history),
            views: &views,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now: at,
            offset: offset(),
            tied_busy: 0,
        };
        let first = runtime.after_snapshot(&tick(now));
        let error = first.error.expect("the learn error");
        assert!(error.starts_with("auto-burn: cannot read the history:"));

        let second = runtime.after_snapshot(&tick(now + Duration::minutes(1)));
        assert_eq!(
            second.error.as_deref(),
            Some(error.as_str()),
            "no retry before five minutes, and the error stays"
        );

        std::fs::remove_file(&root).expect("the file is ours");
        std::fs::create_dir(&root).expect("a history folder");
        let third = runtime.after_snapshot(&tick(now + Duration::minutes(6)));
        assert_eq!(third.error, None, "the successful learn clears the error");
        assert!(third.view.is_some());
    }

    #[test]
    fn a_switch_that_did_not_land_is_tried_again_after_ten_minutes() {
        let now = utc("2026-11-10T12:00:00Z");
        let tmp = tempfile::tempdir().expect("a temp folder");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let (mut runtime, action, _engine) = runtime();
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
        ];
        let views = vec![
            view("pr", "PR", 100.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, false, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let tick = |at: DateTime<Utc>| BurnTick {
            history: Some(&history),
            views: &views,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now: at,
            offset: offset(),
            tied_busy: 0,
        };
        runtime.after_snapshot(&tick(now));
        let reply = match action.try_recv().expect("the first switch") {
            ActionCommand::Switch { reply, .. } => reply,
            _ => panic!("not a switch"),
        };
        reply
            .send(ActionReply::Failed("the login file is busy".to_string()))
            .expect("the runtime listens");

        let failed = runtime.after_snapshot(&tick(now + Duration::minutes(1)));
        assert_eq!(
            failed.error.as_deref(),
            Some("auto-burn: the switch to AC failed: the login file is busy")
        );
        assert!(
            action.try_recv().is_err(),
            "the same decision is not sent again before ten minutes"
        );

        runtime.after_snapshot(&tick(now + Duration::minutes(11)));
        assert!(
            action.try_recv().is_ok(),
            "the same decision is sent again after ten minutes"
        );
    }

    #[test]
    fn a_refused_switch_shows_why() {
        let now = utc("2026-11-10T12:00:00Z");
        let tmp = tempfile::tempdir().expect("a temp folder");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let (mut runtime, action, _engine) = runtime();
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
        ];
        let views = vec![
            view("pr", "PR", 100.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, false, now - Duration::minutes(1)),
        ];
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let tick = |at: DateTime<Utc>| BurnTick {
            history: Some(&history),
            views: &views,
            accounts: &accounts,
            settings: &settings,
            active: Some("pr"),
            now: at,
            offset: offset(),
            tied_busy: 0,
        };
        runtime.after_snapshot(&tick(now));
        let reply = match action.try_recv().expect("the switch") {
            ActionCommand::Switch { reply, .. } => reply,
            _ => panic!("not a switch"),
        };
        reply
            .send(ActionReply::NotSaved(PendingSwitch {
                target_id: "ac".to_string(),
                target_label: "AC".to_string(),
                email: None,
                org_name: None,
            }))
            .expect("the runtime listens");

        let report = runtime.after_snapshot(&tick(now + Duration::minutes(1)));
        assert_eq!(
            report.error.as_deref(),
            Some("auto-burn could not leave PR: the current login is not saved")
        );
    }

    #[test]
    fn a_user_switch_starts_the_hold() {
        let now = utc("2026-11-10T12:00:00Z");
        let event = Hosted {
            host: "atlas".to_string(),
            record: Event {
                t: now - Duration::hours(1),
                kind: EventKind::Switch {
                    from: None,
                    to: AccountRef {
                        acc: AC_ACC.to_string(),
                        org: ORG.to_string(),
                    },
                    by: By::User,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: None,
                },
            },
        };
        let accounts = vec![
            account("pr", "PR", PR_ACC, true),
            account("ac", "AC", AC_ACC, true),
        ];
        let mut rates = BTreeMap::new();
        // PR fills its week fast and its 5 h windows are small; AC can work far
        // longer, so without a hold the early rule would switch to PR.
        rates.insert(
            AccountRef::of(&accounts[0]),
            Rates {
                c: 1.0,
                s: Some(100.0),
            },
        );
        rates.insert(AccountRef::of(&accounts[1]), Rates { c: 1.0, s: None });
        let learned = Learned {
            rhythm: Rhythm { wu: [1.0; 168] },
            rates,
            cost: None,
            warnings: Vec::new(),
            events: vec![event],
            host: "atlas".to_string(),
        };
        let views = vec![
            view("pr", "PR", 10.0, true, now - Duration::minutes(1)),
            view("ac", "AC", 10.0, false, now - Duration::minutes(1)),
        ];
        let readiness = live::situation(&learned, &views, &accounts, Some("ac"), now, offset(), 0)
            .expect("the views are valid");
        let Readiness::Ready(situation) = readiness else {
            panic!("every account has a reading");
        };
        let (mut held, action, _engine) = runtime();
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        let report =
            held.decision_report(&situation, &regime(true), &views, &settings, now, offset());
        assert!(action.try_recv().is_err(), "a hold stays");
        assert_eq!(
            report.view.expect("a view").paused_until,
            Some(now + Duration::hours(3))
        );

        // The same reading without the hold would switch to PR: the input
        // above would therefore fail if the hold rule were missing.
        let mut without_hold = situation;
        without_hold.manual_hold_until = None;
        let (mut fresh, action, _engine) = runtime();
        fresh.decision_report(
            &without_hold,
            &regime(true),
            &views,
            &settings,
            now,
            offset(),
        );
        match action.try_recv().expect("a switch to PR") {
            ActionCommand::Switch { key, .. } => assert_eq!(key, "pr"),
            _ => panic!("not a switch"),
        }
    }

    #[test]
    fn stale_readings_ask_for_a_poll_and_do_not_switch() {
        let now = utc("2026-11-10T12:00:00Z");
        let (mut runtime, action, engine) = runtime();
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(25)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(25)),
        ];
        let mut situation = situation(now, Some("pr"));
        situation.oldest_reading = now - Duration::minutes(25);
        let settings = Settings {
            auto_burn: true,
            ..Settings::default()
        };
        runtime.decision_report(&situation, &regime(true), &views, &settings, now, offset());
        assert!(action.try_recv().is_err(), "stale readings do not switch");
        assert!(matches!(
            engine.try_recv().expect("a poll"),
            EngineCommand::Poll(PollReason::Stale)
        ));
    }

    #[test]
    fn the_line_names_the_account_in_use_and_the_next() {
        let now = utc("2026-11-10T12:00:00Z");
        let views = vec![
            view("pr", "PR", 20.0, false, now - Duration::minutes(1)),
            view("ac", "AC", 80.0, true, now - Duration::minutes(1)),
        ];
        let line = burn_text::line(&plan(now), Some("pr"), &views, &[], now)
            .expect("a line")
            .expect("a next account");
        assert_eq!(line, "On PR until its week is full, about 2h. Then AC.");
    }

    #[test]
    fn planned_holds_the_first_use_of_each_other_account() {
        let now = utc("2026-11-10T12:00:00Z");
        let plan = Plan {
            segments: vec![
                Segment {
                    account: Some("pr".to_string()),
                    from: now,
                    to: now + Duration::hours(1),
                },
                Segment {
                    account: Some("ac".to_string()),
                    from: now + Duration::hours(1),
                    to: now + Duration::hours(2),
                },
                Segment {
                    account: Some("lb".to_string()),
                    from: now + Duration::hours(2),
                    to: now + Duration::hours(3),
                },
                Segment {
                    account: Some("pr".to_string()),
                    from: now + Duration::hours(3),
                    to: now + Duration::hours(4),
                },
            ],
            blocked_wu: 0.0,
            switches: 2,
            left_at_reset: BTreeMap::new(),
            full_at: BTreeMap::new(),
            full_by: BTreeMap::new(),
        };
        let planned = burn_text::planned(&plan, Some("pr"));
        assert_eq!(planned.get("ac"), Some(&(now + Duration::hours(1))));
        assert_eq!(planned.get("lb"), Some(&(now + Duration::hours(2))));
        assert!(!planned.contains_key("pr"));
    }
}
