//! The Subscriptions page's periods and usage rows (spec §3.3, R6 of 1.3): the
//! periods each Claude account's subscription ran, the one rule which period a
//! price typed near a plan change belongs to, and the usage figures of one
//! subscription. The page's figures themselves are in [`super::plans`];
//! everything here only reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Serialize;

use crate::history::{AccountRef, Event, EventKind, Hosted, Range, Sample, WindowRecord};
use crate::live_sessions::Values;
use crate::money::{Company, Missing, Valuer};

use super::plans::{sum_opt, EnteredPrice, UsageRow};
use super::{
    account_chosen, account_id, money_of, AccountWeek, Ctx, MonthValued, Query, StatsError,
};

/// A price typed at most this many minutes before a plan change belongs to the
/// new plan's period (R7 of 1.3, documented rule): the owner enters the new
/// plan's price just before or together with the plan change, and a period of a
/// few minutes with the old plan at the new price would be noise.
pub(crate) const PRICE_BEFORE_PLAN_MIN: i64 = 15;

/// Why a row has no figure yet: it has no finished period of the range.
pub(crate) const NO_FINISHED_WEEK: &str = "no finished week yet in this range";
pub(crate) const NO_FINISHED_MONTH: &str = "no finished month yet in this range";

/// One period of one account's subscription: from its `AccountAdded` (or its
/// first record) to its `AccountRemoved`, split at every plan change and at
/// every price change that changes the price (see [`split_at_changes`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    pub id: String,
    pub plan: Option<String>,
    /// The monthly price of the period, in the display currency: the price at
    /// [`Period::from`], or the first price set in it when the period starts
    /// with a price not known yet; `None` when the account has no price there.
    pub price: Option<f64>,
    pub from: DateTime<Utc>,
    /// `None` while the subscription runs.
    pub to: Option<DateTime<Utc>>,
}

/// The all-time history reads the periods are built from, so a page that also
/// needs the events, samples and windows reads each file once.
pub(crate) struct HistoryReads<'a> {
    pub events: &'a [Hosted<Event>],
    pub samples: &'a [Hosted<Sample>],
    pub windows: &'a [Hosted<WindowRecord>],
}

/// The periods of every chosen Claude account, from reads a caller already has.
pub(crate) fn periods_for_chosen_from<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
    valuer: &Valuer,
    reads: HistoryReads<'_>,
) -> Result<BTreeMap<AccountRef, Vec<Period>>, StatsError> {
    let histories = account_histories_from(reads);
    let mut periods = BTreeMap::new();
    for account in ctx.accounts {
        let reference = AccountRef::of(account);
        if !account_chosen(ctx, q, Some(&reference)) {
            continue;
        }
        if let Some(history) = histories.get(&reference) {
            periods.insert(
                reference.clone(),
                periods_of(ctx, &reference, history, valuer)?,
            );
        }
    }
    Ok(periods)
}

/// The periods of every Claude account no longer stored that has a period
/// overlapping `range` (deviation 5 of M-2's fix): it keeps its usage row and
/// its money, and gets no plan row or figures.
pub(crate) fn removed_periods<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
    valuer: &Valuer,
    reads: HistoryReads<'_>,
) -> Result<BTreeMap<AccountRef, Vec<Period>>, StatsError> {
    let histories = account_histories_from(reads);
    let mut removed = BTreeMap::new();
    for (reference, history) in &histories {
        if super::stored_account(ctx, reference).is_some()
            || Company::of_account(reference) != Company::Anthropic
            || !account_chosen(ctx, q, Some(reference))
        {
            continue;
        }
        let periods = periods_of(ctx, reference, history, valuer)?;
        if periods.iter().any(|period| period_overlaps(period, range)) {
            removed.insert(reference.clone(), periods);
        }
    }
    Ok(removed)
}

/// Whether a period overlaps `range`.
fn period_overlaps(period: &Period, range: &Range) -> bool {
    period.from < range.to && period.to.is_none_or(|to| to > range.from)
}

/// The periods of every chosen Claude account, reading the whole history here.
/// The page uses [`periods_for_chosen_from`] so it can share the reads.
#[cfg(test)]
pub(crate) fn periods_for_chosen<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
    valuer: &Valuer,
) -> Result<BTreeMap<AccountRef, Vec<Period>>, StatsError> {
    let events = crate::stats::keep_warnings(ctx.warnings, || {
        ctx.history
            .read_claude::<Event>(&crate::history::Range::ALL, &crate::history::Hosts::All)
    })?;
    let samples = crate::stats::keep_warnings(ctx.warnings, || {
        ctx.history
            .read_claude::<Sample>(&crate::history::Range::ALL, &crate::history::Hosts::All)
    })?;
    let windows = crate::stats::keep_warnings(ctx.warnings, || {
        ctx.history
            .finished_claude_windows(&crate::history::Range::ALL, &crate::history::Hosts::All)
    })?;
    periods_for_chosen_from(
        ctx,
        q,
        valuer,
        HistoryReads {
            events: &events.records,
            samples: &samples.records,
            windows: &windows.records,
        },
    )
}

/// What one usage row is built from: the period values of a subscription, its
/// weeks or month, and the stored price it may have been entered in.
pub(crate) struct UsageInput<'a> {
    pub id: String,
    pub plan: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub price: Option<f64>,
    pub price_missing: Option<String>,
    pub stored_price: Option<(f64, Option<String>)>,
    pub currency: Option<&'a str>,
    pub weeks: &'a [&'a AccountWeek],
    pub months: &'a [MonthValued],
    pub values: Values,
    /// Whether this subscription counts per month (Ollama) rather than per week.
    pub ollama: bool,
    /// The page's now, for counting only its finished months.
    pub now: DateTime<Utc>,
    /// Its finished weeks or months in the range; `0` means the row has no
    /// figure and says why.
    pub finished_periods: u32,
}

/// One subscription's usage row (decision 9 of M-2): the existing per-period
/// values of the account weeks or Ollama months, and the stored price's own
/// currency when it differs from the display currency.
pub(crate) fn usage_row(input: UsageInput<'_>) -> Result<(UsageRow, Option<f64>), StatsError> {
    let entered = input.stored_price.and_then(|(amount, currency)| {
        if currency.as_deref() == input.currency {
            None
        } else {
            Some(EnteredPrice { amount, currency })
        }
    });
    if input.finished_periods == 0 {
        // No finished week (Ollama: month) of the range: the row has no figure
        // at all, and says why. A share of a period that still runs is not a
        // figure of the range, and a sum over no finished period is not 0
        // (decision 1 of M-2's fix 2).
        let row = UsageRow {
            id: input.id,
            plan: input.plan,
            since: input.since,
            price_month: input.price,
            entered,
            used_percent: None,
            expired: None,
            api: None,
            times_paid: None,
            missing: Some(if input.ollama {
                NO_FINISHED_MONTH.to_string()
            } else {
                NO_FINISHED_WEEK.to_string()
            }),
        };
        return Ok((row, None));
    }

    let used_percent = if input.ollama {
        // A running month's share is not a figure of the range: the row's use
        // counts its finished months only, as its money and API value do
        // (fix 1 of the final review).
        mean(
            input
                .months
                .iter()
                .filter(|month| month.end <= input.now)
                .map(|month| month.used_percent),
        )
    } else {
        mean(input.weeks.iter().map(|week| week.used))
    };

    let (expired, paid) = if input.ollama {
        let expired = sum_opt(input.months.iter().map(|month| month.expired));
        let finished = input
            .months
            .iter()
            .filter(|month| month.end <= input.now)
            .count() as f64;
        let paid = input.price.map(|price| price * finished);
        (expired, paid)
    } else {
        let mut expired = None;
        let mut paid = Some(0.0);
        for week in input.weeks {
            match money_of(week)? {
                Some(money) => {
                    *expired.get_or_insert(0.0) += money.expired;
                    if let Some(paid) = &mut paid {
                        *paid += money.paid;
                    }
                }
                None => paid = None,
            }
        }
        (expired, paid)
    };

    let (api, api_missing) = api_of(&input.values, input.currency);
    let weeks = input.weeks.len() as u32;
    let (times_paid, times_paid_missing) = if input.ollama {
        match (api, paid) {
            (Some(api), Some(paid)) if paid > 0.0 => (Some(api / paid), None),
            (None, _) => (None, api_missing.clone()),
            (Some(_), Some(_)) => (
                None,
                Some("the paid money of its months is zero".to_string()),
            ),
            (Some(_), None) => (None, Some("a month of it has no price".to_string())),
        }
    } else {
        times_paid_of(&input.values, paid, input.currency, weeks)
    };

    let missing = if input.price.is_none() {
        input
            .price_missing
            .or_else(|| Some(Missing::NoAccountPrice.to_string()))
    } else if api.is_none() {
        api_missing
    } else {
        times_paid_missing
    };

    let row = UsageRow {
        id: input.id,
        plan: input.plan,
        since: input.since,
        price_month: input.price,
        entered,
        used_percent,
        expired,
        api,
        times_paid,
        missing,
    };
    Ok((row, paid))
}

/// The mean of a non-empty list; `None` when it is empty.
fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values {
        sum += value;
        count += 1;
    }
    (count > 0).then(|| sum / count as f64)
}

/// The API value in the page's currency, with the reason when it is in another
/// one or has none.
fn api_of(values: &Values, currency: Option<&str>) -> (Option<f64>, Option<String>) {
    let Some(api) = values.api else {
        return (None, values.api_missing.clone());
    };
    match values.api_currency.as_deref() {
        Some(api_currency) if Some(api_currency) == currency => (Some(api), None),
        Some(api_currency) => (
            None,
            Some(format!(
                "the API value is in {api_currency}, not in the settings' currency {}",
                currency.unwrap_or("(none set)")
            )),
        ),
        None => (
            None,
            values
                .api_missing
                .clone()
                .or_else(|| Some("the API value has no currency".to_string())),
        ),
    }
}

/// The API value over the paid money of the same finished weeks, with the
/// reason when it cannot be a ratio in the settings' currency.
fn times_paid_of(
    values: &Values,
    paid: Option<f64>,
    currency: Option<&str>,
    weeks: u32,
) -> (Option<f64>, Option<String>) {
    if weeks == 0 {
        return (
            None,
            Some("no finished weekly window in the range".to_string()),
        );
    }
    let (api, api_missing) = api_of(values, currency);
    let Some(api) = api else {
        return (None, api_missing);
    };
    let Some(paid) = paid else {
        return (
            None,
            Some("one of its finished weeks has no price".to_string()),
        );
    };
    if paid <= 0.0 {
        return (
            None,
            Some("the paid money of its finished weeks is zero".to_string()),
        );
    }
    (Some(api / paid), None)
}

/// Everything the history says about one account's existence and changes.
#[derive(Debug, Default)]
struct AccountHistory {
    /// The earliest sample, window or event that names the account.
    first_record: Option<DateTime<Utc>>,
    added: Vec<DateTime<Utc>>,
    removed: Vec<DateTime<Utc>>,
    /// Every `PriceChanged` time, oldest first.
    price_changes: Vec<DateTime<Utc>>,
    plan_changes: Vec<(DateTime<Utc>, Option<String>, Option<String>)>,
    /// The plan of the account's latest window record, by end.
    latest_window: Option<(DateTime<Utc>, Option<String>)>,
}

impl AccountHistory {
    fn see(&mut self, at: DateTime<Utc>) {
        self.first_record = Some(self.first_record.map_or(at, |first| first.min(at)));
    }

    fn latest_window_plan(&self) -> Option<String> {
        self.latest_window
            .as_ref()
            .and_then(|(_, plan)| plan.clone())
    }
}

/// The events, samples and windows of the given reads, per account, for the
/// periods and their plans.
fn account_histories_from(reads: HistoryReads<'_>) -> BTreeMap<AccountRef, AccountHistory> {
    let mut histories: BTreeMap<AccountRef, AccountHistory> = BTreeMap::new();
    for hosted in reads.windows {
        let record = &hosted.record;
        if Company::of_account(&record.account) != Company::Anthropic {
            continue;
        }
        let entry = histories.entry(record.account.clone()).or_default();
        entry.see(record.weekly_start());
        if entry
            .latest_window
            .as_ref()
            .is_none_or(|(end, _)| record.end > *end)
        {
            entry.latest_window = Some((record.end, record.plan.clone()));
        }
    }
    for hosted in reads.samples {
        if Company::of_account(&hosted.record.account) != Company::Anthropic {
            continue;
        }
        histories
            .entry(hosted.record.account.clone())
            .or_default()
            .see(hosted.record.t);
    }
    for hosted in reads.events {
        let at = hosted.record.t;
        for account in hosted.record.kind.accounts() {
            if Company::of_account(account) != Company::Anthropic {
                continue;
            }
            let entry = histories.entry(account.clone()).or_default();
            entry.see(at);
            match &hosted.record.kind {
                EventKind::AccountAdded { .. } => entry.added.push(at),
                EventKind::AccountRemoved { .. } => entry.removed.push(at),
                EventKind::PlanChanged { from, to, .. } => {
                    entry.plan_changes.push((at, from.clone(), to.clone()));
                }
                EventKind::PriceChanged { .. } => entry.price_changes.push(at),
                _ => {}
            }
        }
    }
    for history in histories.values_mut() {
        history.added.sort_unstable();
        history.added.dedup();
        history.removed.sort_unstable();
        history.removed.dedup();
        history.price_changes.sort_unstable();
        history.price_changes.dedup();
        history.plan_changes.sort_by_key(|(at, _, _)| *at);
    }
    histories
}

/// Every period of one account, over the whole history: a row's `since` comes
/// from the first.
fn periods_of<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    reference: &AccountRef,
    history: &AccountHistory,
    valuer: &Valuer,
) -> Result<Vec<Period>, StatsError> {
    let id = account_id(ctx, reference);
    let mut periods = Vec::new();
    for (start, end) in segments(history) {
        if start >= end.unwrap_or(DateTime::<Utc>::MAX_UTC) {
            continue;
        }
        let mut stretch: Vec<Period> = Vec::new();
        for part in split_at_changes(history, start, end) {
            let period = Period {
                id: id.clone(),
                plan: plan_at(ctx, reference, part.from, history),
                price: valuer.price_at(reference, part.price_at).ok(),
                from: part.from,
                to: part.to,
            };
            // Two adjacent periods with the same plan and price are one
            // period: a price entered again unchanged starts nothing new.
            match stretch.last_mut() {
                Some(last)
                    if last.to == Some(period.from)
                        && last.plan == period.plan
                        && last.price == period.price =>
                {
                    last.to = period.to;
                }
                _ => stretch.push(period),
            }
        }
        periods.extend(stretch);
    }
    periods.sort_by_key(|period| period.from);
    Ok(periods)
}

/// The stretches an account existed: from each `AccountAdded` (or its first
/// record without one) to its `AccountRemoved`; a later `AccountAdded` ends the
/// previous stretch and starts a new one.
fn segments(history: &AccountHistory) -> Vec<(DateTime<Utc>, Option<DateTime<Utc>>)> {
    if history.added.is_empty() {
        let Some(start) = history.first_record else {
            return Vec::new();
        };
        let end = history
            .removed
            .iter()
            .copied()
            .find(|removed| *removed > start);
        return vec![(start, end)];
    }

    let mut segments = Vec::new();
    for (index, start) in history.added.iter().enumerate() {
        let next_added = history.added.get(index + 1).copied();
        let next_removed = history
            .removed
            .iter()
            .copied()
            .find(|removed| removed > start);
        let end = match (next_added, next_removed) {
            (Some(added), Some(removed)) => Some(added.min(removed)),
            (Some(added), None) => Some(added),
            (None, Some(removed)) => Some(removed),
            (None, None) => None,
        };
        segments.push((*start, end));
    }
    segments
}

/// One part of a stretch: its span and the time its price is read at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Part {
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
    price_at: DateTime<Utc>,
}

/// A stretch split at its plan changes and price changes. The stretch's start
/// (an `AccountAdded`, or the first record) and each plan change start a plan
/// whose price is not known yet, unless a price was set at that very moment:
/// the first price set later during that plan is its price from its start —
/// the price was unknown, not different. So a price typed in a few minutes
/// after an account was added or its plan changed makes no period of a few
/// minutes. A price typed at most [`PRICE_BEFORE_PLAN_MIN`] before a plan
/// change belongs to the new plan (R7, documented rule): it makes no part of
/// the old plan at the new price.
fn split_at_changes(
    history: &AccountHistory,
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
) -> Vec<Part> {
    let inside = |at: DateTime<Utc>, from: DateTime<Utc>, to: Option<DateTime<Utc>>| {
        at > from && to.is_none_or(|to| at < to)
    };
    let mut plan_starts: Vec<(DateTime<Utc>, bool)> = vec![(start, false)];
    plan_starts.extend(
        history
            .plan_changes
            .iter()
            .map(|(at, _, _)| (*at, true))
            .filter(|(at, _)| inside(*at, start, end)),
    );
    plan_starts.sort_by_key(|(at, _)| *at);
    plan_starts.dedup_by_key(|(at, _)| *at);

    // The price a plan change takes from just before it: the newest price
    // typed in the [`PRICE_BEFORE_PLAN_MIN`] minutes before the change, but
    // after the previous plan's start.
    let carried: Vec<Option<DateTime<Utc>>> = plan_starts
        .iter()
        .enumerate()
        .map(|(index, (plan_from, is_plan_change))| {
            if index == 0 || !is_plan_change {
                return None;
            }
            let previous = plan_starts[index - 1].0;
            history
                .price_changes
                .iter()
                .copied()
                .filter(|at| {
                    *at > previous
                        && *at < *plan_from
                        && *plan_from - *at <= Duration::minutes(PRICE_BEFORE_PLAN_MIN)
                })
                .max()
        })
        .collect();

    let mut parts = Vec::new();
    for (index, plan_from) in plan_starts.iter().copied().enumerate() {
        let plan_from = plan_from.0;
        let plan_to = plan_starts.get(index + 1).map(|(at, _)| *at).or(end);
        let next_carried = carried.get(index + 1).copied().flatten();
        let prices: Vec<DateTime<Utc>> = history
            .price_changes
            .iter()
            .copied()
            .filter(|at| inside(*at, plan_from, plan_to))
            .filter(|at| Some(*at) != next_carried)
            .collect();
        let mut from = plan_from;
        let mut price_at = carried[index].unwrap_or(plan_from);
        if carried[index].is_none() && !history.price_changes.contains(&plan_from) {
            if let Some(first) = prices.first() {
                price_at = *first;
            }
        }
        for cut in &prices {
            parts.push(Part {
                from,
                to: Some(*cut),
                price_at,
            });
            from = *cut;
            price_at = *cut;
        }
        // The old plan's last part must not read the price the next plan takes:
        // read the price just before that change instead.
        let price_at = match next_carried {
            Some(carried_at) if prices.last().is_none_or(|last| *last < carried_at) => {
                carried_at - Duration::nanoseconds(1)
            }
            _ => price_at,
        };
        parts.push(Part {
            from,
            to: plan_to,
            price_at,
        });
    }
    parts
}

/// The plan at `at`: the last `PlanChanged.to` at or before it; before the first
/// change that change's `from`; without any change the stored account's plan,
/// and for an account no longer stored the plan of its latest window record.
fn plan_at<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    reference: &AccountRef,
    at: DateTime<Utc>,
    history: &AccountHistory,
) -> Option<String> {
    if let Some(first) = history.plan_changes.first() {
        let after = history
            .plan_changes
            .partition_point(|(changed, _, _)| *changed <= at);
        return if after == 0 {
            first.1.clone()
        } else {
            history.plan_changes[after - 1].2.clone()
        };
    }
    match super::stored_account(ctx, reference) {
        Some(account) => account.plan.clone(),
        None => history.latest_window_plan(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::history::{EventKind, Range};
    use crate::money::Valuer;
    use crate::stats::tests::{all, tiny, utc};

    /// The plan's range: 2 Nov .. 16 Nov.
    fn two_weeks() -> Range {
        Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        }
    }

    /// The fixture's history with LB added, three price changes and one plan
    /// change: 20 EUR from the add, 100 EUR typed five minutes before the
    /// plan change, and the plan change ten minutes later.
    fn plan_change_history() -> crate::stats::fixture::Tiny {
        let tiny = tiny();
        tiny.history
            .append(&Event {
                t: utc("2026-11-02T06:33:00Z"),
                kind: EventKind::AccountAdded {
                    account: tiny.lb_ref(),
                    plan: None,
                },
            })
            .expect("the add");
        tiny.history
            .append(&Event {
                t: utc("2026-11-02T06:36:00Z"),
                kind: EventKind::PriceChanged {
                    account: tiny.lb_ref(),
                    amount: Some(20.0),
                    currency: Some("EUR".to_string()),
                },
            })
            .expect("the first price");
        tiny.history
            .append(&Event {
                t: utc("2026-11-09T05:34:00Z"),
                kind: EventKind::PriceChanged {
                    account: tiny.lb_ref(),
                    amount: Some(100.0),
                    currency: Some("EUR".to_string()),
                },
            })
            .expect("the new price");
        tiny.history
            .append(&Event {
                t: utc("2026-11-09T05:39:00Z"),
                kind: EventKind::PlanChanged {
                    account: tiny.lb_ref(),
                    from: Some("Pro".to_string()),
                    to: Some("Max".to_string()),
                },
            })
            .expect("the plan change");
        tiny
    }

    #[test]
    fn a_price_typed_just_before_a_plan_change_belongs_to_the_new_plan() {
        let tiny = plan_change_history();
        let ctx_tiny = tiny.ctx();
        let valuer = Valuer::build(
            ctx_tiny.history,
            &two_weeks(),
            ctx_tiny.prices,
            ctx_tiny.fx,
            ctx_tiny.settings,
            ctx_tiny.accounts,
            ctx_tiny.provider_subscriptions,
            ctx_tiny.warnings,
        )
        .expect("the valuer");
        let periods = periods_for_chosen(&ctx_tiny, &all(), &valuer).expect("the periods");
        let lb = periods.get(&tiny.lb_ref()).expect("LB's periods");
        assert_eq!(lb.len(), 2, "{lb:?}");
        assert_eq!(lb[0].plan.as_deref(), Some("Pro"));
        assert_eq!(lb[0].from, utc("2026-11-02T06:33:00Z"));
        assert_eq!(lb[0].to, Some(utc("2026-11-09T05:39:00Z")));
        assert_eq!(lb[0].price, Some(20.0));
        assert_eq!(lb[1].plan.as_deref(), Some("Max"));
        assert_eq!(lb[1].from, utc("2026-11-09T05:39:00Z"));
        assert_eq!(lb[1].price, Some(100.0));
    }
}
