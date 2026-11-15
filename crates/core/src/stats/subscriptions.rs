//! The Subscriptions page and its "what if" (spec §3.3): the periods each
//! account's subscription ran, one row per subscription with its verdict, and
//! the replay of the range without one account. Everything here only reads.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;

use crate::burn::live;
use crate::burn::plan::Policy;
use crate::burn::replay::{replay, ReplayResult};
use crate::burn::wu_hours;
use crate::history::{AccountRef, Event, EventKind, Hosts, Range, Sample, TokenRecord};
use crate::live_sessions::Values;
use crate::money::{window_money, Valuer};

use super::{
    account_abbr, account_chosen, account_id, account_weeks_valued, accounts_without_price,
    add_value, blocked_by_week, blocked_spans, hours_text, lost_percent, money_of, next_week_start,
    output_by_account_week, range_of, stored_account, week_columns, week_of_record, windows_span,
    working_rhythm, AccountWeek, Ctx, Query, StatsError,
};

/// One period of one account's subscription: from its `AccountAdded` (or its
/// first record) to its `AccountRemoved`, split at every plan and price change.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    pub id: String,
    pub plan: Option<String>,
    /// The monthly price at [`Period::from`], in the display currency; `None`
    /// when the account has no price there.
    pub price: Option<f64>,
    pub from: DateTime<Utc>,
    /// `None` while the subscription runs.
    pub to: Option<DateTime<Utc>>,
}

/// The one-line verdict of a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    MostlyUnused,
    RegularlyFull,
    WellUsed,
    TooEarly,
}

/// One account's subscriptions of the range.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubRow {
    pub id: String,
    /// The plan and price of the account's latest period that overlaps the
    /// range; without one, what the store or its latest window record says.
    pub plan: Option<String>,
    pub price: Option<f64>,
    /// The start of the account's first period of the whole history, not only
    /// of the periods the range shows; a stored account without any period
    /// starts at its `added_at`.
    pub since: DateTime<Utc>,
    pub weeks: u32,
    /// The mean final percent of its account weeks; `None` without a week.
    pub avg_used: Option<f64>,
    /// The mean expired percent of its account weeks; `None` without a week.
    pub expired_percent: Option<f64>,
    /// The expired money of its priced account weeks; `None` when none of them
    /// has a price.
    pub expired: Option<f64>,
    /// The weeks whose final percent is at least 100.
    pub full_weeks: u32,
    /// The working hours blocked while it was the account in use.
    pub blocked_hours_on_it: f64,
    /// The two values of its records in its finished weekly windows, built
    /// with [`Values::of`]; the running week's records count in neither this
    /// value nor the paid money beside it.
    pub values: Values,
    /// The API value over the paid money of the same finished weeks; `None`
    /// with `times_paid_missing` when either is missing, a week has no price,
    /// or the API value is not in the settings' currency.
    pub times_paid: Option<f64>,
    /// Why `times_paid` is `None`.
    pub times_paid_missing: Option<String>,
    pub verdict: Verdict,
    /// The output tokens per final percent of each account week, oldest first.
    pub tokens_per_percent: Vec<(DateTime<Utc>, f64)>,
}

/// The Subscriptions page: the period bars and the rows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionsPage {
    pub periods: Vec<Period>,
    pub rows: Vec<SubRow>,
    /// The accounts with at least one week left out of the money sums: stored
    /// accounts in [`Ctx::accounts`] order, then accounts no longer stored by
    /// id.
    pub without_price: Vec<String>,
}

/// What dropping one account would have done to the range.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhatIf {
    pub without: String,
    /// The hours blocked by the replays, never below [`WhatIf::blocked_hours_was`];
    /// `None` while there is not enough history to replay.
    pub blocked_hours: Option<f64>,
    /// The recorded blocked hours of the range.
    pub blocked_hours_was: f64,
    /// The money expired under the replay without the account; `None` while
    /// there is not enough history or no account week of the range has a price.
    pub expired: Option<f64>,
    /// The recorded expired money of the range; `None` when no account week of
    /// the range has a price.
    pub expired_was: Option<f64>,
    /// The paid money of the account's weeks of the range; `None` when one of
    /// them has no price.
    pub saved_price: Option<f64>,
    pub sentence: String,
    /// Per calendar week: the recorded hours and the hours without the account,
    /// never below the recorded hours.
    pub weekly: Vec<(DateTime<Utc>, f64, f64)>,
}

/// The sentence of a range too young to replay.
const NOT_ENOUGH: &str = "Not enough history yet to replay this.";

/// The Subscriptions page of the range the query names.
pub fn subscriptions<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
) -> Result<SubscriptionsPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    subscriptions_in(ctx, &range, q)
}

/// The Subscriptions page of `range`; the page's [`subscriptions`] takes the
/// range from its [`Query`], the tests name one.
pub(crate) fn subscriptions_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
) -> Result<SubscriptionsPage, StatsError> {
    let valuer = build_valuer(ctx, range)?;
    let weeks = account_weeks_valued(ctx, range, q, &valuer)?;
    let spans = blocked_spans(ctx, range, q)?;
    let histories = account_histories(ctx)?;
    // A ratio divides each window by its own span, which can reach past the
    // range; the reads that serve it cover every chosen window whole.
    let week_span = windows_span(&weeks, range);
    let tokens = ctx.history.read::<TokenRecord>(&week_span, &Hosts::All)?;

    let mut all_periods: BTreeMap<AccountRef, Vec<Period>> = BTreeMap::new();
    for (reference, history) in &histories {
        if !account_chosen(ctx, q, Some(reference)) {
            continue;
        }
        let periods = periods_of(ctx, reference, history, &valuer)?;
        all_periods.insert(reference.clone(), periods);
    }

    // The period bars list the periods that overlap the range, while a row's
    // `since` comes from the account's first period of the whole history.
    let mut visible_periods: BTreeMap<AccountRef, Vec<Period>> = BTreeMap::new();
    for (reference, account_periods) in &all_periods {
        visible_periods.insert(
            reference.clone(),
            account_periods
                .iter()
                .filter(|period| period_overlaps(period, range))
                .cloned()
                .collect(),
        );
    }

    // The period bars: the stored accounts in the store's order, then the
    // accounts that are no longer stored, each account's periods oldest first.
    let mut periods = Vec::new();
    for account in ctx.accounts {
        let reference = AccountRef::of(account);
        if let Some(account_periods) = visible_periods.get(&reference) {
            periods.extend(account_periods.iter().cloned());
        }
    }
    for (reference, account_periods) in &visible_periods {
        if stored_account(ctx, reference).is_some() {
            continue;
        }
        periods.extend(account_periods.iter().cloned());
    }

    // The chosen records in the account's finished windows, each valued once:
    // their values per account and the output tokens of the window their time
    // falls in. The running week counts in neither.
    let mut values_by_account: BTreeMap<AccountRef, Values> = BTreeMap::new();
    let mut record_refs: Vec<(AccountRef, &TokenRecord)> = Vec::new();
    for hosted in &tokens.records {
        if q.host.as_ref().is_some_and(|host| host != &hosted.host) {
            continue;
        }
        let value = valuer.value(&hosted.host, &hosted.record);
        if !account_chosen(ctx, q, value.account.as_ref()) {
            continue;
        }
        let Some(account) = value.account.clone() else {
            continue;
        };
        if week_of_record(&weeks, &account, hosted.record.t).is_none() {
            continue;
        }
        let record_values = Values::of(std::slice::from_ref(&value), ctx.settings);
        add_value(&mut values_by_account, account.clone(), &record_values);
        record_refs.push((account, &hosted.record));
    }
    let output_by_week = output_by_account_week(&weeks, record_refs.iter().map(|(a, r)| (a, *r)));

    let mut weeks_of_account: BTreeMap<AccountRef, Vec<&AccountWeek>> = BTreeMap::new();
    for week in &weeks {
        weeks_of_account
            .entry(week.account.clone())
            .or_default()
            .push(week);
    }
    let no_weeks: Vec<&AccountWeek> = Vec::new();

    let mut rows = Vec::new();
    for account in ctx.accounts {
        let reference = AccountRef::of(account);
        if !account_chosen(ctx, q, Some(&reference)) {
            continue;
        }
        rows.push(row_for(RowFacts {
            id: account.id.clone(),
            reference: reference.clone(),
            weeks: weeks_of_account
                .get(&reference)
                .unwrap_or(&no_weeks)
                .clone(),
            periods: visible_periods
                .get(&reference)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            blocked_hours_on_it: blocked_hours_on(&spans, &reference),
            values: values_of(&reference, &values_by_account),
            currency: ctx.settings.currency.as_deref(),
            output: &output_by_week,
            since: all_periods
                .get(&reference)
                .and_then(|periods| periods.first())
                .map(|period| period.from)
                .unwrap_or(account.added_at),
            fallback_plan: account.plan.clone(),
            fallback_price: valuer.price_at(&reference, range.to).ok(),
        })?);
    }

    // An account that is no longer stored keeps its row when it has an account
    // week in the range: after the stored accounts, with the id `account_id`
    // gives it.
    let mut removed: Vec<AccountRef> = weeks_of_account
        .keys()
        .filter(|reference| stored_account(ctx, reference).is_none())
        .cloned()
        .collect();
    removed.sort();
    for reference in removed {
        let account_periods = all_periods
            .get(&reference)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let since = account_periods
            .first()
            .map(|period| period.from)
            .ok_or_else(|| {
                StatsError::Message(format!(
                    "the account {} is no longer stored and has no period",
                    account_id(ctx, &reference)
                ))
            })?;
        rows.push(row_for(RowFacts {
            id: account_id(ctx, &reference),
            reference: reference.clone(),
            weeks: weeks_of_account
                .get(&reference)
                .unwrap_or(&no_weeks)
                .clone(),
            periods: visible_periods
                .get(&reference)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            blocked_hours_on_it: blocked_hours_on(&spans, &reference),
            values: values_of(&reference, &values_by_account),
            currency: ctx.settings.currency.as_deref(),
            output: &output_by_week,
            since,
            fallback_plan: histories
                .get(&reference)
                .and_then(|history| history.latest_window_plan()),
            fallback_price: valuer.price_at(&reference, range.to).ok(),
        })?);
    }

    Ok(SubscriptionsPage {
        periods,
        rows,
        without_price: accounts_without_price(ctx, &weeks),
    })
}

/// The "what if" of the range the query names: the range replayed without the
/// account whose id is `without`.
pub fn what_if<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
    without: &str,
) -> Result<WhatIf, StatsError> {
    let range = range_of(q.range, ctx)?;
    what_if_in(ctx, &range, q, without)
}

/// The "what if" of `range`; the page's [`what_if`] takes the range from its
/// [`Query`], the tests name one.
pub(crate) fn what_if_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
    without: &str,
) -> Result<WhatIf, StatsError> {
    let histories = account_histories(ctx)?;
    let mut candidates: Vec<AccountRef> = ctx.accounts.iter().map(AccountRef::of).collect();
    candidates.extend(histories.keys().cloned());
    candidates.sort();
    candidates.dedup();
    let without_ref = candidates
        .into_iter()
        .find(|reference| account_id(ctx, reference) == without)
        .ok_or_else(|| StatsError::Message(format!("unknown account {without}")))?;

    let valuer = build_valuer(ctx, range)?;
    let weeks = account_weeks_valued(ctx, range, q, &valuer)?;
    let spans = blocked_spans(ctx, range, q)?;
    let rhythm = working_rhythm(ctx, range)?;
    let recorded_weeks = blocked_by_week(&spans, &rhythm, &ctx.zone);
    // The recorded hours of every calendar week, computed once: the same values
    // serve the replay and the "not enough history" path.
    let was_by_week: Vec<(DateTime<Utc>, f64)> = week_columns(range, &ctx.zone)
        .into_iter()
        .map(|week| (week, recorded_weeks.get(&week).copied().unwrap_or(0.0)))
        .collect();
    let blocked_hours_was = was_by_week.iter().fold(0.0, |sum, (_, was)| sum + was);

    let mut expired_was = None;
    for week in &weeks {
        if let Some(money) = money_of(week) {
            *expired_was.get_or_insert(0.0) += money.expired;
        }
    }
    // The account's own weeks, whatever the query chose: what dropping it would
    // have saved is its price, not the page's account filter.
    let without_query = Query {
        range: q.range,
        accounts: vec![without.to_string()],
        host: None,
    };
    let without_weeks = account_weeks_valued(ctx, range, &without_query, &valuer)?;
    let saved_price = saved_price(&without_weeks, &without_ref)?;

    let learned = live::learn(
        ctx.history,
        range.to,
        ctx.zone,
        ctx.accounts,
        ctx.settings.read_sessions,
    )?;
    let samples = ctx.history.read::<Sample>(range, &Hosts::All)?;
    let sampled: BTreeSet<AccountRef> = samples
        .records
        .iter()
        .map(|hosted| hosted.record.account.clone())
        .collect();
    let not_enough = learned.rates.is_empty()
        || sampled
            .iter()
            .any(|account| account != &without_ref && !learned.rates.contains_key(account));
    // A sample of an account the store no longer holds cannot be learned: the
    // replay would have no rate for it. It is only left out when it is the
    // account being dropped; then it needs no sentence.
    let refusal = match sampled
        .iter()
        .find(|account| *account != &without_ref && stored_account(ctx, account).is_none())
    {
        Some(account) => Some(format!(
            "{} is no longer stored, so this range cannot be replayed.",
            account_abbr(ctx, account)
        )),
        None if not_enough => Some(NOT_ENOUGH.to_string()),
        None => None,
    };
    if let Some(sentence) = refusal {
        return Ok(WhatIf {
            without: without.to_string(),
            blocked_hours: None,
            blocked_hours_was,
            expired: None,
            expired_was,
            saved_price,
            sentence,
            weekly: Vec::new(),
        });
    }

    // Every calendar week of the range, cut to the range: the replay with every
    // account and the replay without the one. The extra hours of a week are the
    // replay's blocked work difference in hours of the typical working hour.
    let mut weekly = Vec::new();
    let mut expired_with = 0.0;
    let mut expired_without = 0.0;
    for (week, was) in &was_by_week {
        let from = (*week).max(range.from);
        let to = next_week_start(*week, &ctx.zone).min(range.to);
        if from >= to {
            continue;
        }
        let week_range = Range { from, to };
        let with = replay(ctx.history, &week_range, &learned.rates, &[], Policy::Lazy)
            .map_err(|error| StatsError::Message(error.to_string()))?;
        let without_replay = replay(
            ctx.history,
            &week_range,
            &learned.rates,
            std::slice::from_ref(&without_ref),
            Policy::Lazy,
        )
        .map_err(|error| StatsError::Message(error.to_string()))?;
        // A week without recorded work has no typical hour and no blocked
        // work: it adds no hour.
        let extra =
            wu_hours(without_replay.blocked_wu - with.blocked_wu, with.typical_wu).unwrap_or(0.0);
        // The plan's rule: dropping an account never reports fewer blocked hours
        // than were recorded.
        let without_hours = (*was + extra).max(*was);
        weekly.push((*week, *was, without_hours));
        expired_with += replay_expired_money(&valuer, &with, range.to);
        expired_without += replay_expired_money(&valuer, &without_replay, range.to);
    }

    let blocked_hours = weekly.iter().fold(0.0, |sum, (_, _, hours)| sum + hours);
    let expired = expired_was.map(|was| was + expired_without - expired_with);
    let abbr = account_abbr(ctx, &without_ref);
    let more = hours_text(blocked_hours - blocked_hours_was);
    let sentence = if more == "0 h" {
        format!("Dropping {abbr} would have cost no blocked hour in this range.")
    } else {
        format!("Without {abbr} you would have been blocked {more} more.")
    };

    Ok(WhatIf {
        without: without.to_string(),
        blocked_hours: Some(blocked_hours),
        blocked_hours_was,
        expired,
        expired_was,
        saved_price,
        sentence,
        weekly,
    })
}

fn build_valuer<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, range: &Range) -> Result<Valuer, StatsError> {
    Ok(Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
    )?)
}

/// The output tokens of each account week, keyed by the account and the week.
type OutputByWeek = BTreeMap<(AccountRef, DateTime<Utc>), u64>;

/// What the page needs to build one row.
struct RowFacts<'a> {
    id: String,
    reference: AccountRef,
    weeks: Vec<&'a AccountWeek>,
    periods: &'a [Period],
    blocked_hours_on_it: f64,
    values: Values,
    currency: Option<&'a str>,
    output: &'a OutputByWeek,
    since: DateTime<Utc>,
    fallback_plan: Option<String>,
    fallback_price: Option<f64>,
}

fn row_for(facts: RowFacts<'_>) -> Result<SubRow, StatsError> {
    let RowFacts {
        id,
        reference,
        weeks,
        periods,
        blocked_hours_on_it,
        values,
        currency,
        output,
        since,
        fallback_plan,
        fallback_price,
    } = facts;

    let mut used_sum = 0.0;
    let mut lost_sum = 0.0;
    let mut expired = None;
    let mut paid = Some(0.0);
    let mut full_weeks = 0;
    for week in &weeks {
        used_sum += week.window.final_percent;
        lost_sum += lost_percent(&week.window)?;
        if week.window.final_percent >= 100.0 {
            full_weeks += 1;
        }
        match money_of(week) {
            Some(money) => {
                *expired.get_or_insert(0.0) += money.expired;
                if let Some(paid) = &mut paid {
                    *paid += money.paid;
                }
            }
            None => paid = None,
        }
    }
    let weeks_count = weeks.len() as u32;
    let avg_used = if weeks_count > 0 {
        Some(used_sum / f64::from(weeks_count))
    } else {
        None
    };
    let expired_percent = if weeks_count > 0 {
        Some(lost_sum / f64::from(weeks_count))
    } else {
        None
    };
    let (times_paid, times_paid_missing) = times_paid_of(&values, paid, currency, weeks_count);

    let mut tokens_per_percent: Vec<(DateTime<Utc>, f64)> = Vec::new();
    for week in &weeks {
        let percent = week.window.final_percent;
        if percent <= 0.0 {
            continue;
        }
        let out = output
            .get(&(reference.clone(), week.week))
            .copied()
            .unwrap_or(0);
        tokens_per_percent.push((week.week, out as f64 / percent));
    }
    tokens_per_percent.sort_by_key(|(week, _)| *week);

    let (plan, price) = match periods.last() {
        Some(period) => (period.plan.clone(), period.price),
        None => (fallback_plan, fallback_price),
    };
    let verdict = verdict(weeks_count, avg_used, full_weeks);

    Ok(SubRow {
        id,
        plan,
        price,
        since,
        weeks: weeks_count,
        avg_used,
        expired_percent,
        expired,
        full_weeks,
        blocked_hours_on_it,
        values,
        times_paid,
        times_paid_missing,
        verdict,
        tokens_per_percent,
    })
}

/// The verdict of a row, in the order the plan gives it.
fn verdict(weeks: u32, avg_used: Option<f64>, full_weeks: u32) -> Verdict {
    if weeks < 4 {
        Verdict::TooEarly
    } else if avg_used.is_some_and(|avg| avg < 40.0) {
        Verdict::MostlyUnused
    } else if full_weeks * 2 >= weeks {
        Verdict::RegularlyFull
    } else {
        Verdict::WellUsed
    }
}

/// The values of `reference`'s records in its finished windows; an account
/// without one has both halves missing with [`Values::sum`]'s empty reason.
fn values_of(reference: &AccountRef, values: &BTreeMap<AccountRef, Values>) -> Values {
    match values.get(reference) {
        Some(values) => values.clone(),
        None => Values::sum(&[]),
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
    let Some(api) = values.api else {
        return (None, values.api_missing.clone());
    };
    let Some(api_currency) = values.api_currency.as_deref() else {
        return (
            None,
            values
                .api_missing
                .clone()
                .or_else(|| Some("the API value has no currency".to_string())),
        );
    };
    if Some(api_currency) != currency {
        return (
            None,
            Some(format!(
                "the API value is in {api_currency}, not in the settings' currency {}",
                currency.unwrap_or("(none set)")
            )),
        );
    }
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

/// The working hours blocked while `reference` was the account in use.
fn blocked_hours_on(spans: &[super::BlockedSpan], reference: &AccountRef) -> f64 {
    spans
        .iter()
        .filter(|span| &span.account == reference)
        .map(|span| span.hours)
        .sum()
}

/// The paid money of the account's weeks of the range; `None` when one of them
/// has no price.
fn saved_price(weeks: &[AccountWeek], reference: &AccountRef) -> Result<Option<f64>, StatsError> {
    let mut paid = Some(0.0);
    for week in weeks.iter().filter(|week| &week.account == reference) {
        match money_of(week) {
            Some(money) => {
                if let Some(paid) = &mut paid {
                    *paid += money.paid;
                }
            }
            None => paid = None,
        }
    }
    Ok(paid)
}

/// The expired money of one replay at the range's end: each account's expired
/// week-percent as the expired part of a window at its price there; an account
/// without a price is left out.
fn replay_expired_money(valuer: &Valuer, result: &ReplayResult, at: DateTime<Utc>) -> f64 {
    result
        .expired
        .iter()
        .filter_map(|(account, expired)| {
            valuer
                .price_at(account, at)
                .ok()
                .map(|price| window_money(price, 100.0 - *expired).expired)
        })
        .sum()
}

/// Everything the history says about one account's existence and changes.
#[derive(Debug, Default)]
struct AccountHistory {
    /// The earliest sample, window or event that names the account.
    first_record: Option<DateTime<Utc>>,
    added: Vec<DateTime<Utc>>,
    removed: Vec<DateTime<Utc>>,
    /// Every `PlanChanged` and `PriceChanged` time.
    changes: Vec<DateTime<Utc>>,
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

/// The events, samples and windows of the whole history, per account, for the
/// periods and their plans.
fn account_histories<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
) -> Result<BTreeMap<AccountRef, AccountHistory>, StatsError> {
    let events = ctx.history.read::<Event>(&Range::ALL, &Hosts::All)?;
    let samples = ctx.history.read::<Sample>(&Range::ALL, &Hosts::All)?;
    let windows = ctx.history.finished_windows(&Range::ALL, &Hosts::All)?;

    let mut histories: BTreeMap<AccountRef, AccountHistory> = BTreeMap::new();
    for hosted in &windows {
        let record = &hosted.record;
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
    for hosted in &samples.records {
        histories
            .entry(hosted.record.account.clone())
            .or_default()
            .see(hosted.record.t);
    }
    for hosted in &events.records {
        let at = hosted.record.t;
        for account in event_accounts(&hosted.record.kind) {
            let entry = histories.entry(account.clone()).or_default();
            entry.see(at);
            match &hosted.record.kind {
                EventKind::AccountAdded { .. } => entry.added.push(at),
                EventKind::AccountRemoved { .. } => entry.removed.push(at),
                EventKind::PlanChanged { from, to, .. } => {
                    entry.changes.push(at);
                    entry.plan_changes.push((at, from.clone(), to.clone()));
                }
                EventKind::PriceChanged { .. } => entry.changes.push(at),
                _ => {}
            }
        }
    }
    for history in histories.values_mut() {
        history.added.sort_unstable();
        history.added.dedup();
        history.removed.sort_unstable();
        history.removed.dedup();
        history.changes.sort_unstable();
        history.changes.dedup();
        history.plan_changes.sort_by_key(|(at, _, _)| *at);
    }
    Ok(histories)
}

/// The accounts an event names.
fn event_accounts(kind: &EventKind) -> Vec<&AccountRef> {
    match kind {
        EventKind::Switch { from, to, .. } => from.iter().chain(std::iter::once(to)).collect(),
        EventKind::Blocked { account, .. }
        | EventKind::Unblocked { account, .. }
        | EventKind::AccountAdded { account, .. }
        | EventKind::AccountRemoved { account }
        | EventKind::PlanChanged { account, .. }
        | EventKind::PriceChanged { account, .. }
        | EventKind::LoginEnded { account }
        | EventKind::LoginRenewed { account } => vec![account],
        EventKind::Live { account } => account.iter().collect(),
        _ => Vec::new(),
    }
}

/// Every period of one account, over the whole history: the page lists the
/// ones that overlap the range, while a row's `since` comes from the first.
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
        for (from, to) in split_at_changes(history, start, end) {
            if from >= to.unwrap_or(DateTime::<Utc>::MAX_UTC) {
                continue;
            }
            periods.push(Period {
                id: id.clone(),
                plan: plan_at(ctx, reference, from, history),
                price: valuer.price_at(reference, from).ok(),
                from,
                to,
            });
        }
    }
    periods.sort_by_key(|period| period.from);
    Ok(periods)
}

/// Whether `period` overlaps `range`; a period that still runs has no end.
fn period_overlaps(period: &Period, range: &Range) -> bool {
    period.from < range.to && period.to.unwrap_or(DateTime::<Utc>::MAX_UTC) > range.from
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

/// A stretch split at every plan and price change inside it.
fn split_at_changes(
    history: &AccountHistory,
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
) -> Vec<(DateTime<Utc>, Option<DateTime<Utc>>)> {
    let mut cuts: Vec<DateTime<Utc>> = history
        .changes
        .iter()
        .copied()
        .filter(|at| *at > start && end.is_none_or(|end| *at < end))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut parts = Vec::new();
    let mut from = start;
    for cut in cuts {
        parts.push((from, Some(cut)));
        from = cut;
    }
    parts.push((from, end));
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
    match stored_account(ctx, reference) {
        Some(account) => account.plan.clone(),
        None => history.latest_window_plan(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Duration;
    use tempfile::tempdir;

    use crate::api::LimitKind;
    use crate::history::{LimitSample, Source, WindowRecord};
    use crate::stats::tests::{all, tiny, utc, Tiny};
    use crate::stats::{Ctx, Range};

    /// The plan's range: 2 Nov .. 16 Nov.
    fn two_weeks() -> Range {
        Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.001
    }

    fn row<'a>(page: &'a SubscriptionsPage, id: &str) -> &'a SubRow {
        page.rows
            .iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("the row of {id}"))
    }

    /// A sample that names a weekly window and its reset.
    fn sample(at: &str, account: &AccountRef, percent: f64, resets_at: DateTime<Utc>) -> Sample {
        Sample {
            t: utc(at),
            account: account.clone(),
            src: Source::Poll,
            l: vec![LimitSample {
                k: LimitKind::WeeklyAll,
                m: None,
                p: percent,
                r: Some(resets_at),
                b: true,
            }],
        }
    }

    /// One token record of an hour, output tokens only.
    fn record(at: &str, out: u64) -> TokenRecord {
        TokenRecord {
            t: utc(at),
            session: "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34".to_string(),
            cwd: "/home/sam/projects/atlas/engine".to_string(),
            project: crate::project::ProjectKey::Repo("github.com/sam/atlas".to_string()),
            model: "claude-opus-5-5".to_string(),
            input: 0,
            out,
            cw: 0,
            cr: 0,
            answers: 1,
            agent_answers: 0,
        }
    }

    /// The samples and records both accounts need to learn a weekly rate before
    /// 16 Nov: LB's pair at 11 Nov 09:00 and 11:00 around 2,000,000 output tokens
    /// in each hour, AC's pair at 13 Nov 09:00 and 11:00 the same.
    fn add_rates(tiny: &Tiny) {
        tiny.history
            .append_many(&[
                sample(
                    "2026-11-11T09:00:00Z",
                    &tiny.lb_ref(),
                    10.0,
                    utc("2026-11-16T17:00:00Z"),
                ),
                sample(
                    "2026-11-11T11:00:00Z",
                    &tiny.lb_ref(),
                    50.0,
                    utc("2026-11-16T17:00:00Z"),
                ),
                sample(
                    "2026-11-13T09:00:00Z",
                    &tiny.ac_ref(),
                    10.0,
                    utc("2026-11-16T02:00:00Z"),
                ),
                sample(
                    "2026-11-13T11:00:00Z",
                    &tiny.ac_ref(),
                    50.0,
                    utc("2026-11-16T02:00:00Z"),
                ),
            ])
            .expect("the samples");
        tiny.history
            .append_many(&[
                record("2026-11-11T09:00:00Z", 2_000_000),
                record("2026-11-11T10:00:00Z", 2_000_000),
                record("2026-11-13T09:00:00Z", 2_000_000),
                record("2026-11-13T10:00:00Z", 2_000_000),
            ])
            .expect("the token records");
    }

    #[test]
    fn the_rows() {
        let tiny = tiny();
        let page = subscriptions_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(page.rows.len(), 2);

        let lb = row(&page, &tiny.lb().id);
        assert_eq!(lb.weeks, 2);
        assert!(
            lb.avg_used.is_some_and(|avg| close(avg, 90.0)),
            "LB used {:?}",
            lb.avg_used
        );
        assert!(
            lb.expired_percent.is_some_and(|share| close(share, 10.0)),
            "LB expired {:?} %",
            lb.expired_percent
        );
        assert!(
            close(lb.expired.expect("LB's windows have a price"), 8.308),
            "LB expired {:?}",
            lb.expired
        );
        assert_eq!(lb.full_weeks, 1);
        assert!(close(lb.blocked_hours_on_it, 2.0));
        assert_eq!(lb.values.api, Some(60.0));
        assert_eq!(lb.values.api_currency.as_deref(), Some("EUR"));
        assert!(
            close(lb.times_paid.expect("LB's value and price"), 0.722),
            "LB times paid {:?}",
            lb.times_paid
        );

        let ac = row(&page, &tiny.ac().id);
        assert_eq!(ac.weeks, 2);
        assert!(
            ac.avg_used.is_some_and(|avg| close(avg, 70.0)),
            "AC used {:?}",
            ac.avg_used
        );
        assert!(
            ac.expired_percent.is_some_and(|share| close(share, 30.0)),
            "AC expired {:?} %",
            ac.expired_percent
        );
        assert!(
            close(ac.expired.expect("AC's windows have a price"), 19.385),
            "AC expired {:?}",
            ac.expired
        );
        assert_eq!(ac.full_weeks, 0);
        assert_eq!(ac.values.api, Some(80.0));
        assert_eq!(ac.values.api_currency.as_deref(), Some("EUR"));
        assert!(
            close(ac.times_paid.expect("AC's value and price"), 1.238),
            "AC times paid {:?}",
            ac.times_paid
        );
    }

    #[test]
    fn two_weeks_are_too_early_for_a_verdict() {
        let tiny = tiny();
        let page = subscriptions_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        assert_eq!(row(&page, &tiny.lb().id).verdict, Verdict::TooEarly);
        assert_eq!(row(&page, &tiny.ac().id).verdict, Verdict::TooEarly);
    }

    /// Six weeks of windows for LB: all at 30 %, three at 100 %, and all at 70 %.
    #[test]
    fn verdicts() {
        let tiny = tiny();
        let cases: [([f64; 6], Verdict); 3] = [
            ([30.0; 6], Verdict::MostlyUnused),
            (
                [100.0, 100.0, 100.0, 70.0, 70.0, 70.0],
                Verdict::RegularlyFull,
            ),
            ([70.0; 6], Verdict::WellUsed),
        ];
        let range = Range {
            from: utc("2026-10-05T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };
        for (finals, expected) in cases {
            let dir = tempdir().expect("a temp history");
            let history = crate::history::HistoryDir::at(dir.path(), "atlas");
            for (index, final_percent) in finals.iter().enumerate() {
                let start = utc("2026-10-05T17:00:00Z") + Duration::weeks(index as i64);
                let end = start + Duration::weeks(1);
                history
                    .append(&WindowRecord {
                        account: tiny.lb_ref(),
                        k: LimitKind::WeeklyAll,
                        m: None,
                        start: Some(start),
                        end,
                        final_percent: *final_percent,
                        lost: Some(100.0 - final_percent),
                        last_sample: end - Duration::minutes(2),
                        plan: Some("Max".to_string()),
                        uncertain: false,
                    })
                    .expect("a window");
            }
            let ctx = Ctx {
                history: &history,
                ..tiny.ctx()
            };
            let page = subscriptions_in(&ctx, &range, &all()).unwrap();
            let lb = row(&page, &tiny.lb().id);
            assert_eq!(lb.weeks, 6);
            assert_eq!(lb.verdict, expected, "the finals {finals:?}");
        }
    }

    #[test]
    fn a_price_change_splits_the_period() {
        let tiny = tiny();
        tiny.history
            .append(&Event {
                t: utc("2026-11-10T00:00:00Z"),
                kind: EventKind::PriceChanged {
                    account: tiny.lb_ref(),
                    amount: Some(200.0),
                    currency: Some("EUR".to_string()),
                },
            })
            .expect("LB's price change");

        let page = subscriptions_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let lb: Vec<&Period> = page
            .periods
            .iter()
            .filter(|period| period.id == tiny.lb().id)
            .collect();
        assert_eq!(lb.len(), 2, "LB's periods: {lb:#?}");
        assert_eq!(lb[0].to, Some(utc("2026-11-10T00:00:00Z")));
        assert_eq!(lb[1].from, utc("2026-11-10T00:00:00Z"));
        assert_eq!(lb[1].price, Some(200.0));
        assert_eq!(lb[1].to, None);
    }

    #[test]
    fn what_if_never_reports_fewer_blocked_hours_than_recorded() {
        let tiny = tiny();
        add_rates(&tiny);

        let what = what_if_in(&tiny.ctx(), &two_weeks(), &all(), &tiny.ac().id).unwrap();

        assert_eq!(what.blocked_hours_was, 2.0);
        assert!(what.blocked_hours.is_some(), "no replays ran");
        assert!(what.blocked_hours.unwrap() >= 2.0);
        assert!(
            close(
                what.saved_price.expect("AC's weeks have a price"),
                2.0 * 140.0 * 12.0 / 52.0
            ),
            "saved price {:?}",
            what.saved_price
        );
    }

    #[test]
    fn what_if_without_enough_history_says_so() {
        let tiny = tiny();

        let what = what_if_in(&tiny.ctx(), &two_weeks(), &all(), &tiny.ac().id).unwrap();

        assert_eq!(what.sentence, "Not enough history yet to replay this.");
        assert_eq!(what.expired, None);
        assert_eq!(what.blocked_hours, None);
        assert!(what.weekly.is_empty());
        assert_eq!(what.blocked_hours_was, 2.0);
        assert!(
            close(
                what.expired_was.expect("the fixture's priced weeks"),
                27.692
            ),
            "expired was {:?}",
            what.expired_was
        );
        assert!(
            close(
                what.saved_price.expect("AC's weeks have a price"),
                2.0 * 140.0 * 12.0 / 52.0
            ),
            "saved price {:?}",
            what.saved_price
        );
    }

    #[test]
    fn an_unknown_account_is_an_error() {
        let tiny = tiny();
        let error = what_if_in(&tiny.ctx(), &two_weeks(), &all(), "ZZ").unwrap_err();

        assert!(
            error.to_string().contains("ZZ"),
            "the error names the account: {error}"
        );
    }

    #[test]
    fn a_removed_account_keeps_its_row() {
        let tiny = tiny();
        let accounts = vec![tiny.accounts[0].clone()];
        let ctx = Ctx {
            accounts: &accounts,
            ..tiny.ctx()
        };

        let page = subscriptions_in(&ctx, &two_weeks(), &all()).unwrap();

        assert_eq!(page.rows.len(), 2, "LB's row and AC's");
        assert_eq!(page.rows[0].id, tiny.lb().id);
        let ac = &page.rows[1];
        assert_eq!(ac.id, tiny.ac().account_uuid);
        assert_eq!(ac.weeks, 2);
        assert!(
            ac.avg_used.is_some_and(|avg| close(avg, 70.0)),
            "AC used {:?}",
            ac.avg_used
        );
    }

    #[test]
    fn a_week_of_extra_blocking_is_counted_once() {
        let tiny = tiny();
        add_rates(&tiny);

        let what = what_if_in(&tiny.ctx(), &two_weeks(), &all(), &tiny.ac().id).unwrap();

        assert_eq!(what.weekly.len(), 2);
        let hours: f64 = what.weekly.iter().map(|(_, _, hours)| *hours).sum();
        assert!(
            close(hours, what.blocked_hours.expect("the replays ran")),
            "the weeks add up to {hours} h, the page says {:?} h",
            what.blocked_hours
        );
    }

    #[test]
    fn an_account_without_a_price_is_named() {
        let mut tiny = tiny();
        let ac_id = tiny.ac().id.clone();
        tiny.accounts[1].price = None;

        let page = subscriptions_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        assert_eq!(page.without_price, vec![ac_id]);
        let ac = row(&page, &tiny.ac().id);
        assert_eq!(ac.expired, None);
        assert_eq!(ac.times_paid, None);
        assert_eq!(
            ac.times_paid_missing.as_deref(),
            Some("one of its finished weeks has no price")
        );
    }

    #[test]
    fn a_removed_account_cannot_be_replayed() {
        let tiny = tiny();
        add_rates(&tiny);
        let accounts = vec![tiny.accounts[0].clone()];
        let ctx = Ctx {
            accounts: &accounts,
            ..tiny.ctx()
        };
        let removed = format!(
            "{} is no longer stored, so this range cannot be replayed.",
            &tiny.ac().account_uuid[..8]
        );

        let what = what_if_in(&ctx, &two_weeks(), &all(), &tiny.lb().id).unwrap();
        assert_eq!(what.sentence, removed);
        assert_eq!(what.blocked_hours, None);
        assert_eq!(what.expired, None);
        assert!(what.weekly.is_empty());
        assert_eq!(what.blocked_hours_was, 2.0);

        // Dropping the removed account itself leaves it out of the replay, so
        // the page needs no "no longer stored" sentence for it.
        let what = what_if_in(&ctx, &two_weeks(), &all(), &tiny.ac().account_uuid).unwrap();
        assert_ne!(what.sentence, removed);
        assert!(
            what.blocked_hours.is_some(),
            "the replay ran: {}",
            what.sentence
        );
    }

    // ---- proof tests of the review ----

    /// The brief says the row's `since` is the start of the account's *first*
    /// period. An account that was added, removed and added again has two
    /// periods, and only the second one lies in the range the page shows; the
    /// row then shows the second period's start, so "since when" changes with
    /// the range the user picks.
    #[test]
    fn the_row_since_is_the_start_of_the_accounts_first_period() {
        let tiny = tiny();
        tiny.history
            .append_many(&[
                Event {
                    t: utc("2026-10-01T00:00:00Z"),
                    kind: EventKind::AccountAdded {
                        account: tiny.lb_ref(),
                        plan: Some("Max".to_string()),
                    },
                },
                Event {
                    t: utc("2026-11-01T00:00:00Z"),
                    kind: EventKind::AccountRemoved {
                        account: tiny.lb_ref(),
                    },
                },
                Event {
                    t: utc("2026-11-02T00:00:00Z"),
                    kind: EventKind::AccountAdded {
                        account: tiny.lb_ref(),
                        plan: Some("Max".to_string()),
                    },
                },
            ])
            .expect("LB's account events");

        let page = subscriptions_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let lb = row(&page, &tiny.lb().id);

        assert_eq!(
            lb.since,
            utc("2026-10-01T00:00:00Z"),
            "the row's since is the start of the account's first period"
        );

        // The same account in a later range: the row must not forget it.
        let later = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };
        let page = subscriptions_in(&tiny.ctx(), &later, &all()).unwrap();
        assert_eq!(
            row(&page, &tiny.lb().id).since,
            utc("2026-10-01T00:00:00Z"),
            "the row's since does not change with the range"
        );
    }

    /// A range without a block has no blocked hours; the page must not show a
    /// negative zero (the Overview page has a test for the same rule). The sum
    /// of an empty list of hours is -0.0 in Rust.
    #[test]
    fn a_range_without_a_block_shows_no_negative_zero() {
        let tiny = tiny();
        // A range in which no account was blocked: no span, no hours.
        let range = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-12T00:00:00Z"),
        };

        let what = what_if_in(&tiny.ctx(), &range, &all(), &tiny.ac().id).unwrap();

        assert!(
            what.blocked_hours_was.is_sign_positive(),
            "blocked hours {}",
            what.blocked_hours_was
        );
    }
    /// An account added later averages only its own weeks: AC from week 2 has
    /// one week, and its mean is that week's 90 %.
    #[test]
    fn an_account_added_later_averages_only_its_own_weeks() {
        let tiny = tiny();
        tiny.history
            .append(&Event {
                t: utc("2026-11-09T00:00:00Z"),
                kind: EventKind::AccountAdded {
                    account: tiny.ac_ref(),
                    plan: Some("Team 5x".to_string()),
                },
            })
            .expect("AC's later addition");
        let range = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };

        let page = subscriptions_in(&tiny.ctx(), &range, &all()).unwrap();
        let ac = row(&page, &tiny.ac().id);

        assert_eq!(ac.weeks, 1);
        assert!(
            ac.avg_used.is_some_and(|avg| close(avg, 90.0)),
            "AC used {:?}",
            ac.avg_used
        );
    }

    /// Without a dollar rate the API value is in USD: the page must show that
    /// currency, and `times_paid` must not divide USD by the EUR paid money.
    #[test]
    fn without_a_rate_times_paid_is_missing_with_its_reason() {
        let mut tiny = tiny();
        tiny.settings.usd_rate = None;
        tiny.fx = crate::money::Fx::default();

        let page = subscriptions_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let lb = row(&page, &tiny.lb().id);

        assert_eq!(lb.values.api, Some(60.0));
        assert_eq!(lb.values.api_currency.as_deref(), Some("USD"));
        assert_eq!(lb.values.currency.as_deref(), Some("EUR"));
        assert_eq!(lb.times_paid, None);
        let reason = lb.times_paid_missing.as_deref().expect("the reason");
        assert!(
            reason.contains("USD") && reason.contains("EUR"),
            "the reason names both currencies: {reason}"
        );
    }

    /// `times_paid` divides the API value of the finished windows by the paid
    /// money of the same windows: work of the running week counts in neither.
    #[test]
    fn times_paid_counts_only_finished_windows() {
        let tiny = tiny();
        tiny.history
            .append(&record("2026-11-19T09:00:00Z", 2_000_000))
            .expect("work of the running week");
        let ctx = tiny.ctx();
        let range = crate::stats::range_of(crate::stats::RangeKey::Days30, &ctx).unwrap();
        let query = Query {
            range: crate::stats::RangeKey::Days30,
            accounts: Vec::new(),
            host: None,
        };

        let page = subscriptions_in(&ctx, &range, &query).unwrap();
        let ac = row(&page, &tiny.ac().id);

        assert_eq!(
            ac.values.api,
            Some(80.0),
            "the running week's work is not in the finished windows"
        );
        assert!(
            close(ac.times_paid.expect("AC's value and price"), 1.238),
            "AC times paid {:?}",
            ac.times_paid
        );
    }

    /// The tokens per percent of a window count the whole window, also the
    /// records after the range's end when the window reaches past it.
    #[test]
    fn tokens_per_percent_count_the_whole_window() {
        let tiny = tiny();
        tiny.history
            .append(&WindowRecord {
                account: tiny.ac_ref(),
                k: LimitKind::WeeklyAll,
                m: None,
                start: Some(utc("2026-11-11T00:00:00Z")),
                end: utc("2026-11-18T00:00:00Z"),
                final_percent: 100.0,
                lost: Some(0.0),
                last_sample: utc("2026-11-17T23:58:00Z"),
                plan: Some("Team 5x".to_string()),
                uncertain: false,
            })
            .expect("AC's window reaching past the range");
        tiny.history
            .append(&record("2026-11-17T10:00:00Z", 2_000_000))
            .expect("the record after the range's end");
        let range = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };

        let page = subscriptions_in(&tiny.ctx(), &range, &all()).unwrap();
        let ac = row(&page, &tiny.ac().id);
        let ratio = ac
            .tokens_per_percent
            .iter()
            .find(|(week, _)| *week == utc("2026-11-09T00:00:00Z"))
            .expect("AC's week of 9 Nov");

        assert!(
            close(ratio.1, 5_000_000.0 / 100.0),
            "AC's 100 % window counts the 3 M of 14 Nov and the 2 M of 17 Nov: {}",
            ratio.1
        );
    }
}
