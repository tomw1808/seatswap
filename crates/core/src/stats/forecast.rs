//! The Forecast page (spec §3.4): the two auto-burn plans for the next seven
//! days, what each account is expected to have at its reset, what it would be
//! when switching only when full, the lanes of the plan, and the one sentence
//! that says whether the week is enough. Everything here only reads.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Serialize;

use crate::api::LimitKind;
use crate::burn::decide::Regime;
use crate::burn::plan::{
    follow, followed, next_reset, rates_known, week_percent_at, AccountState, Inputs, Plan, Policy,
};
use crate::burn::{wu_hours, HORIZON_H};
use crate::history::{same_reset, AccountRef, Hosts, Range, Sample, WindowRecord};
use crate::money::{window_money, Valuer};

use super::{
    account_abbr, account_weeks_valued, first_record, join_names, keep_warnings, week_start, Ctx,
    Query, RangeKey, StatsError,
};

/// How many finished weekly windows the low forecast reads.
const LOW_WINDOWS: usize = 8;
/// The history the low forecast prices and reads: ten weeks hold its last
/// eight finished windows and the samples that name their resets.
const LOW_HISTORY_WEEKS: i64 = 10;

/// The Forecast page (spec §3.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Forecast {
    /// Whole weeks from the first record's week start to this week's; 0 for an
    /// empty history.
    pub basis_weeks: u32,
    /// The accounts auto-burn may use whose rates are not known yet.
    pub learning: Vec<String>,
    pub kpis: ForecastKpis,
    /// One per [`Inputs::accounts`](crate::burn::plan::Inputs::accounts), in
    /// that order.
    pub accounts: Vec<ForecastAccount>,
    /// One lane per account, in the same order.
    pub lanes: Vec<Lane>,
    /// The plan's stretches in which no account is usable.
    pub blocked: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    /// The plan's first step.
    pub from: DateTime<Utc>,
    /// The plan's end, [`HORIZON_H`] hours after [`Forecast::from`].
    pub to: DateTime<Utc>,
    pub sentence: String,
}

/// The four numbers on top of the page (spec §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForecastKpis {
    /// The money of the percent each account is planned to leave unused at its
    /// reset. `None` when no account has a plan entry, a price and known rates.
    pub expire_planned: Option<f64>,
    /// The same when switching only when an account is full.
    pub expire_only_when_full: Option<f64>,
    /// The hours the plan has no usable account, in the working hours of the
    /// regime's plan; never converted a second time.
    pub blocked_hours: f64,
    pub switches: u32,
    pub switches_only_when_full: u32,
}

/// One account's part of the forecast (spec §3.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForecastAccount {
    pub id: String,
    /// The account's weekly percent now, as the plan plays it: 0 when its
    /// weekly reset has passed.
    pub now: f64,
    /// The percent the plan expects to have used at the account's reset:
    /// 100 − the plan's unused percent. `None` while the rates are unknown or
    /// the plan has no entry.
    pub expected: Option<f64>,
    /// The low end of the last [`LOW_WINDOWS`] window gains: this week's
    /// percent now plus the second-lowest gain, at most 100 (the plan's rule of
    /// spec §3.4). `None` with fewer than four gains.
    pub low: Option<f64>,
    /// The plan's first step at which the account is full.
    pub full_at: Option<DateTime<Utc>>,
    pub resets_at: DateTime<Utc>,
    /// What switching only when full expects to have used at the reset.
    pub only_when_full_expected: Option<f64>,
    /// The money of the percent the lazy plan leaves unused at the reset, from
    /// the expired money of a window ending at 100 % minus that percent:
    /// `window_money(price, 100 − left_at_reset).expired`. `None` without a
    /// price or without known rates.
    pub only_when_full_expires: Option<f64>,
}

/// One account's row of the plan's timeline (spec §3.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lane {
    pub id: String,
    /// The planned stretches played with this account.
    pub blocks: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    /// The account's weekly resets inside the plan's window.
    pub resets: Vec<DateTime<Utc>>,
}

/// The two plans, the accounts' forecasts, the lanes and the sentence. The
/// `planned` plan is the one auto-burn follows now: eager when the regime says
/// early switching pays, else lazy; the other is the lazy plan, "only when
/// full" (spec §3.4).
pub fn forecast<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    inputs: &Inputs,
    regime: &Regime,
) -> Result<Forecast, StatsError> {
    let planned = followed(inputs, regime);
    let lazy = follow(inputs, Policy::Lazy);
    let from = inputs.now;
    let to = inputs.now + Duration::hours(HORIZON_H);

    // One valuer for the whole page: the price at `now` and the priced account
    // weeks the low forecast reads come from it.
    let valuer_range = Range {
        from: inputs.now - Duration::weeks(LOW_HISTORY_WEEKS),
        to: inputs.now,
    };
    let valuer = Valuer::build(
        ctx.history,
        &valuer_range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
        ctx.warnings,
    )?;

    // The stored account of every input account, in the input order. A plan
    // account that is not stored is an error, not a silently missing row.
    let mut stored = Vec::with_capacity(inputs.accounts.len());
    let mut references = Vec::with_capacity(inputs.accounts.len());
    for state in &inputs.accounts {
        let account = ctx
            .accounts
            .iter()
            .find(|account| account.id == state.id)
            .ok_or_else(|| unknown_account(&state.id))?;
        references.push(AccountRef::of(account));
        stored.push(account);
    }
    let prices: Vec<Option<f64>> = references
        .iter()
        .map(|reference| valuer.price_at(reference, inputs.now).ok())
        .collect();
    // The reset the plan plays each account to: a weekly reset that has already
    // passed means that week is over, so the plan and the page show its next one.
    let resets: Vec<DateTime<Utc>> = inputs
        .accounts
        .iter()
        .map(|state| next_reset(state.week_resets_at, inputs.now))
        .collect();

    let query = Query {
        range: RangeKey::All,
        accounts: inputs
            .accounts
            .iter()
            .map(|state| state.id.clone())
            .collect(),
        host: None,
    };
    // Every finished window of the chosen accounts; the range holds all of them
    // so the last eight are always found, however far apart the resets are.
    let weeks = account_weeks_valued(ctx, &Range::ALL, &query, &valuer)?;
    let per_account: Vec<Vec<&WindowRecord>> = references
        .iter()
        .map(|reference| {
            let mut windows: Vec<&WindowRecord> = weeks
                .iter()
                .filter(|week| &week.account == reference && week.window.end <= inputs.now)
                .map(|week| &week.window)
                .collect();
            windows.sort_by_key(|window| window.end);
            if windows.len() > LOW_WINDOWS {
                windows.drain(..windows.len() - LOW_WINDOWS);
            }
            windows
        })
        .collect();

    // The newest weekly_all sample at or before `end - d` that names a window's
    // reset. `d` is the distance to the next reset, so every cutoff lies before
    // `now`; reading from the earliest cutoff, or ten weeks back so a window
    // without a sample near its cutoff still finds its newest one, holds every
    // sample the low needs.
    let earliest = per_account
        .iter()
        .enumerate()
        .fold(None, |earliest, (index, windows)| {
            windows.iter().fold(earliest, |earliest, window| {
                let cutoff = window.end - (resets[index] - inputs.now);
                Some(earliest.map_or(cutoff, |current: DateTime<Utc>| current.min(cutoff)))
            })
        });
    let samples = match earliest {
        None => Vec::new(),
        Some(cutoff) => keep_warnings(ctx.warnings, || {
            ctx.history.read_claude::<Sample>(
                &Range {
                    from: cutoff.min(inputs.now - Duration::weeks(LOW_HISTORY_WEEKS)),
                    to: inputs.now,
                },
                &Hosts::All,
            )
        })?
        .records
        .into_iter()
        .map(|hosted| hosted.record)
        .collect(),
    };

    let lows: Vec<Option<f64>> = inputs
        .accounts
        .iter()
        .zip(&references)
        .zip(&per_account)
        .zip(&resets)
        .map(|(((state, reference), windows), reset)| {
            low_of(state, *reset, reference, windows, &samples, inputs.now)
        })
        .collect();

    let accounts: Vec<ForecastAccount> = inputs
        .accounts
        .iter()
        .enumerate()
        .map(|(index, state)| {
            let known = rates_known(state);
            let expected = if known {
                planned
                    .left_at_reset
                    .get(&state.id)
                    .map(|left| 100.0 - left)
            } else {
                None
            };
            let only_when_full_expected = if known {
                lazy.left_at_reset.get(&state.id).map(|left| 100.0 - left)
            } else {
                None
            };
            let only_when_full_expires = if known {
                match (prices[index], lazy.left_at_reset.get(&state.id)) {
                    (Some(price), Some(left)) => Some(window_money(price, 100.0 - *left).expired),
                    _ => None,
                }
            } else {
                None
            };
            ForecastAccount {
                id: state.id.clone(),
                now: week_percent_at(state, inputs.now),
                expected,
                low: lows[index],
                full_at: planned.full_at.get(&state.id).copied(),
                resets_at: resets[index],
                only_when_full_expected,
                only_when_full_expires,
            }
        })
        .collect();

    let planned_money = expire_money(&inputs.accounts, &prices, &planned.left_at_reset);
    let lazy_money = expire_money(&inputs.accounts, &prices, &lazy.left_at_reset);
    let kpis = ForecastKpis {
        expire_planned: planned_money,
        expire_only_when_full: lazy_money,
        // The plan on the page: its blocked work in the hours the rhythm makes
        // of it, with the one work-units-to-hours function. A typical hour of
        // 0.0 means no hours can be computed; the regime's rule then says 0.0
        // (see `burn::decide::regime`).
        blocked_hours: wu_hours(planned.blocked_wu, inputs.rhythm.typical_hour()).unwrap_or(0.0),
        switches: planned.switches,
        switches_only_when_full: lazy.switches,
    };

    let lanes: Vec<Lane> = inputs
        .accounts
        .iter()
        .enumerate()
        .map(|(index, state)| Lane {
            id: state.id.clone(),
            blocks: planned
                .segments
                .iter()
                .filter(|segment| segment.account.as_deref() == Some(state.id.as_str()))
                .map(|segment| (segment.from, segment.to))
                .collect(),
            resets: lane_resets(resets[index], from, to),
        })
        .collect();
    let blocked: Vec<(DateTime<Utc>, DateTime<Utc>)> = planned
        .segments
        .iter()
        .filter(|segment| segment.account.is_none())
        .map(|segment| (segment.from, segment.to))
        .collect();

    let learning: Vec<String> = inputs
        .accounts
        .iter()
        .filter(|state| state.may_use && !rates_known(state))
        .map(|state| state.id.clone())
        .collect();
    let learning_abbrs: Vec<String> = inputs
        .accounts
        .iter()
        .zip(&stored)
        .filter(|(state, _)| learning.iter().any(|id| id == &state.id))
        .map(|(_, account)| account_abbr(ctx, &AccountRef::of(account)))
        .collect();
    let sentence = sentence(ctx, &learning_abbrs, &planned, &accounts, to)?;
    let basis_weeks = basis_weeks(ctx, inputs.now)?;

    Ok(Forecast {
        basis_weeks,
        learning,
        kpis,
        accounts,
        lanes,
        blocked,
        from,
        to,
        sentence,
    })
}

fn unknown_account(id: &str) -> StatsError {
    StatsError::Message(format!("the account {id} of the plan is not stored"))
}

/// The money of the percent `left_at_reset` leaves unused per account, as the
/// expired money of a window ending at 100 % minus that percent, at the
/// account's price at `now`. An account without known rates, an entry or a
/// price is left out; `None` when that leaves no account.
fn expire_money(
    states: &[AccountState],
    prices: &[Option<f64>],
    left_at_reset: &std::collections::BTreeMap<String, f64>,
) -> Option<f64> {
    let mut sum = 0.0;
    let mut any = false;
    for (state, price) in states.iter().zip(prices) {
        if !rates_known(state) {
            continue;
        }
        let (Some(left), Some(price)) = (left_at_reset.get(&state.id), price) else {
            continue;
        };
        sum += window_money(*price, 100.0 - *left).expired;
        any = true;
    }
    any.then_some(sum)
}

/// The lowest next percent of a window gain (spec §3.4): the gain of each of
/// the last [`LOW_WINDOWS`] finished windows is its final percent minus the
/// percent of the newest `weekly_all` sample at or before `end - d` that names
/// the window's reset, where `d` is the time from now to the reset the plan
/// plays the account to. A window without such a sample is left out; with
/// fewer than four gains there is no low. The value is the percent the plan
/// plays this week (0 after a passed reset) plus the second-lowest gain, at most
/// 100 (the plan's rule of spec §3.4).
fn low_of(
    state: &AccountState,
    reset: DateTime<Utc>,
    reference: &AccountRef,
    windows: &[&WindowRecord],
    samples: &[Sample],
    now: DateTime<Utc>,
) -> Option<f64> {
    let d = reset - now;
    let mut gains = Vec::new();
    for window in windows {
        let cutoff = window.end - d;
        let mut newest: Option<(DateTime<Utc>, f64)> = None;
        for sample in samples {
            if &sample.account != reference || sample.t > cutoff {
                continue;
            }
            let Some(limit) = sample.limit(&LimitKind::WeeklyAll) else {
                continue;
            };
            let Some(reset) = limit.r else {
                continue;
            };
            if !same_reset(reset, window.end) {
                continue;
            }
            if newest.is_none_or(|(at, _)| sample.t >= at) {
                newest = Some((sample.t, limit.p));
            }
        }
        if let Some((_, percent)) = newest {
            gains.push(window.final_percent - percent);
        }
    }
    if gains.len() < 4 {
        return None;
    }
    gains.sort_by(f64::total_cmp);
    Some((week_percent_at(state, now) + gains[1]).min(100.0))
}

/// The account's weekly resets inside `[from, to)`: the reset the plan plays it
/// to, repeated every seven days.
fn lane_resets(reset: DateTime<Utc>, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let week = Duration::weeks(1);
    let mut at = reset;
    let mut resets = Vec::new();
    while at < to {
        if at >= from {
            resets.push(at);
        }
        at += week;
    }
    resets
}

/// Whole weeks from the week start of the history's first record to the week
/// start of `now`; 0 for an empty history.
fn basis_weeks<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, now: DateTime<Utc>) -> Result<u32, StatsError> {
    let Some(first) = first_record(ctx)? else {
        return Ok(0);
    };
    let weeks = (week_start(now, &ctx.zone) - week_start(first, &ctx.zone)).num_weeks();
    u32::try_from(weeks).map_err(|_| {
        StatsError::Message(format!(
            "the first record {first} lies after the forecast's now {now}"
        ))
    })
}

/// The forecast's sentence (spec §3.4): the learning prefix while any account
/// auto-burn may use has no rates, then "Enough for the week." or the first
/// blocked stretch and what ends it: the next account's weekly reset when the
/// stretch ends there, else that account becoming usable again.
fn sentence<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    learning_abbrs: &[String],
    planned: &Plan,
    accounts: &[ForecastAccount],
    to: DateTime<Utc>,
) -> Result<String, StatsError> {
    let mut sentence = String::new();
    if !learning_abbrs.is_empty() {
        sentence.push_str(&format!(
            "Still learning how fast {} is used. ",
            join_names(learning_abbrs)
        ));
    }

    let Some(index) = planned
        .segments
        .iter()
        .position(|segment| segment.account.is_none())
    else {
        sentence.push_str("Enough for the week.");
        return Ok(sentence);
    };
    let blocked = &planned.segments[index];
    let start = clock_text(blocked.from, &ctx.zone);
    if blocked.to >= to {
        sentence.push_str(&format!(
            "All accounts full from {start} to the end of the forecast."
        ));
        return Ok(sentence);
    }
    let next = planned.segments.get(index + 1).ok_or_else(|| {
        StatsError::Message(format!(
            "the plan is blocked from {} without a segment after it",
            blocked.from
        ))
    })?;
    let id = next.account.as_deref().ok_or_else(|| {
        StatsError::Message("the plan has two blocked segments in a row".to_string())
    })?;
    let account = ctx
        .accounts
        .iter()
        .find(|account| account.id == id)
        .ok_or_else(|| unknown_account(id))?;
    let row = accounts
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| unknown_account(id))?;
    let end = clock_text(blocked.to, &ctx.zone);
    let abbr = account_abbr(ctx, &AccountRef::of(account));
    if same_reset(blocked.to, row.resets_at) {
        sentence.push_str(&format!(
            "All accounts full from {start} until {abbr} resets {end}."
        ));
    } else {
        sentence.push_str(&format!(
            "All accounts full from {start} until {abbr} is usable again {end}."
        ));
    }
    Ok(sentence)
}

/// A local time of the page as `%a %H:%M`: `Sat 16:00`.
fn clock_text<Tz: TimeZone + Copy>(t: DateTime<Utc>, zone: &Tz) -> String {
    t.with_timezone(zone)
        .naive_local()
        .format("%a %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::burn::decide::RegimeWhy;
    use crate::burn::plan::testkit::inputs;
    use crate::burn::{wu_hours, Rates, HORIZON_H};
    use crate::history::{LimitSample, Source};
    use crate::store::Account;

    use crate::stats::tests::{append_codex_records, tiny, utc, Tiny};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.001
    }

    /// The stored accounts the plan's ids name: PR, AC and LB. The fixture's
    /// own accounts stay as they are.
    fn stored_accounts(tiny: &Tiny) -> Vec<Account> {
        let mut pr = tiny.lb().clone();
        pr.id = "PR".to_string();
        pr.label = "PR".to_string();
        pr.abbr = "PR".to_string();
        pr.account_uuid = "0a1b2c3d-4e5f-4a6b-8c9d-0e1f2a3b4c5d".to_string();
        pr.organization_uuid = "1b2c3d4e-5f6a-4b7c-9d0e-1f2a3b4c5d6e".to_string();
        pr.price = Some(100.0);
        pr.price_currency = Some("EUR".to_string());

        let mut ac = tiny.ac().clone();
        ac.id = "AC".to_string();
        ac.label = "AC".to_string();
        ac.abbr = "AC".to_string();

        let mut lb = tiny.lb().clone();
        lb.id = "LB".to_string();
        lb.label = "LB".to_string();
        lb.abbr = "LB".to_string();

        vec![pr, ac, lb]
    }

    fn context<'a>(tiny: &'a Tiny, accounts: &'a [Account]) -> Ctx<'a> {
        Ctx {
            accounts,
            ..tiny.ctx()
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
            lazy_blocked_h: 3.5,
            eager_blocked_h: 1.25,
            extra_switches: 0,
            cost_h: None,
        }
    }

    /// A weekly window of PR and the sample that names its reset, `gain`
    /// before its final percent.
    fn low_records(
        reference: &AccountRef,
        end: DateTime<Utc>,
        gain: f64,
    ) -> (WindowRecord, Sample) {
        let now = utc("2026-11-16T08:00:00Z");
        let reset = utc("2026-11-21T19:00:00Z");
        let window = WindowRecord {
            account: reference.clone(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(end - Duration::days(7)),
            end,
            final_percent: 100.0,
            lost: Some(0.0),
            last_sample: end - Duration::minutes(2),
            plan: Some("Max".to_string()),
            uncertain: false,
            reset: false,
            from: None,
        };
        let sample = Sample {
            t: end - (reset - now),
            account: reference.clone(),
            src: Source::Poll,
            l: vec![LimitSample {
                k: LimitKind::WeeklyAll,
                m: None,
                p: 100.0 - gain,
                r: Some(end),
                b: false,
            }],
        };
        (window, sample)
    }

    #[test]
    fn lanes_follow_the_plan() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let inputs = inputs();
        let planned = follow(&inputs, Policy::Eager);
        let lazy = follow(&inputs, Policy::Lazy);
        let forecast = forecast(&ctx, &inputs, &regime(true)).expect("the forecast");

        assert_eq!(forecast.lanes.len(), inputs.accounts.len());
        let ids: Vec<&str> = forecast.lanes.iter().map(|lane| lane.id.as_str()).collect();
        let expected: Vec<&str> = inputs
            .accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect();
        assert_eq!(ids, expected);

        let mut union: Vec<(Option<String>, DateTime<Utc>, DateTime<Utc>)> = forecast
            .lanes
            .iter()
            .flat_map(|lane| {
                lane.blocks
                    .iter()
                    .map(move |(from, to)| (Some(lane.id.clone()), *from, *to))
            })
            .collect();
        let mut segments: Vec<(Option<String>, DateTime<Utc>, DateTime<Utc>)> = planned
            .segments
            .iter()
            .filter(|segment| segment.account.is_some())
            .map(|segment| (segment.account.clone(), segment.from, segment.to))
            .collect();
        union.sort();
        segments.sort();
        assert_eq!(union, segments);

        let blocked: Vec<(DateTime<Utc>, DateTime<Utc>)> = planned
            .segments
            .iter()
            .filter(|segment| segment.account.is_none())
            .map(|segment| (segment.from, segment.to))
            .collect();
        assert_eq!(forecast.blocked, blocked);
        assert_eq!(forecast.from, inputs.now);
        assert_eq!(forecast.to, inputs.now + Duration::hours(HORIZON_H));
        let blocked_hours = wu_hours(planned.blocked_wu, inputs.rhythm.typical_hour())
            .expect("the test rhythm has working hours");
        assert_eq!(forecast.kpis.blocked_hours, blocked_hours);
        assert_eq!(forecast.kpis.switches, planned.switches);
        assert_eq!(forecast.kpis.switches_only_when_full, lazy.switches);
    }

    #[test]
    fn expected_is_100_minus_what_is_left() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        inputs.active = Some("AC".to_string());
        for state in &mut inputs.accounts {
            state.may_use = false;
            state.week_percent = 0.0;
        }
        let ac = inputs
            .accounts
            .iter_mut()
            .find(|state| state.id == "AC")
            .expect("AC");
        ac.may_use = true;
        ac.session = None;
        ac.week_percent = 20.0;
        ac.week_resets_at = utc("2026-11-23T02:00:00Z");
        // 92 work units lie between Monday 08:00 and the reset on Monday
        // 02:00; at 1.0 work unit per working hour this uses 60 more percent.
        ac.rates = Some(Rates {
            c: 60.0 / 92.0,
            s: None,
        });

        let forecast = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let account = forecast
            .accounts
            .iter()
            .find(|account| account.id == "AC")
            .expect("AC's row");
        assert!(close(account.expected.expect("AC's expectation"), 80.0));
        let lazy = follow(&inputs, Policy::Lazy);
        let left = *lazy.left_at_reset.get("AC").expect("AC's lazy entry");
        assert!(close(left, 20.0), "left {left}");
        assert!(close(
            account.only_when_full_expires.expect("AC's money"),
            window_money(140.0, 100.0 - left).expired
        ));
    }

    #[test]
    fn an_account_without_rates_has_no_expectation() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        inputs.accounts[2].rates = None;

        let forecast = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let lb = forecast
            .accounts
            .iter()
            .find(|account| account.id == "LB")
            .expect("LB's row");
        assert_eq!(lb.expected, None);
        assert_eq!(lb.only_when_full_expected, None);
        assert_eq!(forecast.learning, vec!["LB".to_string()]);
        assert!(
            forecast
                .sentence
                .starts_with("Still learning how fast LB is used. "),
            "{}",
            forecast.sentence
        );
    }

    #[test]
    fn low_needs_four_windows() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let pr = AccountRef::of(&accounts[0]);
        let ends = [
            utc("2026-11-14T19:00:00Z"),
            utc("2026-11-07T19:00:00Z"),
            utc("2026-10-31T19:00:00Z"),
            utc("2026-10-24T19:00:00Z"),
            utc("2026-10-17T19:00:00Z"),
            utc("2026-10-10T19:00:00Z"),
            utc("2026-10-03T19:00:00Z"),
            utc("2026-09-26T19:00:00Z"),
        ];
        let gains = [10.0, 12.0, 15.0, 20.0, 20.0, 25.0, 30.0, 40.0];
        let (windows, samples): (Vec<WindowRecord>, Vec<Sample>) = ends[..3]
            .iter()
            .zip(gains)
            .map(|(end, gain)| low_records(&pr, *end, gain))
            .unzip();
        tiny.history.append_many(&windows).expect("three windows");
        tiny.history.append_many(&samples).expect("three samples");

        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        inputs.accounts[0].week_percent = 40.0;
        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let row = page
            .accounts
            .iter()
            .find(|account| account.id == "PR")
            .expect("PR's row");
        assert_eq!(row.low, None, "three windows are too few");

        let (windows, samples): (Vec<WindowRecord>, Vec<Sample>) = ends[3..]
            .iter()
            .zip(gains[3..].iter().copied())
            .map(|(end, gain)| low_records(&pr, *end, gain))
            .unzip();
        tiny.history.append_many(&windows).expect("five windows");
        tiny.history.append_many(&samples).expect("five samples");
        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let row = page
            .accounts
            .iter()
            .find(|account| account.id == "PR")
            .expect("PR's row");
        assert!(close(row.low.expect("eight gains"), 52.0), "{:?}", row.low);
    }

    #[test]
    fn the_sentence_names_the_gap() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        inputs.active = Some("AC".to_string());
        for state in &mut inputs.accounts {
            state.may_use = false;
            state.week_percent = 0.0;
        }
        let ac = inputs
            .accounts
            .iter_mut()
            .find(|state| state.id == "AC")
            .expect("AC");
        ac.may_use = true;
        ac.session = None;
        ac.week_resets_at = utc("2026-11-23T02:00:00Z");
        // 89.75 work units lie between Monday 08:00 and Saturday 15:45, so at
        // this rate AC fills in the step that ends at Saturday 16:00.
        ac.rates = Some(Rates { c: 1.113, s: None });

        let forecast = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        assert_eq!(
            forecast.sentence,
            "All accounts full from Sat 16:00 until AC resets Mon 02:00."
        );
    }

    #[test]
    fn an_unknown_account_is_an_error() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        inputs.accounts[0].id = "XX".to_string();

        match forecast(&ctx, &inputs, &regime(false)) {
            Err(StatsError::Message(text)) => assert!(text.contains("XX"), "{text}"),
            other => panic!("expected an error naming XX, got {other:?}"),
        }
    }

    #[test]
    fn a_gap_to_the_end_of_the_forecast() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        let to = inputs.now + Duration::hours(HORIZON_H);
        for state in &mut inputs.accounts {
            state.week_percent = 100.0;
            state.week_resets_at = to + Duration::days(1);
        }

        let forecast = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        assert_eq!(
            forecast.sentence,
            "All accounts full from Mon 08:00 to the end of the forecast."
        );
        assert_eq!(forecast.blocked, vec![(forecast.from, forecast.to)]);
    }

    // ---- proof tests of the review ----

    /// The KPI "Blocked" is the page's own plan, not the cached regime's:
    /// the plan's blocked work in the hours the rhythm makes of it.
    #[test]
    fn blocked_hours_come_from_the_planned_work() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let inputs = inputs();
        let planned = follow(&inputs, Policy::Lazy);
        let expected = wu_hours(planned.blocked_wu, inputs.rhythm.typical_hour())
            .expect("the test rhythm has working hours");
        assert!(planned.blocked_wu > 0.0, "the plan must have blocked work");

        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        assert!(
            close(page.kpis.blocked_hours, expected),
            "the page says {} blocked hours, the plan {}",
            page.kpis.blocked_hours,
            expected
        );
    }

    /// An account whose weekly reset has passed is played from 0 %: the page's
    /// `now` and the base of its low are 0, not the old week's percent.
    #[test]
    fn a_passed_reset_is_played_from_zero() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let pr = AccountRef::of(&accounts[0]);
        let ctx = context(&tiny, &accounts);

        let ends = [
            utc("2026-11-14T19:00:00Z"),
            utc("2026-11-07T19:00:00Z"),
            utc("2026-10-31T19:00:00Z"),
            utc("2026-10-24T19:00:00Z"),
        ];
        let gains = [10.0, 12.0, 15.0, 20.0];
        let (windows, samples): (Vec<WindowRecord>, Vec<Sample>) = ends
            .iter()
            .zip(gains)
            .map(|(end, gain)| low_records(&pr, *end, gain))
            .unzip();
        tiny.history.append_many(&windows).expect("four windows");
        tiny.history.append_many(&samples).expect("the samples");

        let mut inputs = inputs();
        inputs.accounts[0].week_percent = 40.0;
        // PR's weekly reset of 14 November has passed, so the plan plays it
        // from 0 % to its next reset on 21 November.
        inputs.accounts[0].week_resets_at = utc("2026-11-14T19:00:00Z");
        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let row = page
            .accounts
            .iter()
            .find(|account| account.id == "PR")
            .expect("PR's row");
        assert_eq!(row.now, 0.0, "the old week's percent is played");
        assert_eq!(
            row.low,
            Some(12.0),
            "the low starts at the percent the plan plays, not the old week's"
        );
    }

    /// The plan plays an account whose weekly reset has passed to its *next*
    /// reset (spec §2.4: "a weekly reset at or before `Inputs::now` means that
    /// week is over: the account is played from 0 % with its next reset"). The
    /// low forecast must measure the same stretch before the window's end in
    /// both cases. Today `d` is the distance to the *passed* reset, so the
    /// cutoff lies after the window's end and the gain is taken from the last
    /// sample of the window instead of from the same point before its end.
    #[test]
    fn the_low_measures_the_same_stretch_when_the_reset_has_passed() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let pr = AccountRef::of(&accounts[0]);
        let ctx = context(&tiny, &accounts);

        let ends = [
            utc("2026-11-14T19:00:00Z"),
            utc("2026-11-07T19:00:00Z"),
            utc("2026-10-31T19:00:00Z"),
            utc("2026-10-24T19:00:00Z"),
        ];
        let gains = [10.0, 12.0, 15.0, 20.0];
        let mut windows = Vec::new();
        let mut samples = Vec::new();
        for (end, gain) in ends.iter().zip(gains) {
            let (window, sample) = low_records(&pr, *end, gain);
            windows.push(window);
            // A second sample inside the window: the account was nearly full an
            // hour before the end.
            let mut newer = sample.clone();
            newer.t = *end - Duration::hours(1);
            newer.l[0].p = 99.0;
            samples.push(sample);
            samples.push(newer);
        }
        tiny.history.append_many(&windows).expect("four windows");
        tiny.history.append_many(&samples).expect("the samples");

        let mut inputs = inputs();
        // The base is 0 in both cases, so the comparison is about the stretch
        // the low measures, not about a passed week's percent.
        inputs.accounts[0].week_percent = 0.0;
        // PR's weekly reset: Saturday 21 November 19:00, 131 hours from now.
        inputs.accounts[0].week_resets_at = utc("2026-11-21T19:00:00Z");
        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let future = page
            .accounts
            .iter()
            .find(|account| account.id == "PR")
            .expect("PR's row")
            .low;

        // The same weekly reset, one week earlier: it has passed, so the plan
        // plays PR to the reset of 21 November all the same.
        inputs.accounts[0].week_resets_at = utc("2026-11-14T19:00:00Z");
        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let passed = page
            .accounts
            .iter()
            .find(|account| account.id == "PR")
            .expect("PR's row")
            .low;

        assert_eq!(future, Some(12.0), "the reset of 21 November");
        assert_eq!(
            passed, future,
            "a reset that has passed measures another stretch"
        );
    }

    /// The row's reset and the lane's reset marks must name the same reset of
    /// the account: the one the plan plays it to. A reset that has passed means
    /// the week is over, so both show its next reset.
    #[test]
    fn the_row_and_the_lane_name_the_same_reset() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        // PR's reset has passed: the plan plays it to the reset of 21 November.
        inputs.accounts[0].week_resets_at = utc("2026-11-14T19:00:00Z");

        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        let row = page
            .accounts
            .iter()
            .find(|account| account.id == "PR")
            .expect("PR's row");
        let lane = page
            .lanes
            .iter()
            .find(|lane| lane.id == "PR")
            .expect("PR's lane");
        assert!(
            row.resets_at >= inputs.now,
            "the row shows a reset in the past: {:?}",
            row.resets_at
        );
        assert_eq!(
            lane.resets.first(),
            Some(&row.resets_at),
            "the lane marks another reset than the row"
        );
    }

    /// The sentence says what ends the gap: a weekly reset only when the gap
    /// ends at the reset the page shows for that account. When the 5 h window
    /// reopens instead, it says the account is usable again.
    #[test]
    fn the_sentence_names_the_reset_the_page_shows() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let mut inputs = inputs();
        let now = inputs.now;
        inputs.active = Some("AC".to_string());
        for state in &mut inputs.accounts {
            state.may_use = false;
            state.week_percent = 0.0;
        }
        let ac = inputs
            .accounts
            .iter_mut()
            .find(|state| state.id == "AC")
            .expect("AC");
        ac.may_use = true;
        // AC's 5 h window is full until 11:00; its week resets on Monday at
        // 02:00.
        ac.session = Some(crate::burn::plan::SessionState {
            percent: 100.0,
            resets_at: Some(now + Duration::hours(3)),
        });

        let page = forecast(&ctx, &inputs, &regime(false)).expect("the forecast");
        assert!(page
            .sentence
            .ends_with("until AC is usable again Mon 11:00."));
    }

    /// The planned plan is the eager one when early switching pays. The Task 5
    /// setup (`inputs()`: PR uncapped, AC and LB capped, `week(15)`) switches
    /// more often eagerly than lazily.
    #[test]
    fn the_planned_plan_is_eager_when_early_switching_pays() {
        let tiny = tiny();
        let accounts = stored_accounts(&tiny);
        let ctx = context(&tiny, &accounts);
        let inputs = inputs();
        let eager = follow(&inputs, Policy::Eager);
        let lazy = follow(&inputs, Policy::Lazy);
        assert!(
            eager.switches > lazy.switches,
            "{} eager switches, {} lazy",
            eager.switches,
            lazy.switches
        );

        let early = regime(true);
        let page = forecast(&ctx, &inputs, &early).expect("the forecast");
        assert_eq!(page.kpis.switches, eager.switches);
        assert_eq!(page.kpis.switches_only_when_full, lazy.switches);
        let blocked_hours = wu_hours(eager.blocked_wu, inputs.rhythm.typical_hour())
            .expect("the test rhythm has working hours");
        assert_eq!(page.kpis.blocked_hours, blocked_hours);
    }

    // ---- proof tests of the review ----

    /// The Forecast page counts the weeks between the history's first record
    /// and now. A provider's reading is not a Claude record, so it must not be
    /// the first one and must not count a week.
    #[test]
    fn a_provider_record_does_not_count_the_forecast_basis_weeks() {
        let without = tiny();
        let with = tiny();
        // A provider's reading older than the fixture's first record.
        append_codex_records(&with.history, utc("2026-10-20T09:00:00Z"));
        let accounts = stored_accounts(&without);
        let inputs = inputs();
        let regime = regime(false);

        let before =
            forecast(&context(&without, &accounts), &inputs, &regime).expect("the forecast");
        let after = forecast(&context(&with, &accounts), &inputs, &regime).expect("the forecast");

        assert_eq!(
            after.basis_weeks, before.basis_weeks,
            "a Codex record counted the forecast's basis weeks"
        );
    }
}
