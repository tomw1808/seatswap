//! Slack, choice and the two forward plans of auto-burn (spec §2.2–2.5): how
//! much work each account can still take before its reset, which account to
//! use, and what the next 168 hours look like when it is switched only when
//! blocked (lazy) or as early as it pays (eager).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::{DateTime, Duration, FixedOffset, Utc};

use super::{Rates, Rhythm, HORIZON_H, MIN_DWELL_MIN, SLACK_MARGIN_H, STEP_MIN};

/// A 5 h window lasts this many hours (spec §2.1).
const WINDOW_H: i64 = 5;
/// The work of one step: a quarter of the work of the hour that holds it.
const STEP_FRACTION: f64 = STEP_MIN as f64 / 60.0;
/// Two slacks within this are equal: a smaller difference is rounding noise.
const SLACK_TIE: f64 = 1e-9;

/// An account's 5 h window at the moment of a decision (spec §2.1). A window
/// has a reset time only while it runs; `None` means no window is open.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// One account as the plan needs it (spec §2.3).
#[derive(Clone)]
pub struct AccountState {
    pub id: String,
    pub may_use: bool,
    pub login_ok: bool,
    pub week_percent: f64,
    pub week_resets_at: DateTime<Utc>,
    /// `None`: the account has no 5 h limit at all. It is the only sign of a
    /// capped account: `rates.s` is `None` also while that rate is not learned
    /// yet, and such an account is played like one without rates.
    pub session: Option<SessionState>,
    pub model_blocked: bool,
    pub rates: Option<Rates>,
}

/// Everything a plan is computed from. It is pure: no clock, no files, no
/// network (spec §2.4).
#[derive(Clone)]
pub struct Inputs {
    pub now: DateTime<Utc>,
    pub offset: FixedOffset,
    pub accounts: Vec<AccountState>,
    pub active: Option<String>,
    pub rhythm: Rhythm,
}

/// A value [`Inputs::check`] rejects, because no plan can be computed from it.
/// Every error that concerns an account names it.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(
        "account {account}: its {limit} percent {percent} is not a finite number of at least 0"
    )]
    InvalidPercent {
        account: String,
        limit: &'static str,
        percent: f64,
    },
    #[error("account {account}: its session is at {percent} % without a reset time")]
    SessionWithoutReset { account: String, percent: f64 },
    #[error("the active account {id} is not in accounts")]
    UnknownActive { id: String },
    #[error("two accounts have the id {id}")]
    DuplicateAccount { id: String },
}

impl Inputs {
    /// Checks the values every caller must reject before [`slack`], [`choose`]
    /// or [`follow`] (the tray app's auto-burn calls it first). An error names
    /// the account it concerns: a percent that is NaN, infinite or negative, a
    /// session percent above 0 without a reset time (spec §2.1: a window has a
    /// reset time only while it runs), an `active` id that is not in
    /// `accounts`, or two accounts with the same id.
    pub fn check(&self) -> Result<(), PlanError> {
        for (index, account) in self.accounts.iter().enumerate() {
            if self.accounts[..index]
                .iter()
                .any(|other| other.id == account.id)
            {
                return Err(PlanError::DuplicateAccount {
                    id: account.id.clone(),
                });
            }
        }
        for account in &self.accounts {
            if !valid_percent(account.week_percent) {
                return Err(PlanError::InvalidPercent {
                    account: account.id.clone(),
                    limit: "week",
                    percent: account.week_percent,
                });
            }
            if let Some(session) = &account.session {
                if !valid_percent(session.percent) {
                    return Err(PlanError::InvalidPercent {
                        account: account.id.clone(),
                        limit: "session",
                        percent: session.percent,
                    });
                }
                if session.percent > 0.0 && session.resets_at.is_none() {
                    return Err(PlanError::SessionWithoutReset {
                        account: account.id.clone(),
                        percent: session.percent,
                    });
                }
            }
        }
        if let Some(active) = &self.active {
            if !self.accounts.iter().any(|account| &account.id == active) {
                return Err(PlanError::UnknownActive { id: active.clone() });
            }
        }
        Ok(())
    }
}

fn valid_percent(percent: f64) -> bool {
    percent.is_finite() && percent >= 0.0
}

/// The two plans of spec §2.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Stay on the current account until it is not usable.
    Lazy,
    /// Also move to an account with clearly less slack.
    Eager,
}

/// One stretch of the plan played with one account. `account` is `None` while
/// nothing is usable; such a stretch blocks the work inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub account: Option<String>,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// The next [`HORIZON_H`] hours played in [`STEP_MIN`] steps (spec §2.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub segments: Vec<Segment>,
    pub blocked_wu: f64,
    pub switches: u32,
    /// Week-% unused at each account's first weekly reset inside the horizon.
    pub left_at_reset: BTreeMap<String, f64>,
    /// The first step at which each account is full: its week or its 5 h
    /// window is at 100 %.
    pub full_at: BTreeMap<String, DateTime<Utc>>,
}

/// Whether the account can work at `at` (spec §2.3): it may be used, its login
/// is fine, its week is below 100 %, its 5 h window is below 100 % or over, and
/// no per-model limit blocks it.
pub fn usable(account: &AccountState, at: DateTime<Utc>) -> bool {
    View::of(account, at).usable(at)
}

/// The work units this account can still take before its weekly reset, minus
/// the work units needed to empty it (spec §2.3). The walk runs in [`STEP_MIN`]
/// steps from [`Inputs::now`]; a capped account takes at most
/// `(100 - session-%) / s` work units per window, a window opens at the first
/// step with work after the previous one ended and lasts 5 h. A weekly reset at
/// or before [`Inputs::now`] means that week is over: the account is played
/// from 0 % with its next reset, as the 5 h window already is when its reset
/// has passed. `None` while the rates are unknown, which includes a capped
/// account without `s`. Task 4 returns no rate of 0 or below (it returns
/// `None`), so neither division here can be by zero from a rate.
pub fn slack(account: &AccountState, inputs: &Inputs) -> Option<f64> {
    View::of(account, inputs.now).slack(inputs, inputs.now)
}

/// The usable account with the least slack (spec §2.3), ties broken by the
/// earlier weekly reset and then by the account id, so the result never depends
/// on the order of [`Inputs::accounts`]. Two slacks within 1e-9 are a tie: a
/// difference that small is rounding noise, not a reason to skip the reset and
/// id rule. The percents are those of [`Inputs::now`], so an `at` other than
/// `now` only moves the rhythm and the resets. If any usable account has no
/// rates, the fixed order of spec §2.3 is used instead: capped accounts whose
/// window has room first, then the others, each group by the earlier weekly
/// reset and then the id.
pub fn choose(inputs: &Inputs, at: DateTime<Utc>) -> Option<String> {
    let views: Vec<View<'_>> = inputs
        .accounts
        .iter()
        .map(|account| View::of(account, at))
        .collect();
    let index = pick(&views, inputs, at)?;
    inputs.accounts.get(index).map(|account| account.id.clone())
}

/// Play the next [`HORIZON_H`] hours in [`STEP_MIN`] steps (spec §2.4). Every
/// step has the work of its rhythm hour divided by four. Lazy stays on the
/// current account until it is not usable and then calls [`choose`]. Eager also
/// moves when [`choose`] names another account whose slack is at least
/// [`SLACK_MARGIN_H`] × [`Rhythm::typical_hour`] below the current one's and the
/// last switch is at least [`MIN_DWELL_MIN`] ago.
///
/// An account without rates is played with `c = 0` and `s = 0`: it never fills
/// in the plan and it is never the reason for an eager move, because its slack
/// is unknown. The same holds for a capped account without a session rate. When
/// [`Rhythm::typical_hour`] is 0.0 the margin has no unit, so there is no eager
/// move at all and the eager plan is the lazy one. Weekly resets repeat every 7
/// days from the account's reset time; a weekly reset at or before
/// [`Inputs::now`] means that week is over: the account is played from 0 % with
/// its next reset. A 5 h window opens at the first step with work after the
/// previous one ended and lasts 5 h.
pub fn follow(inputs: &Inputs, policy: Policy) -> Plan {
    let step = Duration::minutes(STEP_MIN);
    let steps = HORIZON_H * 60 / STEP_MIN;
    let horizon_end = inputs.now + Duration::hours(HORIZON_H);
    let typical = inputs.rhythm.typical_hour();
    let mut views: Vec<View<'_>> = inputs
        .accounts
        .iter()
        .map(|account| View::of(account, inputs.now))
        .collect();
    for view in &mut views {
        view.clear_resets(inputs.now, horizon_end);
        if view.is_full(inputs.now) {
            view.full_at = Some(inputs.now);
        }
    }
    let mut current: Option<usize> = inputs
        .active
        .as_ref()
        .and_then(|id| views.iter().position(|view| &view.account.id == id))
        .filter(|&index| views[index].usable(inputs.now));
    let mut last_switch: Option<DateTime<Utc>> = None;
    let mut blocked_wu = 0.0;
    let mut switches = 0u32;
    let mut segments: Vec<Segment> = Vec::new();

    for index in 0..steps {
        let t = inputs.now + Duration::minutes(STEP_MIN * index);

        for view in &mut views {
            view.clear_resets(t, horizon_end);
        }

        let work = step_work(inputs, t);
        let choice = match current {
            Some(index) if views[index].usable(t) => {
                if policy == Policy::Eager && typical > 0.0 {
                    eager_move(&views, inputs, t, index, typical, last_switch).or(Some(index))
                } else {
                    Some(index)
                }
            }
            _ => pick(&views, inputs, t),
        };

        match choice {
            Some(index) => {
                if current != Some(index) {
                    if current.is_some() {
                        switches += 1;
                    }
                    last_switch = Some(t);
                }
                current = Some(index);
                if work > 0.0 {
                    views[index].apply(t, work);
                    if views[index].full_at.is_none() && views[index].is_full(t) {
                        views[index].full_at = Some(t);
                    }
                }
            }
            None => {
                if work > 0.0 {
                    blocked_wu += work;
                }
            }
        }

        let name = choice.map(|index| views[index].account.id.clone());
        match segments.last_mut() {
            Some(last) if last.account == name => last.to = t + step,
            _ => segments.push(Segment {
                account: name,
                from: t,
                to: t + step,
            }),
        }
    }

    for view in &mut views {
        view.clear_resets(horizon_end, horizon_end);
    }

    Plan {
        segments,
        blocked_wu,
        switches,
        left_at_reset: views
            .iter()
            .filter_map(|view| {
                view.left_at_reset
                    .map(|left| (view.account.id.clone(), left))
            })
            .collect(),
        full_at: views
            .iter()
            .filter_map(|view| view.full_at.map(|at| (view.account.id.clone(), at)))
            .collect(),
    }
}

/// One account's state while a plan is played. The public functions build one
/// per account; [`follow`] keeps a vector and moves it forward.
#[derive(Clone, Copy)]
struct View<'a> {
    account: &'a AccountState,
    week: f64,
    session: f64,
    window_end: Option<DateTime<Utc>>,
    reset: DateTime<Utc>,
    full_at: Option<DateTime<Utc>>,
    left_at_reset: Option<f64>,
}

impl<'a> View<'a> {
    fn of(account: &'a AccountState, at: DateTime<Utc>) -> View<'a> {
        let (session, window_end) = match &account.session {
            Some(state) => (state.percent, state.resets_at),
            None => (0.0, None),
        };
        View {
            account,
            week: if account.week_resets_at <= at {
                0.0
            } else {
                account.week_percent
            },
            session,
            window_end,
            reset: next_reset(account.week_resets_at, at),
            full_at: None,
            left_at_reset: None,
        }
    }

    fn capped(&self) -> bool {
        self.account.session.is_some()
    }

    /// The rates the account can be planned with. `None` while they are
    /// unknown, including a capped account whose `s` is not learned yet.
    fn rates(&self) -> Option<Rates> {
        let rates = self.account.rates?;
        if self.capped() && rates.s.is_none() {
            None
        } else {
            Some(rates)
        }
    }

    /// The rates work is applied with: `c = 0`, `s = 0` when they are unknown,
    /// so such an account never fills (the rule the plan states).
    fn play_rates(&self) -> (f64, f64) {
        match self.rates() {
            Some(rates) => (rates.c, rates.s.unwrap_or(0.0)),
            None => (0.0, 0.0),
        }
    }

    /// Clear what has ended at `at`: a weekly reset that has passed (the week
    /// is over and starts again at 0 %, its unused percent recorded if the
    /// reset is inside the horizon) and a 5 h window whose end has passed.
    fn clear_resets(&mut self, at: DateTime<Utc>, horizon_end: DateTime<Utc>) {
        while self.reset <= at {
            if self.left_at_reset.is_none() && self.reset < horizon_end {
                self.left_at_reset = Some(100.0 - self.week);
            }
            self.week = 0.0;
            self.reset += Duration::weeks(1);
        }
        if self.window_end.is_some_and(|end| at >= end) {
            self.window_end = None;
            self.session = 0.0;
        }
    }

    fn window_over(&self, at: DateTime<Utc>) -> bool {
        self.window_end.is_some_and(|end| at >= end)
    }

    fn window_blocked(&self, at: DateTime<Utc>) -> bool {
        self.capped() && self.session >= 100.0 && !self.window_over(at)
    }

    /// An account is full when its week is at 100 % or its 5 h window is at
    /// 100 % and not over: the one rule [`usable`] uses too.
    fn is_full(&self, at: DateTime<Utc>) -> bool {
        self.week >= 100.0 || self.window_blocked(at)
    }

    fn usable(&self, at: DateTime<Utc>) -> bool {
        self.account.may_use
            && self.account.login_ok
            && !self.account.model_blocked
            && !self.is_full(at)
    }

    fn slack(&self, inputs: &Inputs, at: DateTime<Utc>) -> Option<f64> {
        let rates = self.rates()?;
        let week = self.week;
        let mut view = *self;
        let horizon_end = inputs.now + Duration::hours(HORIZON_H);
        let mut usable_wu = 0.0;
        let mut cursor = at;
        let step = Duration::minutes(STEP_MIN);
        while cursor < view.reset {
            view.clear_resets(cursor, horizon_end);
            let work = step_work(inputs, cursor);
            if work > 0.0 {
                usable_wu += view.apply(cursor, work);
            }
            cursor += step;
        }
        Some(usable_wu - (100.0 - week) / rates.c)
    }

    /// Apply one step's work to the account: its week grows by `c`, its open
    /// 5 h window by `s`, and a window opens when none runs. Unknown rates are
    /// 0. Returns the work units the account can take in this step: all of it,
    /// or the part the open window still has room for.
    fn apply(&mut self, at: DateTime<Utc>, work: f64) -> f64 {
        let (c, s) = self.play_rates();
        let mut taken = work;
        if self.capped() {
            if self.window_end.is_none_or(|end| at >= end) {
                self.window_end = Some(at + Duration::hours(WINDOW_H));
                self.session = 0.0;
            }
            if s > 0.0 {
                taken = work.min((100.0 - self.session) / s);
            }
            self.session = (self.session + s * work).min(100.0);
        }
        self.week = (self.week + c * work).min(100.0);
        taken
    }
}

/// The index of the account [`choose`] would name: the usable account with the
/// least slack, or the fixed order when any usable account has no rates.
fn pick(views: &[View<'_>], inputs: &Inputs, at: DateTime<Utc>) -> Option<usize> {
    let mut usable: Vec<usize> = (0..views.len())
        .filter(|&index| views[index].usable(at))
        .collect();
    if usable.is_empty() {
        return None;
    }
    if usable
        .iter()
        .any(|&index| views[index].slack(inputs, at).is_none())
    {
        let group = |index: usize| u8::from(!views[index].capped());
        usable.sort_by(|&a, &b| {
            group(a)
                .cmp(&group(b))
                .then_with(|| {
                    next_reset(views[a].account.week_resets_at, at)
                        .cmp(&next_reset(views[b].account.week_resets_at, at))
                })
                .then_with(|| views[a].account.id.cmp(&views[b].account.id))
        });
        return usable.first().copied();
    }
    let mut best: Option<(f64, DateTime<Utc>, usize)> = None;
    for &index in &usable {
        let Some(slack) = views[index].slack(inputs, at) else {
            continue;
        };
        let reset = next_reset(views[index].account.week_resets_at, at);
        let better = match best {
            None => true,
            Some((best_slack, best_reset, best_index)) => {
                let slack_order = if (slack - best_slack).abs() <= SLACK_TIE {
                    Ordering::Equal
                } else {
                    slack.total_cmp(&best_slack)
                };
                slack_order
                    .then_with(|| reset.cmp(&best_reset))
                    .then_with(|| views[index].account.id.cmp(&views[best_index].account.id))
                    == Ordering::Less
            }
        };
        if better {
            best = Some((slack, reset, index));
        }
    }
    best.map(|(_, _, index)| index)
}

/// The early move of the eager plan (spec §2.5, S2): the account [`choose`]
/// names, when it is another one, its slack is at least the margin below the
/// current account's, and the last switch is at least [`MIN_DWELL_MIN`] ago.
/// A target without known rates is never a reason to move.
fn eager_move(
    views: &[View<'_>],
    inputs: &Inputs,
    at: DateTime<Utc>,
    current: usize,
    typical: f64,
    last_switch: Option<DateTime<Utc>>,
) -> Option<usize> {
    if last_switch.is_some_and(|last| at - last < Duration::minutes(MIN_DWELL_MIN)) {
        return None;
    }
    let target = pick(views, inputs, at)?;
    if target == current {
        return None;
    }
    let target_slack = views[target].slack(inputs, at)?;
    let current_slack = views[current].slack(inputs, at)?;
    if target_slack <= current_slack - SLACK_MARGIN_H * typical {
        Some(target)
    } else {
        None
    }
}

/// The first weekly reset after `at`: the account's reset time repeated every
/// 7 days.
fn next_reset(reset: DateTime<Utc>, at: DateTime<Utc>) -> DateTime<Utc> {
    let mut next = reset;
    while next <= at {
        next += Duration::weeks(1);
    }
    next
}

fn step_work(inputs: &Inputs, at: DateTime<Utc>) -> f64 {
    inputs.rhythm.at(at, inputs.offset) * STEP_FRACTION
}

/// The setup the plan's tests share with the later auto-burn tests (Tasks 6
/// and 7 use it; they do not copy it).
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;

    pub(crate) const EPS: f64 = 1e-9;

    /// The plan's `now`: Monday 08:00 UTC.
    pub(crate) const NOW: &str = "2026-11-16T08:00:00Z";

    pub(crate) fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("test date")
    }

    pub(crate) fn now() -> DateTime<Utc> {
        utc(NOW)
    }

    pub(crate) fn offset() -> FixedOffset {
        FixedOffset::east_opt(0).expect("UTC")
    }

    /// The rhythm of the tests: six working days, one work unit per working
    /// hour, a free Sunday.
    ///
    /// Monday to Friday work `hours_per_weekday` hours in three 5 h blocks —
    /// 07:00–12:00, 12:00–17:00 and 18:00–23:00 for 15 — and Saturday works 18
    /// hours in four blocks, 00:00–05:00, 05:00–10:00, 10:00–15:00 and
    /// 15:00–18:00 for 15. Monday's 07:00–08:00 lies before `now`, so the work
    /// before PR's reset on Saturday 19:00 is 14 + 4 × 15 + 18 = 92 work units.
    /// Each block opens one 5 h window; the five weekdays give three windows
    /// each and Saturday four, so AC can use 19 work units in its windows
    /// before its reset, not the 18 the plan expected: 18 windows hold at most
    /// 18 × 5 = 90 work units of work. The tests assert those numbers.
    ///
    /// `hours_per_weekday` is 0 (an empty week, the "no rhythm" case) or
    /// 10..=16: those are the values the block arithmetic below yields valid
    /// hours of the week for.
    pub(crate) fn week(hours_per_weekday: i64) -> Rhythm {
        assert!(
            hours_per_weekday == 0 || (10..=16).contains(&hours_per_weekday),
            "week: hours_per_weekday must be 0 or 10..=16, got {hours_per_weekday}"
        );
        let mut wu = [0.0f64; 168];
        if hours_per_weekday == 0 {
            return Rhythm { wu };
        }
        let third_end = 18 + (hours_per_weekday - 10) as u32;
        let saturday_end = 15 + (hours_per_weekday - 12) as u32;
        let weekday = [(7u32, 12u32), (12, 17), (18, third_end)];
        let saturday = [(0, 5), (5, 10), (10, 15), (15, saturday_end)];
        for day in 0..7u32 {
            let blocks: &[(u32, u32)] = match day {
                0..=4 => &weekday,
                5 => &saturday,
                _ => &[],
            };
            for &(from, to) in blocks {
                for hour in from..to {
                    wu[(day * 24 + hour) as usize] = 1.0;
                }
            }
        }
        Rhythm { wu }
    }

    pub(crate) fn account(
        id: &str,
        c: f64,
        s: Option<f64>,
        capped: bool,
        reset: &str,
    ) -> AccountState {
        AccountState {
            id: id.to_string(),
            may_use: true,
            login_ok: true,
            week_percent: 0.0,
            week_resets_at: utc(reset),
            session: capped.then_some(SessionState {
                percent: 0.0,
                resets_at: None,
            }),
            model_blocked: false,
            rates: Some(Rates { c, s }),
        }
    }

    /// The three accounts of `specs/sim/burn_sim.py`: PR uncapped, AC and LB
    /// capped, at their resets.
    pub(crate) fn inputs() -> Inputs {
        Inputs {
            now: now(),
            offset: offset(),
            accounts: vec![
                account("PR", 2.0, None, false, "2026-11-21T19:00:00Z"),
                account("AC", 4.0, Some(100.0), true, "2026-11-23T02:00:00Z"),
                account("LB", 4.0, Some(80.0), true, "2026-11-23T17:00:00Z"),
            ],
            active: Some("PR".to_string()),
            rhythm: week(15),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    #[test]
    fn usable_follows_the_limits() {
        let base = inputs().accounts[1].clone();
        assert!(usable(&base, now()));

        let mut full_week = base.clone();
        full_week.week_percent = 100.0;
        assert!(!usable(&full_week, now()));

        let mut full_window = base.clone();
        full_window.session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(now() + Duration::hours(1)),
        });
        assert!(!usable(&full_window, now()));

        let mut window_over = base.clone();
        window_over.session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(now() - Duration::minutes(1)),
        });
        assert!(usable(&window_over, now()));

        let mut off = base.clone();
        off.may_use = false;
        assert!(!usable(&off, now()));

        let mut no_login = base.clone();
        no_login.login_ok = false;
        assert!(!usable(&no_login, now()));

        let mut model_blocked = base;
        model_blocked.model_blocked = true;
        assert!(!usable(&model_blocked, now()));
    }

    #[test]
    fn slack_of_an_uncapped_account_is_work_left_minus_work_needed() {
        let inputs = inputs();
        let slack = slack(&inputs.accounts[0], &inputs).expect("PR has rates");
        assert!((slack - 42.0).abs() < EPS, "{slack}");
    }

    #[test]
    fn slack_of_a_capped_account_counts_windows() {
        // AC needs 100 / 4 = 25 wu. Its 19 windows (three on each of the five
        // weekdays, four on Saturday) take 1.0 wu each, so 19 - 25 = -6.0.
        let inputs = inputs();
        let slack = slack(&inputs.accounts[1], &inputs).expect("AC has c and s");
        assert!((slack - (-6.0)).abs() < EPS, "{slack}");
    }

    #[test]
    fn choose_takes_the_least_slack() {
        assert_eq!(choose(&inputs(), now()), Some("AC".to_string()));
    }

    #[test]
    fn choose_without_rates_prefers_capped_accounts_with_room() {
        let mut inputs = inputs();
        for account in &mut inputs.accounts {
            account.rates = None;
        }
        assert_eq!(choose(&inputs, now()), Some("AC".to_string()));

        inputs.accounts[1].session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(now() + Duration::hours(1)),
        });
        assert_eq!(choose(&inputs, now()), Some("LB".to_string()));

        inputs.accounts[2].session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(now() + Duration::hours(1)),
        });
        assert_eq!(choose(&inputs, now()), Some("PR".to_string()));
    }

    #[test]
    fn lazy_stays_until_blocked() {
        let plan = follow(&inputs(), Policy::Lazy);
        assert_eq!(plan.segments[0].account.as_deref(), Some("PR"));
        assert_eq!(plan.segments[0].from, now());
        let full = plan.full_at.get("PR").copied().expect("PR fills");
        assert_eq!(plan.segments[0].to, full + Duration::minutes(STEP_MIN));
        assert_ne!(plan.segments[1].account.as_deref(), Some("PR"));
    }

    #[test]
    fn eager_is_blocked_less_than_lazy_when_windows_are_small() {
        let inputs = inputs();
        let lazy = follow(&inputs, Policy::Lazy);
        let eager = follow(&inputs, Policy::Eager);
        assert!((eager.blocked_wu - 0.0).abs() < EPS, "{}", eager.blocked_wu);
        assert!(lazy.blocked_wu > 15.0, "{}", lazy.blocked_wu);
    }

    /// The eager move of `inputs()` away from PR, `minutes` after a switch.
    fn eager_target_after(minutes: i64) -> Option<usize> {
        let inputs = inputs();
        let views: Vec<View<'_>> = inputs
            .accounts
            .iter()
            .map(|account| View::of(account, now()))
            .collect();
        eager_move(
            &views,
            &inputs,
            now(),
            0, // PR
            inputs.rhythm.typical_hour(),
            Some(now() - Duration::minutes(minutes)),
        )
    }

    #[test]
    fn eager_respects_the_dwell() {
        let inputs = inputs();
        let plan = follow(&inputs, Policy::Eager);
        for pair in plan.segments.windows(2) {
            let (before, after) = (&pair[0], &pair[1]);
            let (Some(from), Some(to)) = (&before.account, &after.account) else {
                continue;
            };
            if from == to {
                continue;
            }
            let gap = after.from.signed_duration_since(before.from);
            if gap < Duration::minutes(MIN_DWELL_MIN) {
                // Allowed only where the account in use is not usable at the
                // later start: being full somewhere later in the week is not
                // the rule.
                let index = inputs
                    .accounts
                    .iter()
                    .position(|account| &account.id == from)
                    .expect("the segment names an account");
                assert!(
                    !usable(&inputs.accounts[index], after.from),
                    "{from} gave way after {gap:?} while still usable at {:?}",
                    after.from
                );
            }
        }
        // The move itself waits for the dwell: 29 minutes is too soon.
        assert_eq!(eager_target_after(29), None);
        assert_eq!(eager_target_after(30), Some(1)); // AC
    }

    #[test]
    fn eager_move_waits_for_the_dwell() {
        assert_eq!(eager_target_after(29), None);
        assert_eq!(eager_target_after(30), Some(1)); // AC
    }

    #[test]
    fn a_week_that_resets_inside_the_plan_starts_at_zero() {
        let mut inputs = inputs();
        let reset = inputs.now + Duration::hours(2);
        inputs.accounts[0].week_percent = 99.0;
        inputs.accounts[0].week_resets_at = reset;

        let plan = follow(&inputs, Policy::Lazy);
        let left = plan
            .left_at_reset
            .get("PR")
            .copied()
            .expect("PR resets inside the plan");
        assert!(left < 1.0, "{left}");
        assert!(
            plan.segments
                .iter()
                .any(|segment| segment.account.as_deref() == Some("PR") && segment.from >= reset),
            "{:?}",
            plan.segments
        );
    }

    #[test]
    fn nothing_usable_is_a_segment_with_no_account() {
        let mut inputs = inputs();
        for account in &mut inputs.accounts {
            account.week_percent = 100.0;
        }
        let plan = follow(&inputs, Policy::Lazy);
        assert_eq!(plan.segments[0].account, None);
        assert_eq!(plan.segments[0].from, now());
        // PR's weekly reset is the earliest one in the horizon.
        assert_eq!(plan.segments[0].to, utc("2026-11-21T19:00:00Z"));
        assert_eq!(plan.segments[1].account.as_deref(), Some("PR"));
    }

    #[test]
    fn an_account_without_rates_never_fills() {
        let mut inputs = inputs();
        inputs.accounts[2].rates = None;
        let plan = follow(&inputs, Policy::Lazy);
        assert!(!plan.full_at.contains_key("LB"), "{:?}", plan.full_at);
    }

    #[test]
    fn a_capped_account_without_a_session_rate_never_fills_its_window() {
        let mut inputs = inputs();
        inputs.accounts[1].rates = Some(Rates { c: 4.0, s: None });
        assert!(slack(&inputs.accounts[1], &inputs).is_none());

        let plan = follow(&inputs, Policy::Lazy);
        assert!(!plan.full_at.contains_key("AC"), "{:?}", plan.full_at);
    }

    #[test]
    fn no_rhythm_means_no_eager_move() {
        let mut inputs = inputs();
        // AC active: with a margin of 0 the eager plan would want PR, whose
        // slack is smaller, so the typical > 0.0 guard is what stops it.
        inputs.active = Some("AC".to_string());
        inputs.rhythm = week(0);
        assert!((inputs.rhythm.typical_hour() - 0.0).abs() < EPS);

        let lazy = follow(&inputs, Policy::Lazy);
        let eager = follow(&inputs, Policy::Eager);
        assert_eq!(eager.segments, lazy.segments);
        assert_eq!(eager.switches, 0);
        assert!((eager.blocked_wu - 0.0).abs() < EPS);
    }

    #[test]
    fn a_window_whose_reset_has_passed_is_not_full() {
        let mut inputs = inputs();
        inputs.accounts[1].session = Some(SessionState {
            percent: 100.0,
            resets_at: Some(inputs.now - Duration::minutes(1)),
        });
        assert!(usable(&inputs.accounts[1], inputs.now));

        let plan = follow(&inputs, Policy::Lazy);
        assert_ne!(
            plan.full_at.get("AC"),
            Some(&inputs.now),
            "{:?}",
            plan.full_at
        );
    }

    #[test]
    fn check_rejects_a_percent_that_is_not_finite_or_negative() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.5] {
            let mut week_inputs = inputs();
            week_inputs.accounts[1].week_percent = bad;
            let err = week_inputs.check().expect_err("a bad percent is rejected");
            assert!(
                matches!(err, PlanError::InvalidPercent { ref account, limit: "week", .. } if account == "AC"),
                "{err}"
            );

            let mut session_inputs = inputs();
            session_inputs.accounts[1].session = Some(SessionState {
                percent: bad,
                resets_at: Some(session_inputs.now + Duration::hours(1)),
            });
            let err = session_inputs
                .check()
                .expect_err("a bad percent is rejected");
            assert!(
                matches!(err, PlanError::InvalidPercent { ref account, limit: "session", .. } if account == "AC"),
                "{err}"
            );
        }
    }

    #[test]
    fn check_rejects_a_session_percent_without_a_reset_time() {
        let mut inputs = inputs();
        inputs.accounts[1].session = Some(SessionState {
            percent: 5.0,
            resets_at: None,
        });
        let err = inputs.check().expect_err("a running window needs a reset");
        assert!(
            matches!(err, PlanError::SessionWithoutReset { ref account, .. } if account == "AC"),
            "{err}"
        );
        // An idle capped account is at 0 % and has no reset time, which is fine.
        assert!(super::testkit::inputs().check().is_ok());
    }

    #[test]
    fn check_rejects_an_active_account_that_is_not_known() {
        let mut inputs = inputs();
        inputs.active = Some("XX".to_string());
        let err = inputs.check().expect_err("the active account must exist");
        assert!(
            matches!(err, PlanError::UnknownActive { ref id } if id == "XX"),
            "{err}"
        );
    }

    #[test]
    fn check_rejects_two_accounts_with_the_same_id() {
        let mut inputs = inputs();
        inputs.accounts.push(account(
            "AC",
            4.0,
            Some(100.0),
            true,
            "2026-11-23T02:00:00Z",
        ));
        let err = inputs.check().expect_err("ids must be unique");
        assert!(
            matches!(err, PlanError::DuplicateAccount { ref id } if id == "AC"),
            "{err}"
        );
    }

    #[test]
    fn a_weekly_reset_that_has_passed_means_the_week_is_over() {
        let mut inputs = inputs();
        inputs.accounts[0].week_percent = 90.0;
        inputs.accounts[0].week_resets_at = inputs.now - Duration::hours(3);
        let passed = slack(&inputs.accounts[0], &inputs).expect("PR has rates");

        let mut fresh = inputs.clone();
        fresh.accounts[0].week_percent = 0.0;
        fresh.accounts[0].week_resets_at = inputs.now + Duration::days(7) - Duration::hours(3);
        let zero = slack(&fresh.accounts[0], &fresh).expect("PR has rates");
        assert!((passed - zero).abs() < EPS, "{passed} vs {zero}");
    }

    #[test]
    fn a_reset_inside_the_last_step_is_recorded() {
        let mut inputs = inputs();
        // PR stays far from full, so the reset leaves work unused.
        inputs.accounts[0].rates = Some(Rates { c: 0.5, s: None });
        inputs.accounts[0].week_resets_at =
            inputs.now + Duration::hours(167) + Duration::minutes(55);

        let plan = follow(&inputs, Policy::Lazy);
        let left = plan
            .left_at_reset
            .get("PR")
            .copied()
            .expect("PR's reset in the last step is in the horizon");
        assert!(left > 0.0 && left <= 100.0, "{left}");
    }

    #[test]
    fn slacks_within_a_rounding_noise_are_a_tie() {
        let reset = "2026-11-21T19:00:00Z";
        let mut lb = account("LB", 2.0, None, false, reset);
        lb.week_percent = 1e-9;
        let pr = account("PR", 2.0, None, false, reset);
        let mut inputs = inputs();
        inputs.accounts = vec![lb, pr];

        let lb_slack = slack(&inputs.accounts[0], &inputs).expect("LB has rates");
        let pr_slack = slack(&inputs.accounts[1], &inputs).expect("PR has rates");
        assert!(
            (lb_slack - pr_slack).abs() < 1e-9 && lb_slack > pr_slack,
            "{lb_slack} vs {pr_slack}"
        );
        // The raw slacks prefer PR; within 1e-9 the id rule prefers LB.
        assert_eq!(choose(&inputs, now()), Some("LB".to_string()));
    }
}
