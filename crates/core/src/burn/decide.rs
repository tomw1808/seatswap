//! The one decision of auto-burn (spec §2.5): whether to stay, switch, wait for
//! a quiet moment, or say that every account is full. It also decides once an
//! hour whether switching before an account is full pays for itself.

use chrono::{DateTime, Duration, Utc};

use super::plan::{
    blocked, choose, follow, rates_known, rhythm_walk, slack, usable_again, Inputs, PlanError,
    Policy,
};
use super::{
    wu_hours, FRESH_MIN, GAIN_FACTOR, HORIZON_H, MIN_COST_SAMPLES, MIN_DWELL_MIN, MIN_GAIN_H,
    SLACK_MARGIN_H, STEP_MIN,
};

/// The measured cost of one switch (spec §2.13): the extra work units and the
/// number of switches the mean came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwitchCost {
    pub wu: f64,
    pub samples: u32,
}

/// Everything one decision is taken from. [`Situation::check`] rejects a
/// situation no decision may be taken from; [`decide`] and [`regime`] require
/// that it has passed.
#[derive(Clone)]
pub struct Situation {
    pub inputs: Inputs,
    pub cost: Option<SwitchCost>,
    pub last_switch_at: Option<DateTime<Utc>>,
    pub manual_hold_until: Option<DateTime<Utc>>,
    pub tied_busy: u32,
    pub oldest_reading: DateTime<Utc>,
    /// `readSessions` is off, so no rates were learned (spec §2.3).
    pub sessions_off: bool,
}

impl Situation {
    /// Checks the values [`decide`] and [`regime`] take: [`Inputs::check`] must
    /// pass, and neither the last switch nor the oldest reading may lie after
    /// `inputs.now` — that is a clock that went back, not a value to hold or
    /// pass.
    pub fn check(&self) -> Result<(), PlanError> {
        self.inputs.check()?;
        let now = self.inputs.now;
        if let Some(at) = self.last_switch_at {
            if at > now {
                return Err(PlanError::LastSwitchAfterNow { at, now });
            }
        }
        if self.oldest_reading > now {
            return Err(PlanError::OldestReadingAfterNow {
                at: self.oldest_reading,
                now,
            });
        }
        Ok(())
    }
}

/// Why early switching is on or off (spec §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegimeWhy {
    /// Not every account auto-burn may use has rates, or no typical hour is known.
    Learning,
    /// No rates were learned because `readSessions` is off (spec §2.3).
    SessionsOff,
    /// No switch has been measured, or too few of them.
    NoCostYet,
    /// The eager plan does not save enough for its extra switches.
    NotWorthIt,
    /// The eager plan saves enough.
    Pays,
}

/// The regime of the next hour: whether the eager plan pays, and the numbers it
/// was decided from (spec §2.5).
#[derive(Debug, Clone, PartialEq)]
pub struct Regime {
    pub early: bool,
    pub why: RegimeWhy,
    pub lazy_blocked_h: f64,
    pub eager_blocked_h: f64,
    pub extra_switches: u32,
    pub cost_h: Option<f64>,
}

/// Why the decision switches (spec §2.5, §2.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// No account was in use at the reading.
    NoneActive,
    /// The active account is blocked (or the login ended or a model blocks it).
    ActiveFull { until: Option<DateTime<Utc>> },
    /// The target's 5 h window has room.
    WindowOpen,
    /// The target is about to lose unused quota at its weekly reset.
    ResetNear,
}

/// What auto-burn does at this reading (spec §2.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Stay,
    Stale,
    Switch {
        to: String,
        reason: Reason,
    },
    Wait {
        to: String,
        latest: DateTime<Utc>,
        tied_busy: u32,
        reason: Reason,
    },
    AllFull {
        next: Option<(String, DateTime<Utc>)>,
    },
}

/// Which of the two plans pays for the next hour (spec §2.5). `Pays` only when
/// both rules hold: the eager plan blocks at least [`MIN_GAIN_H`] typical hours
/// less than the lazy one, and that gain is at least [`GAIN_FACTOR`] times what
/// its extra switches cost. `Learning` while an account auto-burn may use has no
/// rates, or while the rhythm's typical hour is 0.0; `NoCostYet` until the switch
/// cost has [`MIN_COST_SAMPLES`] samples. `early` is `why == Pays`.
///
/// The plans are [`follow`] with [`Policy::Lazy`] and [`Policy::Eager`];
/// `extra_switches` is eager's switches minus lazy's when eager has more, else 0
/// (fewer switches cost nothing extra). `cost_h` is the switch cost divided by
/// the typical hour. When that hour is 0.0 no hours can be computed: every `_h`
/// field is 0.0 and `cost_h` is `None` (it is also `None` while no cost is
/// known).
///
/// `situation` must have passed [`Situation::check`]; this function does not
/// check it again.
pub fn regime(situation: &Situation) -> Regime {
    let inputs = &situation.inputs;
    let typical = inputs.rhythm.typical_hour();
    let lazy = follow(inputs, Policy::Lazy);
    let eager = follow(inputs, Policy::Eager);
    let lazy_blocked_h = wu_hours(lazy.blocked_wu, typical).unwrap_or(0.0);
    let eager_blocked_h = wu_hours(eager.blocked_wu, typical).unwrap_or(0.0);
    let extra_switches = eager.switches.saturating_sub(lazy.switches);
    let cost_h = situation.cost.and_then(|cost| wu_hours(cost.wu, typical));
    let learning = typical == 0.0
        || inputs
            .accounts
            .iter()
            .any(|account| account.may_use && !rates_known(account));
    let why = if learning && situation.sessions_off {
        RegimeWhy::SessionsOff
    } else if learning {
        RegimeWhy::Learning
    } else {
        match (situation.cost, cost_h) {
            (Some(cost), Some(cost_h)) if cost.samples >= MIN_COST_SAMPLES => {
                let gain = lazy_blocked_h - eager_blocked_h;
                let extra_cost = GAIN_FACTOR * extra_switches as f64 * cost_h;
                if gain >= MIN_GAIN_H && gain >= extra_cost {
                    RegimeWhy::Pays
                } else {
                    RegimeWhy::NotWorthIt
                }
            }
            _ => RegimeWhy::NoCostYet,
        }
    };
    Regime {
        early: why == RegimeWhy::Pays,
        why,
        lazy_blocked_h,
        eager_blocked_h,
        extra_switches,
        cost_h,
    }
}

/// The one decision of auto-burn (spec §2.5, §2.6), in this order:
/// 1. a reading older than [`FRESH_MIN`] minutes → [`Decision::Stale`];
/// 2. and 3. the active account is [`blocked`] or there is none: [`choose`] names
///    a target → [`Decision::Switch`] with [`Reason::ActiveFull`] (or
///    [`Reason::NoneActive`] when there is no active account), else
///    [`Decision::AllFull`];
/// 4. a manual hold in the future → [`Decision::Stay`];
/// 5. early switching ([`Regime::early`]) and [`choose`] names another account
///    whose slack is at least [`SLACK_MARGIN_H`] × the typical hour below the
///    active one's, at least [`MIN_DWELL_MIN`] after the last switch: with no
///    busy tied session → [`Decision::Switch`] (`WindowOpen` when the target has
///    a 5 h limit, else `ResetNear`); with one → [`Decision::Wait`] until the
///    rhythm's work from now reaches the target's slack. A slack of 0 or less
///    switches at once;
/// 6. otherwise [`Decision::Stay`].
///
/// Step 3 tests the active account for `blocked`, not for `usable`: an account
/// auto-burn may not use is left only when it is blocked (spec §2.6), and it
/// never gives `AllFull` while it is not blocked. Step 5 needs the active
/// account's slack and an active account auto-burn may use; when either is
/// missing there is no early switch.
///
/// `situation` must have passed [`Situation::check`]; this function does not
/// check it again.
pub fn decide(situation: &Situation, regime: &Regime) -> Decision {
    let inputs = &situation.inputs;
    let now = inputs.now;
    if now.signed_duration_since(situation.oldest_reading) > Duration::minutes(FRESH_MIN) {
        return Decision::Stale;
    }

    let active = inputs
        .active
        .as_ref()
        .and_then(|id| inputs.accounts.iter().find(|account| &account.id == id));
    let Some(active) = active else {
        return match choose(inputs, now) {
            Some(to) => Decision::Switch {
                to,
                reason: Reason::NoneActive,
            },
            None => Decision::AllFull {
                next: next_free(inputs),
            },
        };
    };
    if blocked(active, now) {
        return match choose(inputs, now) {
            Some(to) => Decision::Switch {
                to,
                reason: Reason::ActiveFull {
                    until: usable_again(active, now),
                },
            },
            None => Decision::AllFull {
                next: next_free(inputs),
            },
        };
    }

    if situation.manual_hold_until.is_some_and(|until| until > now) {
        return Decision::Stay;
    }

    if !regime.early || !active.may_use {
        return Decision::Stay;
    }
    let Some(to) = choose(inputs, now) else {
        return Decision::Stay;
    };
    if to == active.id || !at_least_a_dwell_ago(situation.last_switch_at, now) {
        return Decision::Stay;
    }
    let Some(target) = inputs.accounts.iter().find(|account| account.id == to) else {
        return Decision::Stay;
    };
    let (Some(target_slack), Some(active_slack)) = (slack(target, inputs), slack(active, inputs))
    else {
        return Decision::Stay;
    };
    let margin = SLACK_MARGIN_H * inputs.rhythm.typical_hour();
    if target_slack > active_slack - margin {
        return Decision::Stay;
    }
    let reason = if target.session.is_some() {
        Reason::WindowOpen
    } else {
        Reason::ResetNear
    };
    if situation.tied_busy == 0 || target_slack <= 0.0 {
        Decision::Switch { to, reason }
    } else {
        Decision::Wait {
            to,
            latest: latest_reaching(inputs, target_slack),
            tied_busy: situation.tied_busy,
            reason,
        }
    }
}

/// Whether the last switch is at least [`MIN_DWELL_MIN`] ago; no known switch
/// counts as long ago.
fn at_least_a_dwell_ago(last_switch_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match last_switch_at {
        None => true,
        Some(last) => now.signed_duration_since(last) >= Duration::minutes(MIN_DWELL_MIN),
    }
}

/// The account auto-burn may use that is usable again first (spec §2.5, S3),
/// ties by account id; `None` when no such account has a time.
fn next_free(inputs: &Inputs) -> Option<(String, DateTime<Utc>)> {
    let mut best: Option<(String, DateTime<Utc>)> = None;
    for account in &inputs.accounts {
        if !account.may_use {
            continue;
        }
        let Some(at) = usable_again(account, inputs.now) else {
            continue;
        };
        let earlier = match &best {
            None => true,
            Some((id, when)) => at < *when || (at == *when && account.id < *id),
        };
        if earlier {
            best = Some((account.id.clone(), at));
        }
    }
    best
}

/// The end of the first [`STEP_MIN`] step at which the rhythm's work summed from
/// [`Inputs::now`] reaches `target_wu`; the horizon's end when it never does
/// (the decision is taken again at every reading).
fn latest_reaching(inputs: &Inputs, target_wu: f64) -> DateTime<Utc> {
    let step = Duration::minutes(STEP_MIN);
    let mut sum = 0.0;
    for (at, work) in rhythm_walk(inputs).steps_from(inputs.now) {
        sum += work;
        if sum >= target_wu {
            return at + step;
        }
    }
    inputs.now + Duration::hours(HORIZON_H)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burn::plan::testkit::{account, inputs, now, week, EPS};
    use crate::burn::plan::SessionState;
    use crate::burn::Rates;

    fn situation(inputs: Inputs) -> Situation {
        Situation {
            inputs,
            cost: Some(SwitchCost {
                wu: 0.05,
                samples: 3,
            }),
            last_switch_at: None,
            manual_hold_until: None,
            tied_busy: 0,
            oldest_reading: now() - Duration::minutes(1),
            sessions_off: false,
        }
    }

    /// An early regime, for the cases that test `decide` alone.
    fn pays() -> Regime {
        Regime {
            early: true,
            why: RegimeWhy::Pays,
            lazy_blocked_h: 1.0,
            eager_blocked_h: 0.0,
            extra_switches: 0,
            cost_h: Some(0.0),
        }
    }

    #[test]
    fn stale_readings_decide_nothing() {
        let mut situation = situation(inputs());
        situation.oldest_reading = now() - Duration::minutes(21);
        let state = regime(&situation);
        assert_eq!(decide(&situation, &state), Decision::Stale);
    }

    #[test]
    fn a_full_active_account_switches_at_once() {
        let mut inputs = inputs();
        inputs.accounts[0].week_percent = 100.0;
        let mut situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "AC".to_string(),
                reason: Reason::ActiveFull {
                    until: Some(now() + Duration::days(5) + Duration::hours(11)),
                },
            }
        );

        // Neither a busy tied session nor a manual hold holds S1 back.
        situation.tied_busy = 3;
        situation.manual_hold_until = Some(now() + Duration::hours(3));
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "AC".to_string(),
                reason: Reason::ActiveFull {
                    until: Some(now() + Duration::days(5) + Duration::hours(11)),
                },
            }
        );
    }

    #[test]
    fn everything_full_names_the_next_free_account() {
        let mut inputs = inputs();
        // PR and LB have their week at 100 %. AC's week has room; only its 5 h
        // window is full, until 14:40. That is a time at which AC can really
        // work again, so it is the next account.
        inputs.accounts[0].week_percent = 100.0;
        inputs.accounts[2].week_percent = 100.0;
        let session_reset = now() + Duration::hours(6) + Duration::minutes(40);
        inputs.accounts[1].session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(session_reset),
        });
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::AllFull {
                next: Some(("AC".to_string(), session_reset)),
            }
        );
    }

    #[test]
    fn the_active_account_blocked_by_week_and_window_is_free_at_the_later_reset() {
        let mut inputs = inputs();
        inputs.active = Some("AC".to_string());
        inputs.accounts[1].week_percent = 100.0;
        // The window resets at 14:40, long before AC's week resets on Monday
        // 02:00, so 14:40 is still a blocked time.
        let session_reset = now() + Duration::hours(6) + Duration::minutes(40);
        inputs.accounts[1].session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(session_reset),
        });
        let weekly_reset = inputs.accounts[1].week_resets_at;
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "LB".to_string(),
                reason: Reason::ActiveFull {
                    until: Some(weekly_reset),
                },
            }
        );
    }

    #[test]
    fn all_full_names_the_time_an_account_can_really_work() {
        let mut inputs = inputs();
        for account in &mut inputs.accounts {
            account.week_percent = 100.0;
        }
        // AC's window resets at 14:40, but its week is full until Monday 02:00;
        // PR's weekly reset on Saturday 19:00 comes first, so PR is the next
        // account that can really work.
        inputs.accounts[1].session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(now() + Duration::hours(6) + Duration::minutes(40)),
        });
        let pr_reset = inputs.accounts[0].week_resets_at;
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::AllFull {
                next: Some(("PR".to_string(), pr_reset)),
            }
        );
    }

    #[test]
    fn without_rates_there_is_no_early_switch() {
        let mut inputs = inputs();
        inputs.accounts[2].rates = None;
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(state.why, RegimeWhy::Learning);
        assert!(!state.early);
        assert_eq!(decide(&situation, &state), Decision::Stay);
    }

    #[test]
    fn sessions_off_is_the_regime_why() {
        let mut inputs = inputs();
        for account in &mut inputs.accounts {
            account.rates = None;
        }
        let mut situation = situation(inputs);
        situation.sessions_off = true;
        let state = regime(&situation);
        assert_eq!(state.why, RegimeWhy::SessionsOff);
        assert!(!state.early);
    }

    #[test]
    fn without_a_measured_cost_there_is_no_early_switch() {
        let mut situation = situation(inputs());
        situation.cost = None;
        let state = regime(&situation);
        assert_eq!(state.why, RegimeWhy::NoCostYet);
        assert_eq!(decide(&situation, &state), Decision::Stay);

        situation.cost = Some(SwitchCost {
            wu: 0.05,
            samples: 2,
        });
        let state = regime(&situation);
        assert_eq!(state.why, RegimeWhy::NoCostYet);
        assert_eq!(decide(&situation, &state), Decision::Stay);
    }

    #[test]
    fn small_windows_make_early_switching_pay() {
        let situation = situation(inputs());
        let state = regime(&situation);
        assert!(state.early);
        assert_eq!(state.why, RegimeWhy::Pays);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "AC".to_string(),
                reason: Reason::WindowOpen,
            }
        );
    }

    #[test]
    fn large_windows_make_it_not_worth_it() {
        let mut inputs = inputs();
        inputs.accounts[1].rates = Some(Rates {
            c: 4.0,
            s: Some(25.0),
        });
        inputs.accounts[2].rates = Some(Rates {
            c: 4.0,
            s: Some(20.0),
        });
        let mut situation = situation(inputs);
        situation.cost = Some(SwitchCost {
            wu: 2.0,
            samples: 3,
        });
        let state = regime(&situation);
        assert_eq!(state.why, RegimeWhy::NotWorthIt);
        assert_eq!(decide(&situation, &state), Decision::Stay);
    }

    #[test]
    fn a_manual_switch_holds_for_four_hours() {
        let mut situation = situation(inputs());
        situation.manual_hold_until = Some(now() + Duration::hours(3));
        let state = regime(&situation);
        assert_eq!(decide(&situation, &state), Decision::Stay);

        situation.manual_hold_until = Some(now() - Duration::minutes(1));
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "AC".to_string(),
                reason: Reason::WindowOpen,
            }
        );
    }

    #[test]
    fn a_recent_switch_holds_for_thirty_minutes() {
        let mut situation = situation(inputs());
        situation.last_switch_at = Some(now() - Duration::minutes(29));
        let state = regime(&situation);
        assert_eq!(decide(&situation, &state), Decision::Stay);

        situation.last_switch_at = Some(now() - Duration::minutes(30));
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "AC".to_string(),
                reason: Reason::WindowOpen,
            }
        );
    }

    #[test]
    fn a_tied_busy_session_makes_it_wait() {
        let mut inputs = inputs();
        // AC at 48 % still needs 52 / 4 = 13 wu. Its 19 windows before its reset
        // take 19 wu, so its slack is 19 - 13 = 6.0 wu: six working hours at the
        // test rhythm's 1.0 wu per working hour. LB's c of 10 lowers what its
        // week still needs to 100 / 10 = 10 wu, so its slack is 23.75 - 10 =
        // 13.75 wu and AC (6.0) stays the account choose() names.
        inputs.accounts[1].week_percent = 48.0;
        inputs.accounts[2].rates = Some(Rates {
            c: 10.0,
            s: Some(80.0),
        });
        let ac_slack = slack(&inputs.accounts[1], &inputs).expect("AC has rates");
        assert!((ac_slack - 6.0).abs() < EPS, "{ac_slack}");
        assert_eq!(choose(&inputs, now()), Some("AC".to_string()));

        let mut situation = situation(inputs);
        situation.tied_busy = 1;
        let state = regime(&situation);
        assert!(state.early);
        assert_eq!(
            decide(&situation, &state),
            Decision::Wait {
                to: "AC".to_string(),
                latest: now() + Duration::hours(6),
                tied_busy: 1,
                reason: Reason::WindowOpen,
            }
        );
    }

    #[test]
    fn waiting_ends_when_the_target_has_no_slack_left() {
        // The test setup's AC has slack -6.0 (19 windows take 19 wu, emptying it
        // needs 25), which is all this case needs: a slack at or below 0.
        let mut situation = situation(inputs());
        situation.tied_busy = 1;
        let ac_slack =
            slack(&situation.inputs.accounts[1], &situation.inputs).expect("AC has rates");
        assert!(ac_slack <= 0.0, "{ac_slack}");
        let state = regime(&situation);
        assert!(state.early);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "AC".to_string(),
                reason: Reason::WindowOpen,
            }
        );
    }

    #[test]
    fn an_account_auto_burn_may_not_use_is_never_a_target() {
        let mut inputs = inputs();
        inputs.accounts[1].may_use = false;
        let situation = situation(inputs);
        let state = regime(&situation);
        assert!(state.early);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "LB".to_string(),
                reason: Reason::WindowOpen,
            }
        );
    }

    #[test]
    fn an_active_account_auto_burn_may_not_use_is_left_only_when_blocked() {
        let mut inputs = inputs();
        inputs.active = Some("AC".to_string());
        inputs.accounts[1].may_use = false;
        // AC's c of 100 means its remaining 100 % needs only 1 wu; its 19 windows
        // take 19 wu, so its slack is 18.0 wu and LB (-1.25) lies more than the
        // 2 wu margin below it. The regime is stated directly: this case is
        // about decide, not about whether the eager plan pays.
        inputs.accounts[1].rates = Some(Rates {
            c: 100.0,
            s: Some(100.0),
        });
        let fresh = situation(inputs);
        let early = Regime {
            early: true,
            why: RegimeWhy::Pays,
            lazy_blocked_h: 1.0,
            eager_blocked_h: 0.0,
            extra_switches: 0,
            cost_h: Some(0.0),
        };
        assert_eq!(decide(&fresh, &early), Decision::Stay);

        // Blocked, S1 still leaves it: the target is another account.
        let mut blocked = fresh;
        blocked.inputs.accounts[1].week_percent = 100.0;
        assert_eq!(
            decide(&blocked, &early),
            Decision::Switch {
                to: "LB".to_string(),
                reason: Reason::ActiveFull {
                    until: Some(blocked.inputs.accounts[1].week_resets_at),
                },
            }
        );
    }

    #[test]
    fn no_typical_hour_means_learning() {
        let mut inputs = inputs();
        inputs.rhythm = week(0);
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(state.why, RegimeWhy::Learning);
        assert!(!state.early);
        assert!(state.lazy_blocked_h.abs() < EPS);
        assert!(state.eager_blocked_h.abs() < EPS);
        assert_eq!(state.extra_switches, 0);
        assert_eq!(state.cost_h, None);
    }

    #[test]
    fn a_capped_account_without_a_session_rate_is_learning() {
        // The plan treats a capped account without `s` as one without known
        // rates; the regime must use the same rule, not only `rates.is_none()`.
        let mut inputs = inputs();
        inputs.accounts[2].rates = Some(Rates { c: 4.0, s: None });
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(state.why, RegimeWhy::Learning);
        assert!(!state.early);
    }

    #[test]
    fn situation_check_rejects_a_clock_that_went_back() {
        let good = situation(inputs());
        assert!(good.check().is_ok());

        let mut future_switch = good.clone();
        future_switch.last_switch_at = Some(now() + Duration::minutes(1));
        let err = future_switch
            .check()
            .expect_err("a last switch after now is rejected");
        assert!(matches!(err, PlanError::LastSwitchAfterNow { .. }), "{err}");

        let mut future_reading = good;
        future_reading.oldest_reading = now() + Duration::minutes(1);
        let err = future_reading
            .check()
            .expect_err("a reading after now is rejected");
        assert!(
            matches!(err, PlanError::OldestReadingAfterNow { .. }),
            "{err}"
        );
    }

    #[test]
    fn situation_check_runs_the_inputs_check() {
        let mut situation = situation(inputs());
        situation
            .inputs
            .accounts
            .push(situation.inputs.accounts[0].clone());
        let err = situation
            .check()
            .expect_err("the inputs must still pass their own check");
        assert!(
            matches!(err, PlanError::DuplicateAccount { ref id } if id == "PR"),
            "{err}"
        );
    }

    #[test]
    fn a_target_whose_slack_is_within_the_margin_is_not_switched_to() {
        let mut inputs = inputs();
        // LB is full. PR's slack is 43 wu (92 work units before its reset minus
        // 49 needed); AC is played uncapped with c = 2 and the next Monday's
        // reset, so its slack is 92 - 50 = 42 wu. That is one work unit below
        // PR and inside the 2-hour margin, so there is no early switch.
        inputs.accounts[2].week_percent = 100.0;
        inputs.accounts[0].week_percent = 2.0;
        inputs.accounts[1].session = None;
        inputs.accounts[1].rates = Some(Rates { c: 2.0, s: None });
        let pr_slack = slack(&inputs.accounts[0], &inputs).expect("PR has rates");
        let ac_slack = slack(&inputs.accounts[1], &inputs).expect("AC has rates");
        assert!((pr_slack - 43.0).abs() < EPS, "{pr_slack}");
        assert!((ac_slack - 42.0).abs() < EPS, "{ac_slack}");
        assert_eq!(choose(&inputs, now()), Some("AC".to_string()));

        let situation = situation(inputs);
        assert_eq!(decide(&situation, &pays()), Decision::Stay);
    }

    #[test]
    fn an_uncapped_target_switches_for_its_reset_near() {
        let mut inputs = inputs();
        inputs.active = Some("AC".to_string());
        // LB is full. AC's slack is 19 - (100 - 84) / 4 = 15 wu; PR's is
        // 92 - (100 - 13) / 1 = 5 wu, so choose() names the uncapped PR.
        inputs.accounts[2].week_percent = 100.0;
        inputs.accounts[1].week_percent = 84.0;
        inputs.accounts[0].week_percent = 13.0;
        inputs.accounts[0].rates = Some(Rates { c: 1.0, s: None });
        assert_eq!(choose(&inputs, now()), Some("PR".to_string()));
        let situation = situation(inputs);
        assert_eq!(
            decide(&situation, &pays()),
            Decision::Switch {
                to: "PR".to_string(),
                reason: Reason::ResetNear,
            }
        );
    }

    #[test]
    fn a_gain_below_the_minimum_is_not_worth_it() {
        let mut inputs = inputs();
        // One account only: both plans are the same and gain nothing, so
        // extra_switches is 0 and the gain is below MIN_GAIN_H.
        inputs.accounts.truncate(1);
        inputs.active = Some("PR".to_string());
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(state.extra_switches, 0);
        assert!(
            (state.lazy_blocked_h - state.eager_blocked_h).abs() < EPS,
            "{} vs {}",
            state.lazy_blocked_h,
            state.eager_blocked_h
        );
        assert_eq!(state.why, RegimeWhy::NotWorthIt);
        assert!(!state.early);
    }

    #[test]
    fn all_full_skips_an_account_auto_burn_may_not_use() {
        let mut inputs = inputs();
        for account in &mut inputs.accounts {
            account.week_percent = 100.0;
        }
        // PR would be usable again first (Saturday 19:00), but auto-burn may not
        // use it, so AC (Monday 02:00) is next.
        inputs.accounts[0].may_use = false;
        let ac_reset = inputs.accounts[1].week_resets_at;
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::AllFull {
                next: Some(("AC".to_string(), ac_reset)),
            }
        );
    }

    #[test]
    fn all_full_breaks_an_equal_time_by_account_id() {
        let reset = "2026-11-21T19:00:00Z";
        let mut lb = account("LB", 4.0, Some(80.0), true, reset);
        lb.week_percent = 100.0;
        let mut ac = account("AC", 4.0, Some(100.0), true, reset);
        ac.week_percent = 100.0;
        let mut inputs = inputs();
        // LB comes first and has the same time as AC; the id rule takes AC.
        inputs.accounts = vec![lb, ac];
        inputs.active = Some("LB".to_string());
        let when = inputs.accounts[1].week_resets_at;
        let situation = situation(inputs);
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::AllFull {
                next: Some(("AC".to_string(), when)),
            }
        );
    }

    #[test]
    fn a_manual_hold_that_ends_now_is_no_hold() {
        let mut situation = situation(inputs());
        situation.manual_hold_until = Some(now());
        let state = regime(&situation);
        assert_eq!(
            decide(&situation, &state),
            Decision::Switch {
                to: "AC".to_string(),
                reason: Reason::WindowOpen,
            }
        );
    }
}
