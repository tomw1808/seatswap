//! The Overview page (spec §3.1): what the chosen range used, paid and let
//! expire, the hours blocked, the switches and what they cost, each week by
//! account and by project, the marks of the range, and the findings. Everything
//! here only reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Serialize;

use crate::burn::cost::switch_cost;
use crate::burn::rates;
use crate::burn::{Rates, RATES_DAYS};
use crate::history::{
    AccountRef, Event, EventKind, Hosted, Hosts, Range, Read, Sample, TokenRecord,
};
use crate::live_sessions::Values;
use crate::money::{Company, Valuer, WindowMoney};
use crate::projects::ProjectId;

use super::{
    account_abbr, account_id, account_position, account_weeks_valued, accounts_without_price,
    add_opt_value, add_value, api_shares, blocked_by_week, blocked_spans, date_text, hours_text,
    keep_warnings, money_of, money_text, offered_money, percent_of_offered, percent_text, range_of,
    running_account_weeks, switch_counts, top_three, week_columns, week_lost_percent,
    week_of_record, week_span, week_start, window_used, working_rhythm, AccountWeek, Ctx, Query,
    StatsError, SwitchCounts,
};

/// The Overview page's numbers for one range (spec §3.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    /// The accounts with at least one week left out of the money sums, in the
    /// order of [`Ctx::accounts`]: a week without a price is left out, while
    /// the account's priced weeks still count. An account no longer stored is
    /// named by its id after the stored ones.
    pub without_price: Vec<String>,
    pub kpis: Kpis,
    pub weeks: Vec<OverviewWeek>,
    pub marks: Vec<Mark>,
    pub findings: Vec<String>,
}

/// The This-week card on top of the Overview page ([`this_week_card`]): each
/// chosen account's weekly window that runs now ([`running_account_weeks`]),
/// the work of every other provider in the current calendar week, and what it
/// all went to. The Overview's history below counts finished windows only.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThisWeek {
    /// One per chosen account with a running weekly window, in the order of
    /// [`Ctx::accounts`], an account no longer stored after them by id; empty
    /// until the first usage reading.
    pub accounts: Vec<ThisWeekAccount>,
    /// One per other company (OpenAI, Ollama) with token records in the
    /// current calendar week, in [`Company`]'s order.
    pub providers: Vec<ThisWeekProvider>,
    /// The API value of every listed account's running window and every
    /// listed provider's week together by project, in percent of their total,
    /// the three largest then `other` ([`api_shares`]).
    pub projects: Vec<(String, f64)>,
}

/// One provider subscription's work on the This-week card: its records from its
/// running window's start (OpenAI: its `weekly_all` week; Ollama: its
/// `monthly_credits` month) to now, exactly as an account row sums from its
/// window's start (decision P-2 6). The window's use, money and reset are the
/// subscription row's, filled like an account row's when it has a price; without
/// one `money_missing` says why while the API value stays. Its records belong to
/// its subscription, so the account chips choose them like an account's
/// (decision P-2 8). A record whose subscription cannot be named is one more row
/// per company, named by the company, with its API value and the reason.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThisWeekProvider {
    /// The subscription's stored abbreviation (`OA`, `OA2`, `OL`), or the
    /// company's name for a record whose subscription cannot be named.
    pub name: String,
    /// The subscription's company: `openai` or `ollama`.
    pub company: Company,
    /// The records' API value, summed with [`Values::sum`]; `None` when a
    /// record's is unknown.
    pub api: Option<f64>,
    /// The currency `api` is in: the settings' currency with a stored rate,
    /// "USD" without one; `None` when `api` is.
    pub api_currency: Option<String>,
    /// Why `api` is `None`; `None` when it has a value.
    pub api_missing: Option<String>,
    /// The API value by project, in percent of `api`, the three largest then
    /// `other` ([`api_shares`]); empty while the value is unknown or 0.
    pub projects: Vec<(String, f64)>,
    /// The running window's use so far, in percent: OpenAI's `weekly_all` week
    /// or Ollama's `monthly_credits` month; `None` while the subscription has
    /// no running window.
    pub used: Option<f64>,
    /// The money of `used` at the subscription's price; `None` without a price
    /// or without a running window.
    pub money: Option<f64>,
    /// Why `money` is `None`, in the words [`ThisWeekAccount::money_missing`]
    /// uses; `None` when it has a value.
    pub money_missing: Option<String>,
    /// The running window's reset; `None` without a running window.
    pub resets_at: Option<DateTime<Utc>>,
}

/// One account's running weekly window on the This-week card.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThisWeekAccount {
    /// The account's id ([`account_id`]).
    pub id: String,
    /// The use so far in percent: the newest reading minus the reading a
    /// reset began the window at (`stats::window_used`).
    pub used: f64,
    /// The newest reading's percent; above `used` when a reset began the
    /// window.
    pub now: f64,
    /// The money of `used` at the window's price; `None` without a price.
    pub money: Option<f64>,
    /// Why `money` is `None`, in the words the week money uses; `None` when
    /// it has a value.
    pub money_missing: Option<String>,
    /// The window's start ([`crate::history::WindowRecord::weekly_start`]).
    pub started_at: DateTime<Utc>,
    /// The window's reset.
    pub resets_at: DateTime<Utc>,
    /// The account's API value from `started_at` to now by project, in
    /// percent of its total, the three largest then `other`
    /// ([`api_shares`]); empty while the value is unknown or 0.
    pub projects: Vec<(String, f64)>,
}

/// The five numbers of the page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Kpis {
    /// Money used of the priced account weeks: `Σ week.used × pv`.
    pub used: f64,
    /// Money paid for the priced account weeks: `100 × pv` each.
    pub paid: f64,
    /// `lost × pv` of the priced account weeks.
    pub expired: f64,
    /// The used part of the offered weeks, in percent: every finished weekly
    /// window offers one week of the account's price, so a week that a reset
    /// split offers each of its windows. `used + expired + still open` make
    /// 100 % of them, except that a window whose use passed 100 % counts its
    /// whole use, so the two shares can then pass 100 % together (nothing is
    /// cut). `None` when no account week has a price.
    pub used_percent: Option<f64>,
    /// The expired part of the offered weeks, in percent: every window's
    /// `lost`, over the same offered windows. `None` without a price.
    pub expired_percent: Option<f64>,
    /// The expired money per account id, largest first.
    pub expired_by_account: Vec<(String, f64)>,
    /// The working hours blocked in the range, one entry per company that has
    /// working hours in it, Anthropic first (decision P-2 5, 6): no sum across
    /// companies, because a provider's block is not an Anthropic one.
    pub blocked_by_company: Vec<BlockedByCompany>,
    /// The hours blocked in the calendar weeks before the last account added
    /// in the range was added; `None` when the range holds no such account.
    pub blocked_before_added: Option<BlockedBefore>,
    /// The part of Claude's blocked hours in which other quota expired.
    pub avoidable_hours: f64,
    pub switches: SwitchCounts,
    /// The five switch counts added up.
    pub switches_total: u32,
    /// What one switch costs the account in use at the range's end, as a
    /// percent of its 5 h window; `None` when the rates or the cost are missing.
    pub switch_cost_percent_of_window: Option<f64>,
    /// The calendar weeks the page shows: the weeks of the range.
    pub weeks: u32,
}

/// One company's working hours blocked in the range (decision P-2 6).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedByCompany {
    /// [`Company`]'s lowercase JSON name: `anthropic`, `openai`, `ollama`.
    pub company: Company,
    /// The company's blocked hours inside its own working hours.
    pub hours: f64,
}

/// The blocked hours of the weeks before an account was added, and the account.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedBefore {
    /// The added account's short name ([`account_abbr`]), as the chart's
    /// marks name it; the account may be one the page's filter leaves out.
    pub name: String,
    /// The blocked hours of the calendar weeks before the week it was added.
    pub hours: f64,
}

/// One calendar week of the range, with the finished account weeks that fall
/// in it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewWeek {
    pub week: DateTime<Utc>,
    pub accounts: Vec<WeekAccount>,
    /// The projects' subscription value, the three largest then `other`.
    pub projects: Vec<(String, f64)>,
    /// The working hours blocked in this week.
    pub blocked_hours: f64,
    /// The expired money of the priced account weeks of this week over the
    /// money their windows offered, in percent: the same rule as
    /// [`Kpis::expired_percent`]. `None` when no account week of the week has
    /// a price.
    pub expired_share: Option<f64>,
}

/// One account's finished week as the Overview shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekAccount {
    pub id: String,
    pub used_percent: f64,
    /// Money used; `None` without a price for the week.
    pub used: Option<f64>,
    pub expired_percent: f64,
    /// Money expired; `None` without a price for the week.
    pub expired: Option<f64>,
    pub uncertain: bool,
}

/// What the page marks on its chart in one calendar week: the accounts added
/// and the auto-burn switches of that week, in one label, at the first of them
/// ([`week_marks`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    pub at: DateTime<Utc>,
    pub label: String,
}

pub fn overview<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, q: &Query) -> Result<Overview, StatsError> {
    let range = range_of(q.range, ctx)?;
    overview_in(ctx, &range, q)
}

/// The Overview of `range`; the page's [`overview`] takes the range from its
/// [`Query`], the tests name one.
pub(crate) fn overview_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
) -> Result<Overview, StatsError> {
    let valuer = Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
        ctx.provider_subscriptions,
        ctx.warnings,
    )?;
    let finished = account_weeks_valued(ctx, range, q, &valuer)?;
    let spans = blocked_spans(ctx, range, q, Company::Anthropic)?;
    let rhythm = working_rhythm(ctx, range, Company::Anthropic)?;
    let currency = ctx.settings.currency.clone();

    let events = keep_warnings(ctx.warnings, || {
        ctx.history.read_claude::<Event>(&Range::ALL, &Hosts::All)
    })?;
    let reading = reading_range(range, &finished);
    let samples = keep_warnings(ctx.warnings, || {
        ctx.history.read_claude::<Sample>(&reading, &Hosts::All)
    })?;
    let tokens = keep_warnings(ctx.warnings, || {
        ctx.history.read::<TokenRecord>(&reading, &Hosts::All)
    })?;

    let without_price = accounts_without_price(ctx, &finished);

    // The page shows every calendar week of the range, with or without an
    // account week; the blocked hours of all of them add up to the KPI.
    let mut weeks_by_start: BTreeMap<DateTime<Utc>, OverviewWeek> = week_columns(range, &ctx.zone)
        .into_iter()
        .map(|week| (week, empty_week(week)))
        .collect();

    // The money of every priced account week; a week without a price is left
    // out of the sums and its account is named in `without_price`.
    let mut used = 0.0;
    let mut paid = 0.0;
    let mut expired = 0.0;
    // The same parts as shares of the offered weeks: every finished weekly
    // window offers one week of its account's price, so a week a reset split
    // offers each of its windows (item 33, R-1).
    let mut offered = 0.0;
    let mut used_part = 0.0;
    let mut expired_part = 0.0;
    let mut priced_weeks: BTreeMap<AccountRef, u32> = BTreeMap::new();
    let mut expired_by_account: BTreeMap<AccountRef, f64> = BTreeMap::new();
    let mut money: BTreeMap<DateTime<Utc>, WindowMoney> = BTreeMap::new();
    let mut offered_by_week: BTreeMap<DateTime<Utc>, f64> = BTreeMap::new();
    for week in &finished {
        let week_money = money_of(week)?;
        if let Some(week_money) = week_money {
            used += week_money.used;
            paid += week_money.paid;
            expired += week_money.expired;
            let week_offered = offered_money(week, &week_money);
            offered += week_offered;
            *offered_by_week.entry(week.week).or_insert(0.0) += week_offered;
            used_part += week_money.used;
            expired_part += week_money.expired;
            *priced_weeks.entry(week.account.clone()).or_insert(0) += 1;
            *expired_by_account
                .entry(week.account.clone())
                .or_insert(0.0) += week_money.expired;
            let week_sum = money.entry(week.week).or_insert(WindowMoney {
                used: 0.0,
                paid: 0.0,
                expired: 0.0,
            });
            week_sum.used += week_money.used;
            week_sum.paid += week_money.paid;
            week_sum.expired += week_money.expired;
        }
        weeks_by_start
            .entry(week.week)
            .or_insert_with(|| empty_week(week.week))
            .accounts
            .push(WeekAccount {
                id: account_id(ctx, &week.account),
                used_percent: week.used,
                used: week_money.map(|money| money.used),
                expired_percent: week_lost_percent(week)?,
                expired: week_money.map(|money| money.expired),
                uncertain: week.window.uncertain,
            });
    }

    let blocked_weeks = blocked_by_week(&spans, &rhythm, &ctx.zone);
    // One entry per company that has working hours in the range, Anthropic
    // first: a company's blocked hours count in its own working hours only,
    // never summed with another's (decision P-2 5, 6).
    let mut blocked_by_company = Vec::new();
    for company in [Company::Anthropic, Company::OpenAI, Company::Ollama] {
        let company_rhythm = working_rhythm(ctx, range, company)?;
        if !company_rhythm.usd.iter().any(|usd| *usd > 0.0) {
            continue;
        }
        let hours = if company == Company::Anthropic {
            // The page's own spans, which its weeks and findings count too.
            spans.iter().fold(0.0, |sum, span| sum + span.hours)
        } else {
            blocked_spans(ctx, range, q, company)?
                .iter()
                .fold(0.0, |sum, span| sum + span.hours)
        };
        blocked_by_company.push(BlockedByCompany { company, hours });
    }
    let switches = switch_counts(&events.records, range, q.host.as_deref());
    let switches_total = switches.total();
    let mut kpis = Kpis {
        used,
        paid,
        expired,
        used_percent: percent_of_offered(used_part, offered),
        expired_percent: percent_of_offered(expired_part, offered),
        expired_by_account: expired_by_account
            .into_iter()
            .map(|(reference, expired)| (account_id(ctx, &reference), expired))
            .collect(),
        blocked_by_company,
        blocked_before_added: blocked_before_added(ctx, range, &events.records, &blocked_weeks),
        avoidable_hours: spans
            .iter()
            .fold(0.0, |sum, span| sum + span.avoidable_hours),
        switches,
        switches_total,
        switch_cost_percent_of_window: switch_cost_percent(ctx, range, &events, &samples, &tokens),
        weeks: weeks_by_start.len() as u32,
    };
    kpis.expired_by_account
        .sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    // The week's account rows follow the stored accounts' order; an account
    // that is no longer stored comes after them.
    for week in weeks_by_start.values_mut() {
        week.accounts.sort_by(|a, b| {
            account_position(ctx, &a.id)
                .cmp(&account_position(ctx, &b.id))
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    for (week, hours) in &blocked_weeks {
        weeks_by_start
            .entry(*week)
            .or_insert_with(|| empty_week(*week))
            .blocked_hours += *hours;
    }

    for (start, week) in weeks_by_start.iter_mut() {
        if let (Some(week_money), Some(offered)) = (money.get(start), offered_by_week.get(start)) {
            week.expired_share = percent_of_offered(week_money.expired, *offered);
        }
    }

    let mut project_sums: BTreeMap<DateTime<Utc>, BTreeMap<String, f64>> = BTreeMap::new();
    for hosted in &tokens.records {
        if q.host.as_ref().is_some_and(|host| host != &hosted.host) {
            continue;
        }
        let value = valuer.value(&hosted.host, &hosted.record);
        let (Some(account), Some(subscription)) = (value.account, value.subscription) else {
            continue;
        };
        let Some(week) = week_of_record(&finished, &account, hosted.record.t) else {
            continue;
        };
        let name = ctx
            .projects
            .of(&ProjectId::of(&hosted.record.project, &hosted.host))
            .name;
        *project_sums
            .entry(week.week)
            .or_default()
            .entry(name)
            .or_insert(0.0) += subscription;
    }
    for (start, sums) in project_sums {
        weeks_by_start
            .entry(start)
            .or_insert_with(|| empty_week(start))
            .projects = top_three(sums.into_iter().collect());
    }

    let marks = week_marks(ctx, range, &events.records);

    let weeks: Vec<OverviewWeek> = weeks_by_start.into_values().collect();
    let mut findings = Vec::new();
    if let Some(text) = finding_expired(ctx, &kpis, &priced_weeks, currency.as_deref()) {
        findings.push(text);
    }
    if let Some(text) = finding_blocked(&kpis, &weeks) {
        findings.push(text);
    }
    if let Some(text) = finding_auto_burn(
        ctx,
        range,
        &events.records,
        &weeks,
        &money,
        &offered_by_week,
    ) {
        findings.push(text);
    }
    if let Some(text) = finding_full(ctx, &finished) {
        findings.push(text);
    }

    Ok(Overview {
        without_price,
        kpis,
        weeks,
        marks,
        findings,
    })
}

/// The This-week card: the chosen accounts' running weekly windows, each with
/// its use so far, its money and the projects its API value went to, from the
/// window's start to now; and the work of every other provider (Codex,
/// Ollama) from Monday 00:00 of the current calendar week to now, valued at
/// API prices. The host filter applies to the token records, as the Weeks
/// summary filters them; the range in `q` does not apply, so a range that
/// holds no record still has the card. A Claude record counts in its
/// account's running window; a record of another provider uses no account's
/// week and is not filtered by the account chips.
pub fn this_week_card<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
) -> Result<ThisWeek, StatsError> {
    // A running weekly window began at most seven days back, and so did this
    // calendar week's Monday.
    let valuer = Valuer::build(
        ctx.history,
        &Range {
            from: ctx.now - Duration::days(7),
            to: ctx.now,
        },
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
        ctx.provider_subscriptions,
        ctx.warnings,
    )?;
    let running = running_account_weeks(ctx, q, &valuer);
    let calendar_week = week_start(ctx.now, &ctx.zone);
    // Every provider subscription the page can show: the stored ones, the
    // OpenAI weeks and Ollama month running now, and (once the records are
    // read) every subscription a record names. `None` means no running window.
    let mut provider_runs: BTreeMap<AccountRef, (Company, Option<ProviderRun>)> = BTreeMap::new();
    for subscription in ctx.provider_subscriptions {
        let company = Company::of_account(&subscription.account);
        if company != Company::Anthropic {
            provider_runs.insert(subscription.account.clone(), (company, None));
        }
    }
    for window in valuer.running_windows(ctx.now) {
        let company = Company::of_account(&window.account);
        if company == Company::Anthropic {
            continue;
        }
        let run = ProviderRun {
            start: window.weekly_start(),
            end: window.end,
            used: window_used(&window),
        };
        provider_runs.insert(window.account.clone(), (company, Some(run)));
    }
    let ollama = crate::money::ollama_subscription();
    if let Some(month) = valuer.running_month(&ollama, ctx.now) {
        provider_runs.insert(
            ollama,
            (
                Company::Ollama,
                Some(ProviderRun {
                    start: month.start,
                    end: month.end,
                    used: Some(month.used),
                }),
            ),
        );
    }
    let from = running
        .iter()
        .map(|week| week.window.weekly_start())
        .chain(
            provider_runs
                .values()
                .filter_map(|(_, run)| run.as_ref().map(|run| run.start)),
        )
        .fold(calendar_week, DateTime::min);
    let tokens = keep_warnings(ctx.warnings, || {
        ctx.history.read::<TokenRecord>(
            &Range {
                from: from.min(ctx.now),
                to: ctx.now,
            },
            &Hosts::All,
        )
    })?;

    // Each account's and each provider subscription's API value and its split
    // by project, the records no subscription can be named for by company, and
    // the same over all of them together.
    let mut by_account: BTreeMap<AccountRef, (Option<Values>, BTreeMap<String, Values>)> =
        BTreeMap::new();
    let mut by_provider: BTreeMap<AccountRef, (Option<Values>, BTreeMap<String, Values>)> =
        BTreeMap::new();
    let mut unassigned: BTreeMap<Company, (Option<Values>, BTreeMap<String, Values>)> =
        BTreeMap::new();
    let mut unassigned_missing: BTreeMap<Company, crate::money::Missing> = BTreeMap::new();
    let mut total: Option<Values> = None;
    let mut by_project: BTreeMap<String, Values> = BTreeMap::new();
    for hosted in &tokens.records {
        if q.host.as_ref().is_some_and(|host| host != &hosted.host) {
            continue;
        }
        let record = &hosted.record;
        if record.t > ctx.now {
            continue;
        }
        let value = valuer.value(&hosted.host, record);
        let own = match value.company {
            Company::Anthropic => {
                let Some(account) = value.account.as_ref() else {
                    continue;
                };
                let Some(week) = running.iter().find(|week| &week.account == account) else {
                    continue;
                };
                if record.t < week.window.weekly_start() {
                    continue;
                }
                by_account.entry(account.clone()).or_default()
            }
            company => match value.account.as_ref() {
                None => {
                    if record.t < calendar_week {
                        continue;
                    }
                    if let Some(missing) = &value.subscription_missing {
                        unassigned_missing
                            .entry(company)
                            .or_insert_with(|| missing.clone());
                    }
                    unassigned.entry(company).or_default()
                }
                Some(account) => {
                    let start = provider_runs
                        .get(account)
                        .and_then(|(_, run)| run.as_ref())
                        .map(|run| run.start)
                        .unwrap_or(calendar_week);
                    if record.t < start {
                        continue;
                    }
                    provider_runs
                        .entry(account.clone())
                        .or_insert((company, None));
                    by_provider.entry(account.clone()).or_default()
                }
            },
        };
        let name = ctx
            .projects
            .of(&ProjectId::of(&record.project, &hosted.host))
            .name;
        let record_values = Values::of(std::slice::from_ref(&value), ctx.settings, ctx.fx, ctx.now);
        add_opt_value(&mut own.0, &record_values);
        add_value(&mut own.1, name.clone(), &record_values);
        add_opt_value(&mut total, &record_values);
        add_value(&mut by_project, name, &record_values);
    }

    let mut accounts = Vec::with_capacity(running.len());
    for week in &running {
        let money = money_of(week)?;
        // An account without a token record in its window has no split yet.
        let projects = match by_account.remove(&week.account) {
            Some((Some(own_total), own_projects)) => api_shares(own_projects, &own_total),
            _ => Vec::new(),
        };
        accounts.push(ThisWeekAccount {
            id: account_id(ctx, &week.account),
            used: week.used,
            now: week.window.final_percent,
            money: money.map(|money| money.used),
            money_missing: week.price_missing.as_ref().map(ToString::to_string),
            started_at: week.window.weekly_start(),
            resets_at: week.window.end,
            projects,
        });
    }
    accounts.sort_by(|a, b| {
        account_position(ctx, &a.id)
            .cmp(&account_position(ctx, &b.id))
            .then_with(|| a.id.cmp(&b.id))
    });

    // The rows in the stored subscriptions' order, then the unnamed companies'.
    let mut providers: Vec<(usize, String, ThisWeekProvider)> =
        Vec::with_capacity(provider_runs.len());
    for (account, (company, run)) in &provider_runs {
        let (own_total, own_projects) = by_provider.remove(account).unwrap_or_default();
        let (used, money, money_missing, resets_at) =
            provider_row(&valuer, *company, account, run.as_ref());
        let name = super::stored_subscription(ctx, account)
            .map(|subscription| subscription.abbr.clone())
            .unwrap_or_else(|| company.name().to_string());
        providers.push((
            account_position(ctx, &account.acc),
            name.clone(),
            ThisWeekProvider {
                name,
                company: *company,
                api: own_total.as_ref().and_then(|total| total.api),
                api_currency: own_total
                    .as_ref()
                    .and_then(|total| total.api_currency.clone()),
                api_missing: own_total
                    .as_ref()
                    .and_then(|total| total.api_missing.clone()),
                projects: match own_total {
                    Some(total) => api_shares(own_projects, &total),
                    None => Vec::new(),
                },
                used,
                money,
                money_missing,
                resets_at,
            },
        ));
    }
    // A record whose subscription cannot be named is one row per company,
    // named by the company, with its API value and the reason (decision P-2 6).
    for (company, (own_total, own_projects)) in unassigned {
        let Some(own_total) = own_total else {
            continue;
        };
        let money_missing = unassigned_missing
            .get(&company)
            .map(ToString::to_string)
            .or_else(|| Some(crate::money::Missing::NoAccount.to_string()));
        let name = company.name().to_string();
        providers.push((
            usize::MAX,
            name.clone(),
            ThisWeekProvider {
                name,
                company,
                api: own_total.api,
                api_currency: own_total.api_currency.clone(),
                api_missing: own_total.api_missing.clone(),
                projects: api_shares(own_projects, &own_total),
                used: None,
                money: None,
                money_missing,
                resets_at: None,
            },
        ));
    }
    providers.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let providers: Vec<ThisWeekProvider> = providers.into_iter().map(|(_, _, row)| row).collect();
    let projects = match total {
        Some(total) => api_shares(by_project, &total),
        None => Vec::new(),
    };
    Ok(ThisWeek {
        accounts,
        providers,
        projects,
    })
}

/// A provider subscription's running window on the This-week card.
struct ProviderRun {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    /// The window's use so far; `None` when the readings cannot compute it.
    used: Option<f64>,
}

/// A provider row's four subscription fields (decision P-2 6, 7): filled like
/// an account row's from the subscription's own running window, and
/// `money_missing` with the reason when it has no price. A subscription with no
/// running window gets its own reason, never four `None` fields without one:
/// OpenAI's missing week, Ollama's missing month, or Ollama's reading without a
/// monthly allowance. Never a 0 for an unknown.
fn provider_row(
    valuer: &Valuer,
    company: Company,
    account: &AccountRef,
    run: Option<&ProviderRun>,
) -> (
    Option<f64>,
    Option<f64>,
    Option<String>,
    Option<DateTime<Utc>>,
) {
    let Some(run) = run else {
        let missing = match company {
            Company::OpenAI | Company::Ollama => valuer.no_reading(account),
            _ => crate::money::Missing::NoWeekReading,
        };
        return (None, None, Some(missing.to_string()), None);
    };
    match valuer.window_price(account, run.start, run.end) {
        Ok(price) => {
            let money = match (company, run.used) {
                // One percent of Ollama's month is a hundredth of the month's
                // price; one percent of a week is a week's share of it.
                (_, None) => None,
                (Company::Ollama, Some(used)) => Some(crate::money::month_money(price, used).used),
                (_, Some(used)) => Some(crate::money::window_money(price, used).used),
            };
            (run.used, money, None, Some(run.end))
        }
        Err(missing) => (run.used, None, Some(missing.to_string()), Some(run.end)),
    }
}

/// The range the token and sample reads cover: the range itself, the 14 days
/// the rates are learned over before its end, and every account week's windows.
fn reading_range(range: &Range, finished: &[AccountWeek]) -> Range {
    let mut span = Range {
        from: range.from.min(range.to - Duration::days(RATES_DAYS)),
        to: range.to,
    };
    for week in finished {
        let week = week_span(week);
        span.from = span.from.min(week.from);
        span.to = span.to.max(week.to);
    }
    span
}

/// The hours blocked in the calendar weeks before the week the last account
/// added in the range was added, and that account's short name. The week it was added
/// is not counted: the page's hours are counted per calendar week, not split at
/// the moment of the addition. `None` when the range holds no `AccountAdded`
/// of a stored account.
fn blocked_before_added<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    events: &[Hosted<Event>],
    blocked_weeks: &BTreeMap<DateTime<Utc>, f64>,
) -> Option<BlockedBefore> {
    let (at, reference) = last_account_added(ctx, range, events)?;
    let added_week = week_start(at, &ctx.zone);
    let hours = blocked_weeks
        .iter()
        .filter(|(week, _)| **week < added_week)
        .map(|(_, hours)| *hours)
        .sum();
    Some(BlockedBefore {
        name: account_abbr(ctx, &reference),
        hours,
    })
}

/// The latest `AccountAdded` event in the range of an account the settings
/// still store; ties go to the larger [`AccountRef`], so the choice does not
/// depend on the order of the history's files.
fn last_account_added<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    events: &[Hosted<Event>],
) -> Option<(DateTime<Utc>, AccountRef)> {
    let mut latest: Option<(DateTime<Utc>, AccountRef)> = None;
    for hosted in events {
        let at = hosted.record.t;
        if at < range.from || at >= range.to {
            continue;
        }
        let EventKind::AccountAdded { account, .. } = &hosted.record.kind else {
            continue;
        };
        if !ctx
            .accounts
            .iter()
            .any(|stored| AccountRef::of(stored) == *account)
        {
            continue;
        }
        let candidate = (at, account.clone());
        if latest.as_ref().is_none_or(|current| candidate > *current) {
            latest = Some(candidate);
        }
    }
    latest
}

/// The chart's marks: one per calendar week that holds an `AccountAdded` or
/// `AutoBurnSet` event of the range, at the week's first such event. The chart
/// puts a mark at its week's column, so two marks of one week would print on
/// top of each other; one mark names them all instead: the accounts added, in
/// time order, as `PR, AC added`, then each auto-burn switch, joined by ` · `.
fn week_marks<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    events: &[Hosted<Event>],
) -> Vec<Mark> {
    let mut chosen: Vec<&Event> = events
        .iter()
        .map(|hosted| &hosted.record)
        .filter(|event| range.from <= event.t && event.t < range.to)
        .filter(|event| {
            matches!(
                event.kind,
                EventKind::AccountAdded { .. } | EventKind::AutoBurnSet { .. }
            )
        })
        .collect();
    chosen.sort_by_key(|event| event.t);
    /// One week's marked events: its first event's time, the accounts added
    /// and the auto-burn switches, each in time order.
    struct WeekEvents {
        at: DateTime<Utc>,
        added: Vec<String>,
        auto_burn: Vec<&'static str>,
    }
    let mut weeks: BTreeMap<DateTime<Utc>, WeekEvents> = BTreeMap::new();
    for event in chosen {
        let week = weeks
            .entry(week_start(event.t, &ctx.zone))
            .or_insert_with(|| WeekEvents {
                at: event.t,
                added: Vec::new(),
                auto_burn: Vec::new(),
            });
        match &event.kind {
            EventKind::AccountAdded { account, .. } => {
                let name = account_abbr(ctx, account);
                if !week.added.contains(&name) {
                    week.added.push(name);
                }
            }
            EventKind::AutoBurnSet { on } => {
                week.auto_burn
                    .push(if *on { "auto-burn on" } else { "auto-burn off" });
            }
            _ => {}
        }
    }
    weeks
        .into_values()
        .map(|week| {
            let mut parts: Vec<String> = Vec::new();
            if !week.added.is_empty() {
                parts.push(format!("{} added", week.added.join(", ")));
            }
            parts.extend(week.auto_burn.into_iter().map(str::to_string));
            Mark {
                at: week.at,
                label: parts.join(" · "),
            }
        })
        .collect()
}

/// A week with no accounts yet.
fn empty_week(week: DateTime<Utc>) -> OverviewWeek {
    OverviewWeek {
        week,
        accounts: Vec::new(),
        projects: Vec::new(),
        blocked_hours: 0.0,
        expired_share: None,
    }
}

/// The measured cost of one switch at the range's end (spec §2.13): the mean
/// over the switches of the range, times `s` of the account in use at the end
/// on this host. The rule is the Settings page's (`cost.usd × s`, the 5 h
/// factor's value).
fn switch_cost_percent<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    events: &Read<Event>,
    samples: &Read<Sample>,
    tokens: &Read<TokenRecord>,
) -> Option<f64> {
    let rate_accounts: Vec<rates::RateAccount> =
        ctx.accounts.iter().map(rates::RateAccount::of).collect();
    let learned: BTreeMap<AccountRef, Rates> = rates::rates(
        &rate_accounts,
        &samples.records,
        &tokens.records,
        &events.records,
        ctx.prices,
        range.to,
    )
    .rates;
    let cost = switch_cost(
        &events.records,
        &samples.records,
        &tokens.records,
        &learned,
        ctx.prices,
        range,
    )?;
    let timelines = rates::timelines(&events.records);
    let active = timelines.get(ctx.history.host())?.at(range.to)?;
    let window = learned.get(active)?.s?.value;
    Some(cost.usd * window)
}

/// The first finding: the account with the most expired money, if above zero;
/// no money text without a currency.
fn finding_expired<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    kpis: &Kpis,
    priced_weeks: &BTreeMap<AccountRef, u32>,
    currency: Option<&str>,
) -> Option<String> {
    let currency = currency?;
    let (id, expired) = kpis.expired_by_account.first()?;
    if *expired <= 0.0 {
        return None;
    }
    let account = ctx.accounts.iter().find(|account| &account.id == id)?;
    let weeks = priced_weeks
        .get(&AccountRef::of(account))
        .copied()
        .unwrap_or(0);
    let noun = if weeks == 1 { "week" } else { "weeks" };
    Some(format!(
        "{} is where most money expired: {} in {weeks} {noun}.",
        account.abbr,
        money_text(*expired, currency)
    ))
}

/// The second finding: the blocked time, over the weeks the page shows. It
/// speaks of Claude's accounts alone: auto-burn and the finding are
/// Anthropic's, and no company's hours are added to another's (decision P-2 5).
fn finding_blocked(kpis: &Kpis, weeks: &[OverviewWeek]) -> Option<String> {
    // No Anthropic entry means no Anthropic blocked hours in the range: there
    // is no finding, not a finding of 0 h.
    let blocked = kpis
        .blocked_by_company
        .iter()
        .find(|entry| entry.company == Company::Anthropic)
        .map(|entry| entry.hours)?;
    if blocked <= 0.0 || weeks.is_empty() {
        return None;
    }
    let noun = if weeks.len() == 1 { "week" } else { "weeks" };
    Some(format!(
        "You were blocked {} in {} {noun}; {} of them while other quota expired.",
        hours_text(blocked),
        weeks.len(),
        hours_text(kpis.avoidable_hours)
    ))
}

/// The third finding: since auto-burn was switched on and stayed on, with a
/// full week on each side. Its expired share needs priced weeks, not a
/// currency of its own.
fn finding_auto_burn<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    events: &[Hosted<Event>],
    weeks: &[OverviewWeek],
    money: &BTreeMap<DateTime<Utc>, WindowMoney>,
    offered_by_week: &BTreeMap<DateTime<Utc>, f64>,
) -> Option<String> {
    let at = auto_burn_start(range, events)?;
    let before: Vec<&OverviewWeek> = weeks
        .iter()
        .filter(|week| {
            week.week
                .checked_add_signed(Duration::weeks(1))
                .is_some_and(|end| end <= at)
        })
        .collect();
    let after: Vec<&OverviewWeek> = weeks.iter().filter(|week| week.week >= at).collect();
    let (blocked_before, expired_before) = week_averages(&before, money, offered_by_week)?;
    let (blocked_after, expired_after) = week_averages(&after, money, offered_by_week)?;
    Some(format!(
        "Since auto-burn ({}): {} blocked a week, {} expired. Before: {}, {}.",
        date_text(at, &ctx.zone),
        hours_text(blocked_after),
        percent_text(expired_after),
        hours_text(blocked_before),
        percent_text(expired_before)
    ))
}

/// The latest `AutoBurnSet { on: true }` in the range that has a full week
/// before and after it and no `AutoBurnSet { on: false }` after it.
fn auto_burn_start(range: &Range, events: &[Hosted<Event>]) -> Option<DateTime<Utc>> {
    events.iter().rev().find_map(|hosted| {
        let at = hosted.record.t;
        if at < range.from || at >= range.to {
            return None;
        }
        let EventKind::AutoBurnSet { on: true } = &hosted.record.kind else {
            return None;
        };
        let stays_on = !events.iter().any(|later| {
            later.record.t > at
                && later.record.t < range.to
                && matches!(later.record.kind, EventKind::AutoBurnSet { on: false })
        });
        let week_before = at
            .checked_sub_signed(Duration::weeks(1))
            .is_some_and(|start| start >= range.from);
        let week_after = at
            .checked_add_signed(Duration::weeks(1))
            .is_some_and(|end| end <= range.to);
        (stays_on && week_before && week_after).then_some(at)
    })
}

/// The blocked hours a week and the expired share of some weeks; `None` when a
/// week list is empty or none of its weeks has a price. The share divides by the
/// windows the weeks offered, as the page's KPI does.
fn week_averages(
    weeks: &[&OverviewWeek],
    money: &BTreeMap<DateTime<Utc>, WindowMoney>,
    offered_by_week: &BTreeMap<DateTime<Utc>, f64>,
) -> Option<(f64, f64)> {
    if weeks.is_empty() {
        return None;
    }
    let mut blocked = 0.0;
    let mut offered = 0.0;
    let mut expired = 0.0;
    for week in weeks {
        blocked += week.blocked_hours;
        if let Some(week_money) = money.get(&week.week) {
            expired += week_money.expired;
        }
        if let Some(value) = offered_by_week.get(&week.week) {
            offered += value;
        }
    }
    if offered <= 0.0 {
        return None;
    }
    Some((blocked / weeks.len() as f64, expired / offered * 100.0))
}

/// 4. The account with the most full weeks.
fn finding_full<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, finished: &[AccountWeek]) -> Option<String> {
    let mut fullest: Option<(&crate::store::Account, u32, u32)> = None;
    for account in ctx.accounts {
        let reference = AccountRef::of(account);
        let total = finished
            .iter()
            .filter(|week| week.account == reference)
            .count() as u32;
        let full = finished
            .iter()
            .filter(|week| week.account == reference && week.used >= 100.0)
            .count() as u32;
        if full == 0 {
            continue;
        }
        if fullest.is_none_or(|(_, best, _)| full > best) {
            fullest = Some((account, full, total));
        }
    }
    let (account, full, total) = fullest?;
    let noun = if total == 1 { "week" } else { "weeks" };
    Some(format!(
        "{} was full in {full} of {total} {noun}.",
        account.abbr
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    use crate::history::{HistoryDir, Source};
    use crate::money::percent_value;
    use crate::stats::tests::{
        all, append_codex_records, codex_prices, codex_ref, codex_token, provider_subscription,
        tiny, utc,
    };
    use crate::stats::RangeKey;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.001
    }

    /// The plan's range: 2 Nov .. 16 Nov.
    fn two_weeks() -> Range {
        Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        }
    }

    fn week<'a>(page: &'a Overview, start: &str) -> &'a OverviewWeek {
        page.weeks
            .iter()
            .find(|week| week.week == utc(start))
            .unwrap_or_else(|| panic!("the week of {start}"))
    }

    /// One company's blocked hours as the page's KPI shows them.
    fn blocked(page: &Overview, company: Company) -> f64 {
        page.kpis
            .blocked_by_company
            .iter()
            .find(|entry| entry.company == company)
            .map(|entry| entry.hours)
            .unwrap_or(0.0)
    }

    #[test]
    fn the_totals() {
        let tiny = tiny();
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        assert!(close(page.kpis.used, 120.000), "used {}", page.kpis.used);
        assert!(close(page.kpis.paid, 147.692), "paid {}", page.kpis.paid);
        assert!(
            close(page.kpis.expired, 27.692),
            "expired {}",
            page.kpis.expired
        );
        assert_eq!(
            page.kpis.blocked_by_company,
            vec![BlockedByCompany {
                company: Company::Anthropic,
                hours: 2.0,
            }],
            "one entry per company with working hours, Anthropic first"
        );
        assert_eq!(page.kpis.avoidable_hours, 2.0);
        assert_eq!(page.kpis.switches.user, 1);
        assert_eq!(page.kpis.switches.cli, 0);
        assert_eq!(page.kpis.switches.auto, 1);
        assert_eq!(page.kpis.switches.agent, 1);
        assert_eq!(page.kpis.switches.external, 0);
        assert!(page.without_price.is_empty());
    }

    #[test]
    fn expired_by_account() {
        let tiny = tiny();
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let split = &page.kpis.expired_by_account;
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].0, tiny.ac().id);
        assert!(close(split[0].1, 19.385), "AC {}", split[0].1);
        assert_eq!(split[1].0, tiny.lb().id);
        assert!(close(split[1].1, 8.308), "LB {}", split[1].1);
    }

    #[test]
    fn per_week() {
        let tiny = tiny();
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let first = week(&page, "2026-11-02T00:00:00Z");
        assert!(
            close(first.expired_share.unwrap(), 21.875),
            "W1 share {:?}",
            first.expired_share
        );
        let lb = first
            .accounts
            .iter()
            .find(|account| account.id == tiny.lb().id)
            .expect("LB's W1");
        assert_eq!(lb.used_percent, 100.0);
        let ac = first
            .accounts
            .iter()
            .find(|account| account.id == tiny.ac().id)
            .expect("AC's W1");
        assert_eq!(ac.used_percent, 50.0);

        let second = week(&page, "2026-11-09T00:00:00Z");
        assert!(
            close(second.expired_share.unwrap(), 15.625),
            "W2 share {:?}",
            second.expired_share
        );
        assert_eq!(first.blocked_hours, 2.0);
        assert_eq!(second.blocked_hours, 0.0);
    }

    #[test]
    fn projects_per_week() {
        let tiny = tiny();
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let first = &week(&page, "2026-11-02T00:00:00Z").projects;
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].0, "atlas");
        assert!(close(first[0].1, 41.538), "atlas W1 {}", first[0].1);
        assert_eq!(first[1].0, "acme-kb");
        assert!(close(first[1].1, 16.154), "acme-kb W1 {}", first[1].1);

        let second = &week(&page, "2026-11-09T00:00:00Z").projects;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, "atlas");
        assert!(close(second[0].1, 62.308), "atlas W2 {}", second[0].1);
    }

    /// A token in the first window of a week that a reset split in two is
    /// attributed to that week: the token of 3 Nov lies in the 2 Nov .. 5 Nov
    /// 12:00 window, and its subscription value is the week's Atlas bar.
    #[test]
    fn a_token_in_the_first_window_of_a_reset_week_is_counted() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&Event {
                t: utc("2026-11-02T08:00:00Z"),
                kind: EventKind::Live {
                    account: Some(tiny.lb_ref()),
                },
            })
            .expect("LB live");
        history
            .append_many(&[
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: crate::api::LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-02T00:00:00Z")),
                    end: utc("2026-11-05T12:00:00Z"),
                    final_percent: 62.0,
                    lost: None,
                    last_sample: utc("2026-11-05T11:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: true,
                    from: None,
                },
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: crate::api::LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-05T12:00:00Z")),
                    end: utc("2026-11-09T12:00:00Z"),
                    final_percent: 30.0,
                    lost: Some(70.0),
                    last_sample: utc("2026-11-09T11:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: false,
                    from: Some(0.0),
                },
            ])
            .expect("the two windows of the reset week");
        history
            .append(&TokenRecord {
                t: utc("2026-11-03T10:00:00Z"),
                minutes: 60,
                session: "s0".to_string(),
                cwd: "/home/sam/projects/atlas/engine".to_string(),
                project: crate::project::ProjectKey::Repo("github.com/sam/atlas".to_string()),
                model: "claude-opus-5-5".to_string(),
                input: 0,
                out: 1_000_000,
                cw: 0,
                cr: 0,
                answers: 1,
                agent_answers: 0,
            })
            .expect("the token in the first window");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        // The range starts after the token and before the first window's
        // midpoint; the whole week counts, so the token counts too.
        let page = overview_in(
            &ctx,
            &Range {
                from: utc("2026-11-03T12:00:00Z"),
                to: utc("2026-11-21T00:00:00Z"),
            },
            &all(),
        )
        .unwrap();
        let first = week(&page, "2026-11-02T00:00:00Z");
        assert_eq!(
            first.projects.len(),
            1,
            "the token of 3 Nov is attributed to the week: {:?}",
            first.projects
        );
        assert_eq!(first.projects[0].0, "atlas");
        assert!(
            close(first.projects[0].1, 41.538_461_5 * 0.62),
            "the token's share of the first window's 62 %: {}",
            first.projects[0].1
        );
    }

    /// The week's project bars follow the host filter: vega has none of atlas's
    /// records, so its week has no project bar.
    #[test]
    fn projects_per_week_follow_the_host_filter() {
        let tiny = tiny();
        let query = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: Some("vega".to_string()),
        };

        let page = overview_in(&tiny.ctx(), &two_weeks(), &query).unwrap();

        for week in &page.weeks {
            assert!(
                week.projects.is_empty(),
                "vega has no project bar: {:#?}",
                week.projects
            );
        }
    }

    #[test]
    fn findings_are_worded_exactly() {
        let tiny = tiny();
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        assert!(
            page.findings
                .iter()
                .any(|text| text == "AC is where most money expired: 19 EUR in 2 weeks."),
            "findings: {:#?}",
            page.findings
        );
        assert!(
            page.findings
                .iter()
                .any(|text| text == "LB was full in 1 of 2 weeks."),
            "findings: {:#?}",
            page.findings
        );
        assert!(
            page.findings.iter().any(|text| text
                == "You were blocked 2h in 2 weeks; 2h of them while other quota expired."),
            "findings: {:#?}",
            page.findings
        );
        assert_eq!(page.findings.len(), 3, "findings: {:#?}", page.findings);
    }

    /// A sentence that counts one week says "week", not "weeks".
    #[test]
    fn a_sentence_counting_one_week_is_singular() {
        let tiny = tiny();
        let one_week = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-09T00:00:00Z"),
        };
        let page = overview_in(&tiny.ctx(), &one_week, &all()).unwrap();

        assert!(
            page.findings
                .iter()
                .any(|text| text == "AC is where most money expired: 16 EUR in 1 week."),
            "findings: {:#?}",
            page.findings
        );
        assert!(
            page.findings
                .iter()
                .any(|text| text == "LB was full in 1 of 1 week."),
            "findings: {:#?}",
            page.findings
        );
    }

    #[test]
    fn an_account_without_a_price_is_left_out_and_named() {
        let mut tiny = tiny();
        let ac_id = tiny.ac().id.clone();
        tiny.accounts[1].price = None;

        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert!(close(page.kpis.used, 74.769), "used {}", page.kpis.used);
        assert_eq!(page.without_price, vec![ac_id]);
        let first = week(&page, "2026-11-02T00:00:00Z");
        let ac = first
            .accounts
            .iter()
            .find(|account| account.id == tiny.ac().id)
            .expect("AC's W1");
        assert_eq!(ac.used, None);
        assert_eq!(ac.expired, None);
    }

    #[test]
    fn marks_name_added_accounts() {
        let tiny = tiny();
        tiny.history
            .append(&Event {
                t: utc("2026-11-05T00:00:00Z"),
                kind: EventKind::AccountAdded {
                    account: tiny.ac_ref(),
                    plan: Some("Team Premium".to_string()),
                },
            })
            .expect("AC added");

        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let mark = page
            .marks
            .iter()
            .find(|mark| mark.label == "AC added")
            .expect("AC's mark");
        assert_eq!(mark.at, utc("2026-11-05T00:00:00Z"));
    }

    /// The fixture's running weekly windows on the This-week card: LB at 30 %
    /// and AC at 40 %, each from its start to its reset, with the money of its
    /// use so far at its price. No token record lies in them yet, so neither
    /// has a project split.
    #[test]
    fn the_card_lists_the_running_windows() {
        let tiny = tiny();
        let card = this_week_card(&tiny.ctx(), &all()).unwrap();

        let money: Vec<f64> = card
            .accounts
            .iter()
            .map(|account| account.money.expect("a priced window"))
            .collect();
        assert_eq!(money.len(), 2);
        assert!(
            close(money[0], 0.30 * 180.0 * 12.0 / 52.0),
            "LB {}",
            money[0]
        );
        assert!(
            close(money[1], 0.40 * 140.0 * 12.0 / 52.0),
            "AC {}",
            money[1]
        );
        let without_money: Vec<ThisWeekAccount> = card
            .accounts
            .iter()
            .map(|account| ThisWeekAccount {
                money: None,
                ..account.clone()
            })
            .collect();
        assert_eq!(
            without_money,
            vec![
                ThisWeekAccount {
                    id: tiny.lb().id.clone(),
                    used: 30.0,
                    now: 30.0,
                    money: None,
                    money_missing: None,
                    started_at: utc("2026-11-16T17:00:00Z"),
                    resets_at: utc("2026-11-23T17:00:00Z"),
                    projects: Vec::new(),
                },
                ThisWeekAccount {
                    id: tiny.ac().id.clone(),
                    used: 40.0,
                    now: 40.0,
                    money: None,
                    money_missing: None,
                    started_at: utc("2026-11-16T02:00:00Z"),
                    resets_at: utc("2026-11-23T02:00:00Z"),
                    projects: Vec::new(),
                },
            ]
        );
        assert!(card.projects.is_empty());
    }

    /// The card ignores the range: a query for last week or for the last
    /// twelve months still lists the windows running now.
    #[test]
    fn the_card_ignores_the_range() {
        let tiny = tiny();
        for range in [RangeKey::LastWeek, RangeKey::Months12] {
            let query = Query { range, ..all() };
            let card = this_week_card(&tiny.ctx(), &query).unwrap();
            let listed: Vec<(&str, f64)> = card
                .accounts
                .iter()
                .map(|account| (account.id.as_str(), account.used))
                .collect();
            assert_eq!(
                listed,
                vec![(tiny.lb().id.as_str(), 30.0), (tiny.ac().id.as_str(), 40.0)],
                "the card of the range {range:?}"
            );
        }
    }

    /// The history counts finished windows only: the week that holds now has
    /// no account week while its windows run, the KPIs count nothing of them
    /// and no finding counts the running week.
    #[test]
    fn the_history_does_not_count_a_running_window() {
        let tiny = tiny();
        let this_week = Range {
            from: utc("2026-11-16T00:00:00Z"),
            to: tiny.now,
        };
        let page = overview_in(&tiny.ctx(), &this_week, &all()).unwrap();

        assert_eq!(page.kpis.used, 0.0);
        assert_eq!(page.kpis.paid, 0.0);
        assert_eq!(page.kpis.expired, 0.0);
        assert_eq!(page.kpis.used_percent, None);
        assert_eq!(page.kpis.expired_percent, None);
        assert_eq!(page.weeks.len(), 1);
        assert!(page.weeks[0].accounts.is_empty());
        assert!(page.without_price.is_empty());
        assert!(
            page.findings.iter().all(|text| !text.contains("full in")),
            "findings: {:#?}",
            page.findings
        );
    }

    /// A reset began AC's running window: the card counts its use from the
    /// reading the reset began it at (60 − 45 = 15 %), not the newest
    /// reading, and starts it at the reset. LB has no reading in this
    /// history and is not listed.
    #[test]
    fn the_card_counts_the_use_since_a_reset() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let reset_at = utc("2026-11-18T08:30:00Z");
        let next_reset = utc("2026-11-25T08:30:00Z");
        let weekly = |t: DateTime<Utc>, p: f64| Sample {
            t,
            account: tiny.ac_ref(),
            src: crate::history::Source::Poll,
            l: vec![crate::history::LimitSample {
                k: LimitKind::WeeklyAll,
                m: None,
                p,
                r: Some(next_reset),
                b: true,
            }],
        };
        history
            .append(&WindowRecord {
                account: tiny.ac_ref(),
                k: LimitKind::WeeklyAll,
                m: None,
                start: Some(utc("2026-11-16T02:00:00Z")),
                end: reset_at,
                final_percent: 70.0,
                lost: None,
                last_sample: utc("2026-11-18T08:00:00Z"),
                plan: Some("Team Premium".to_string()),
                uncertain: false,
                reset: true,
                from: None,
            })
            .expect("the reset window");
        history
            .append_many(&[weekly(reset_at, 45.0), weekly(tiny.now, 60.0)])
            .expect("AC's readings after the reset");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let card = this_week_card(&ctx, &all()).unwrap();
        assert_eq!(card.accounts.len(), 1, "only AC has a reading");
        let ac = &card.accounts[0];
        assert_eq!(ac.id, tiny.ac().id);
        assert_eq!(ac.used, 15.0, "60 % now minus the 45 % the reset began at");
        assert_eq!(ac.now, 60.0);
        assert_eq!(ac.started_at, reset_at);
        assert_eq!(ac.resets_at, next_reset);
        assert!(
            close(ac.money.expect("AC's price"), 0.15 * 140.0 * 12.0 / 52.0),
            "money {:?}",
            ac.money
        );
    }

    /// An account without any usage reading has no running window: the card
    /// does not list it, and a history without any reading has an empty card.
    #[test]
    fn an_account_without_a_reading_is_not_listed() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };
        let card = this_week_card(&ctx, &all()).unwrap();
        assert!(card.accounts.is_empty());
        assert!(card.projects.is_empty());

        let only_lb = Query {
            range: RangeKey::All,
            accounts: vec![tiny.lb().id.clone()],
            host: None,
        };
        let card = this_week_card(&tiny.ctx(), &only_lb).unwrap();
        let listed: Vec<&str> = card
            .accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect();
        assert_eq!(
            listed,
            vec![tiny.lb().id.as_str()],
            "the chosen account only"
        );
    }

    /// Each account's API value in its running window by project, and all
    /// listed accounts' and providers' together. AC worked 3 M output tokens
    /// on atlas and 1 M on acme-kb (60 and 20 EUR), LB 2 M on atlas (40 EUR).
    /// A flash worker's 9 M on acme-kb (18 EUR) is no account's: it is the
    /// Ollama row, and in the combined legend. A record before AC's window
    /// and one of another host's filter are left out.
    ///
    /// Until 1.2.1 this test asserted that the flash record was left out of
    /// the card altogether; it is now the Ollama row.
    #[test]
    fn the_card_splits_each_account_by_project() {
        let mut tiny = tiny();
        price_flash_and_codex(&mut tiny);
        let atlas = crate::project::ProjectKey::Repo("github.com/sam/atlas".to_string());
        let acme_kb = crate::project::ProjectKey::Repo("github.com/acme/acme-kb".to_string());
        let token =
            |t: &str, project: &crate::project::ProjectKey, model: &str, out: u64| TokenRecord {
                t: utc(t),
                minutes: 60,
                session: "4a1f9c2e-6b3d-4e8a-9f7c-1d2e3b4a5c6d".to_string(),
                cwd: "/home/sam/projects/atlas/engine".to_string(),
                project: project.clone(),
                model: model.to_string(),
                input: 0,
                out,
                cw: 0,
                cr: 0,
                answers: 1,
                agent_answers: 0,
            };
        tiny.history
            .append_many(&[
                // In AC's finished window, before its running one began.
                token("2026-11-16T01:00:00Z", &atlas, "claude-opus-5-5", 5_000_000),
                token("2026-11-17T10:00:00Z", &atlas, "claude-opus-5-5", 3_000_000),
                token(
                    "2026-11-17T12:00:00Z",
                    &acme_kb,
                    "claude-opus-5-5",
                    1_000_000,
                ),
                token(
                    "2026-11-17T13:00:00Z",
                    &acme_kb,
                    "deepseek-v4.1-flash:cloud",
                    9_000_000,
                ),
                token("2026-11-18T10:00:00Z", &atlas, "claude-opus-5-5", 2_000_000),
            ])
            .expect("the tokens of this week");
        tiny.history
            .append(&Event {
                t: utc("2026-11-18T09:00:00Z"),
                kind: EventKind::Switch {
                    from: Some(tiny.ac_ref()),
                    to: tiny.lb_ref(),
                    by: crate::history::By::User,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: None,
                },
            })
            .expect("the switch to LB");

        let card = this_week_card(&tiny.ctx(), &all()).unwrap();
        let split = |id: &str| -> Vec<(String, f64)> {
            card.accounts
                .iter()
                .find(|account| account.id == id)
                .unwrap_or_else(|| panic!("{id} on the card"))
                .projects
                .clone()
        };
        assert_eq!(split(&tiny.lb().id), vec![("atlas".to_string(), 100.0)]);
        let ac = split(&tiny.ac().id);
        assert_eq!(ac.len(), 2, "AC's split: {ac:?}");
        assert_eq!(ac[0].0, "atlas");
        assert!(close(ac[0].1, 75.0), "atlas {}", ac[0].1);
        assert_eq!(ac[1].0, "acme-kb");
        assert!(close(ac[1].1, 25.0), "acme-kb {}", ac[1].1);
        assert_eq!(card.providers.len(), 1, "providers: {:?}", card.providers);
        let ollama = &card.providers[0];
        assert_eq!(ollama.name, "Ollama");
        assert!(close(ollama.api.expect("the flash price"), 18.0));
        assert_eq!(ollama.api_currency.as_deref(), Some("EUR"));
        assert_eq!(ollama.api_missing, None);
        assert_eq!(ollama.projects, vec![("acme-kb".to_string(), 100.0)]);
        assert_eq!(card.projects.len(), 2, "all: {:?}", card.projects);
        assert_eq!(card.projects[0].0, "atlas");
        assert!(close(card.projects[0].1, 100.0 / 138.0 * 100.0));
        assert_eq!(card.projects[1].0, "acme-kb");
        assert!(close(card.projects[1].1, 38.0 / 138.0 * 100.0));

        // vega has none of these records: the windows stay, the split goes.
        let vega = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: Some("vega".to_string()),
        };
        let card = this_week_card(&tiny.ctx(), &vega).unwrap();
        assert_eq!(card.accounts.len(), 2);
        assert!(card
            .accounts
            .iter()
            .all(|account| account.projects.is_empty()));
        assert!(card.providers.is_empty());
        assert!(card.projects.is_empty());
    }

    /// The flash model's and Codex's prices, so their records have an API value:
    /// 2 and 10 USD per million output tokens, at the fixture's rate of 1.
    fn price_flash_and_codex(tiny: &mut crate::stats::tests::Tiny) {
        tiny.prices.models.insert(
            crate::money::price_key("ollama-cloud", "deepseek-v4.1-flash"),
            vec![crate::money::PricePoint {
                from: utc("2026-01-01T00:00:00Z"),
                price: crate::money::ModelPrice {
                    input: 0.5,
                    output: 2.0,
                    cache_write: 0.0,
                    cache_read: 0.0,
                    tiered: false,
                },
            }],
        );
        tiny.prices.models.insert(
            crate::money::price_key("openai", "gpt-6.1-sol"),
            vec![crate::money::PricePoint {
                from: utc("2026-01-01T00:00:00Z"),
                price: crate::money::ModelPrice {
                    input: 1.25,
                    output: 10.0,
                    cache_write: 0.0,
                    cache_read: 0.125,
                    tiered: false,
                },
            }],
        );
    }

    /// A token line of this week for the provider tests.
    fn week_token(
        t: &str,
        project: &crate::project::ProjectKey,
        model: &str,
        out: u64,
    ) -> TokenRecord {
        TokenRecord {
            t: utc(t),
            minutes: 60,
            session: "019a7c3e-4b2a-7d3f-9e6a-b8c52f0a1d7e".to_string(),
            cwd: "/home/sam/projects/atlas".to_string(),
            project: project.clone(),
            model: model.to_string(),
            input: 0,
            out,
            cw: 0,
            cr: 0,
            answers: 1,
            agent_answers: 0,
        }
    }

    /// A Codex record done while AC was live is the Codex row, valued at API
    /// prices from Monday 00:00 of this calendar week: a record of last week
    /// is not in it. The account chips do not filter it out, the host filter
    /// does. AC's own split stays without it.
    #[test]
    fn the_card_lists_a_codex_row() {
        let mut tiny = tiny();
        price_flash_and_codex(&mut tiny);
        let atlas = crate::project::ProjectKey::Repo("github.com/sam/atlas".to_string());
        let ledger = crate::project::ProjectKey::Repo("github.com/sam/ledger".to_string());
        tiny.history
            .append_many(&[
                // Last week (Sun 15 Nov): before this calendar week.
                week_token("2026-11-15T20:00:00Z", &atlas, "gpt-6.1-sol", 4_000_000),
                week_token("2026-11-17T09:00:00Z", &atlas, "gpt-6.1-sol", 3_000_000),
                week_token("2026-11-18T09:00:00Z", &ledger, "gpt-6.1-sol", 1_000_000),
                week_token("2026-11-17T11:00:00Z", &atlas, "claude-opus-5-5", 1_000_000),
            ])
            .expect("this week's tokens");
        let only_lb = Query {
            range: RangeKey::All,
            accounts: vec![tiny.lb().id.clone()],
            host: None,
        };

        for query in [all(), only_lb] {
            let card = this_week_card(&tiny.ctx(), &query).unwrap();
            assert_eq!(card.providers.len(), 1, "providers: {:?}", card.providers);
            let codex = &card.providers[0];
            assert_eq!(codex.name, "OpenAI");
            assert!(
                close(codex.api.expect("Codex's price"), 40.0),
                "4 M output at 10 USD per million: {:?}",
                codex.api
            );
            assert_eq!(codex.api_missing, None);
            assert_eq!(codex.projects.len(), 2, "{:?}", codex.projects);
            assert_eq!(codex.projects[0].0, "atlas");
            assert!(close(codex.projects[0].1, 75.0));
            assert_eq!(codex.projects[1].0, "ledger");
            assert!(close(codex.projects[1].1, 25.0));
            assert!(card.projects.iter().any(|(name, _)| name == "ledger"));
        }

        let card = this_week_card(&tiny.ctx(), &all()).unwrap();
        let ac = card
            .accounts
            .iter()
            .find(|account| account.id == tiny.ac().id)
            .expect("AC on the card");
        assert_eq!(
            ac.projects,
            vec![("atlas".to_string(), 100.0)],
            "AC's split has no Codex work"
        );

        let vega = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: Some("vega".to_string()),
        };
        let card = this_week_card(&tiny.ctx(), &vega).unwrap();
        assert!(card.providers.is_empty(), "the host filter applies");
    }

    /// A provider's model without a price leaves its row's API value unknown,
    /// with the model named, and the combined legend unknown with it.
    #[test]
    fn an_unpriced_provider_row_names_its_reason() {
        let tiny = tiny();
        let atlas = crate::project::ProjectKey::Repo("github.com/sam/atlas".to_string());
        tiny.history
            .append(&week_token(
                "2026-11-18T09:00:00Z",
                &atlas,
                "glm-5:cloud",
                1_000_000,
            ))
            .expect("an Ollama record");
        let card = this_week_card(&tiny.ctx(), &all()).unwrap();
        assert_eq!(card.providers.len(), 1);
        let ollama = &card.providers[0];
        assert_eq!(ollama.name, "Ollama");
        assert_eq!(ollama.api, None);
        assert_eq!(ollama.api_currency, None);
        assert_eq!(
            ollama.api_missing.as_deref(),
            Some("no price for glm-5:cloud")
        );
        assert!(ollama.projects.is_empty());
        assert!(
            card.projects.is_empty(),
            "an unknown part makes the share unknown"
        );
    }

    /// Two accounts added and auto-burn switched on in one calendar week are
    /// one mark at the first of them, so their labels never print on top of
    /// each other; a mark of another week stays its own.
    #[test]
    fn marks_of_one_week_are_one_label() {
        let tiny = tiny();
        tiny.history
            .append_many(&[
                Event {
                    t: utc("2026-11-04T09:00:00Z"),
                    kind: EventKind::AccountAdded {
                        account: tiny.lb_ref(),
                        plan: Some("Max".to_string()),
                    },
                },
                Event {
                    t: utc("2026-11-04T09:03:00Z"),
                    kind: EventKind::AccountAdded {
                        account: tiny.ac_ref(),
                        plan: Some("Team Premium".to_string()),
                    },
                },
                Event {
                    t: utc("2026-11-06T17:00:00Z"),
                    kind: EventKind::AutoBurnSet { on: true },
                },
                Event {
                    t: utc("2026-11-10T08:00:00Z"),
                    kind: EventKind::AutoBurnSet { on: false },
                },
            ])
            .expect("the marked events");

        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(
            page.marks,
            vec![
                Mark {
                    at: utc("2026-11-04T09:00:00Z"),
                    label: "LB, AC added · auto-burn on".to_string(),
                },
                Mark {
                    at: utc("2026-11-10T08:00:00Z"),
                    label: "auto-burn off".to_string(),
                },
            ]
        );
    }

    /// An October range has history in none of its calendar weeks: every week
    /// is shown with no account and no blocked hour.
    #[test]
    fn a_range_without_history_has_weeks_without_accounts() {
        let tiny = tiny();
        let october = Range {
            from: utc("2026-10-01T00:00:00Z"),
            to: utc("2026-11-01T00:00:00Z"),
        };
        let page = overview_in(&tiny.ctx(), &october, &all()).unwrap();

        assert_eq!(page.weeks.len(), 5, "the calendar weeks of October");
        for week in &page.weeks {
            assert!(week.accounts.is_empty(), "{} has accounts", week.week);
            assert_eq!(week.blocked_hours, 0.0);
            assert_eq!(week.expired_share, None);
        }
        assert_eq!(page.kpis.used, 0.0);
        assert_eq!(page.kpis.paid, 0.0);
        assert_eq!(page.kpis.expired, 0.0);
        assert!(page.kpis.expired_by_account.is_empty());
        assert!(
            page.kpis.blocked_by_company.is_empty(),
            "a range with no work has no working hours and no company entry"
        );
        assert_eq!(page.kpis.avoidable_hours, 0.0);
        assert_eq!(page.kpis.switches, SwitchCounts::default());
        assert_eq!(page.kpis.switch_cost_percent_of_window, None);
        assert!(page.without_price.is_empty());
        assert!(page.marks.is_empty());
        assert!(page.findings.is_empty());
    }

    #[test]
    fn switches_of_one_host() {
        let tiny = tiny();
        let query = Query {
            range: RangeKey::All,
            accounts: Vec::new(),
            host: Some("vega".to_string()),
        };
        let page = overview_in(&tiny.ctx(), &two_weeks(), &query).unwrap();

        assert_eq!(page.kpis.switches, SwitchCounts::default());
    }

    /// Money is written as `crates/app/ui/common.js`'s `money` writes it.
    #[test]
    fn money_text_follows_the_page() {
        assert_eq!(money_text(8.308, "EUR"), "8.31 EUR");
        assert_eq!(money_text(19.385, "EUR"), "19 EUR");
        assert_eq!(money_text(1204.4, "EUR"), "1,204 EUR");
    }

    /// Auto-burn that was switched off again inside the range has no sentence:
    /// only a switch that stayed on for a full week on each side does.
    #[test]
    fn auto_burn_off_inside_the_range_removes_the_sentence() {
        let tiny = tiny();
        tiny.history
            .append_many(&[
                Event {
                    t: utc("2026-11-09T12:00:00Z"),
                    kind: EventKind::AutoBurnSet { on: true },
                },
                Event {
                    t: utc("2026-11-11T12:00:00Z"),
                    kind: EventKind::AutoBurnSet { on: false },
                },
            ])
            .expect("auto-burn on and off");
        let three_weeks = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-23T00:00:00Z"),
        };
        let page = overview_in(&tiny.ctx(), &three_weeks, &all()).unwrap();

        assert!(
            page.findings
                .iter()
                .all(|text| !text.starts_with("Since auto-burn")),
            "findings: {:#?}",
            page.findings
        );
    }

    // ---- proof tests of the review ----

    use crate::api::LimitKind;
    use crate::history::WindowRecord;

    /// One finished weekly window of an account.
    fn window(
        account: &crate::history::AccountRef,
        start: &str,
        end: &str,
        final_percent: f64,
    ) -> WindowRecord {
        WindowRecord {
            account: account.clone(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(utc(start)),
            end: utc(end),
            final_percent,
            lost: Some((100.0 - final_percent).max(0.0)),
            last_sample: utc(end) - Duration::minutes(1),
            plan: None,
            uncertain: false,
            reset: false,
            from: None,
        }
    }

    /// A window that ended above 100 %: nothing expired in it, so the page
    /// must not show a negative expired money, percent or share.
    #[test]
    fn a_window_above_100_percent_expired_nothing() {
        let tiny = tiny();
        tiny.history
            .append(&window(
                &tiny.lb_ref(),
                "2026-11-02T17:00:00Z",
                "2026-11-09T17:00:00Z",
                105.0,
            ))
            .expect("LB's window at 105 %");
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        for week in &page.weeks {
            assert!(
                week.expired_share.is_none_or(|share| share >= 0.0),
                "the share {:?} of the week",
                week.expired_share
            );
            for account in &week.accounts {
                assert!(
                    account.expired_percent >= 0.0,
                    "{} expired {} %",
                    account.id,
                    account.expired_percent
                );
                if let Some(expired) = account.expired {
                    assert!(expired >= 0.0, "{} expired {expired}", account.id);
                }
            }
        }
    }

    /// A week that a reset split into two windows counts both windows' use:
    /// 62 % from 0, a reset, then 30 % from 0 → the week used 92 %. Its expired
    /// part is the last window's `lost` (70 %), never the reset window's unused
    /// part (the reset gave that quota back). The page's percentages count each
    /// offered window (see `the_overview_percentages_are_shares_of_the_offered_weeks`),
    /// so this week is 46 % used and 35 % expired of the two weeks it offered.
    /// Every window's `lost` counts; the reset window has none.
    #[test]
    fn used_counts_every_window_of_a_week_with_a_reset() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append_many(&[
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-02T00:00:00Z")),
                    end: utc("2026-11-05T12:00:00Z"),
                    final_percent: 62.0,
                    lost: None,
                    last_sample: utc("2026-11-05T11:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: true,
                    from: None,
                },
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-05T12:00:00Z")),
                    end: utc("2026-11-09T12:00:00Z"),
                    final_percent: 30.0,
                    lost: Some(70.0),
                    last_sample: utc("2026-11-09T11:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: false,
                    from: Some(0.0),
                },
            ])
            .expect("the two windows of the reset week");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        let page = overview_in(&ctx, &two_weeks(), &all()).unwrap();
        let first = week(&page, "2026-11-02T00:00:00Z");
        let lb = first
            .accounts
            .iter()
            .find(|account| account.id == tiny.lb().id)
            .expect("LB's week");
        assert_eq!(lb.used_percent, 92.0, "62 % then 30 % from 0");
        assert_eq!(
            lb.expired_percent, 70.0,
            "every window's lost; the reset window has none"
        );
        let weekly = tiny.lb().price.expect("LB's price") * 12.0 / 52.0;
        assert!(
            close(lb.used.expect("LB's price"), 0.92 * weekly),
            "used money {}",
            lb.used.unwrap()
        );
        assert!(
            close(lb.expired.expect("LB's price"), 0.70 * weekly),
            "expired money {}",
            lb.expired.unwrap()
        );
        assert!(
            close(page.kpis.used, 0.92 * weekly),
            "used {}",
            page.kpis.used
        );
        assert!(
            close(page.kpis.paid, weekly),
            "paid {}: one week's price, not two",
            page.kpis.paid
        );
        assert!(
            close(page.kpis.used_percent.expect("a priced week"), 46.0),
            "the two offered windows: {:?}",
            page.kpis.used_percent
        );
        assert!(
            close(page.kpis.expired_percent.expect("a priced week"), 35.0),
            "the two offered windows: {:?}",
            page.kpis.expired_percent
        );
    }

    /// Used and Expired are shares of the offered weeks. Every finished weekly
    /// window offers one week of its account's price; a week that a reset split
    /// offers each of its windows. Used is the weeks' use (Σ final − from),
    /// expired is every window's lost, and the rest is still open, so
    /// used + expired + still open make 100 % of the windows counted. With one
    /// window per week this is the mock-up's `used / paid` and `expired / paid`.
    #[test]
    fn the_overview_percentages_are_shares_of_the_offered_weeks() {
        let tiny = tiny();
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let used = page.kpis.used_percent.expect("the weeks have prices");
        let expired = page.kpis.expired_percent.expect("the weeks have prices");
        assert!(close(used, 81.25), "used {used}");
        assert!(close(expired, 18.75), "expired {expired}");
        assert!(
            close(used + expired, 100.0),
            "used + expired = {}",
            used + expired
        );
        assert_eq!(page.kpis.weeks, 2, "the range's two calendar weeks");
        assert_eq!(
            page.kpis.switches_total, 3,
            "you 1 · cli 0 · auto-burn 1 · agents 1 · external 0"
        );
    }

    /// The Blocked sub-line's number: the hours of the calendar weeks before the
    /// week the last account added in the range was added. AC was added on
    /// 12 Nov; the 2 h block of 7 Nov lies in the week before it.
    #[test]
    fn the_blocked_hours_before_the_last_added_account() {
        let tiny = tiny();
        tiny.history
            .append(&Event {
                t: utc("2026-11-12T00:00:00Z"),
                kind: EventKind::AccountAdded {
                    account: tiny.ac_ref(),
                    plan: Some("Team Premium".to_string()),
                },
            })
            .expect("AC added");

        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let before = page
            .kpis
            .blocked_before_added
            .expect("AC was added in the range");
        assert_eq!(before.name, tiny.ac().abbr);
        assert!(
            close(before.hours, 2.0),
            "the hours before AC: {}",
            before.hours
        );
    }

    /// The hours the page shows are the hours its weeks show: every calendar
    /// week of the range is on the page, so the blocked hours of all of them
    /// add up to the KPI.
    #[test]
    fn the_blocked_hours_of_the_page_add_up() {
        let tiny = tiny();
        // A range that starts inside the week of the block: that week holds
        // no finished window of the range, but it is on the page.
        let range = Range {
            from: utc("2026-11-07T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };
        let page = overview_in(&tiny.ctx(), &range, &all()).unwrap();

        let shown: f64 = page.weeks.iter().map(|week| week.blocked_hours).sum();
        assert!(
            close(shown, blocked(&page, Company::Anthropic)),
            "the weeks show {shown} hours, the page says {} hours",
            blocked(&page, Company::Anthropic)
        );
    }

    /// A range without a block has no hours; the page must not show a
    /// negative zero.
    #[test]
    fn the_hours_of_a_range_without_a_block_are_not_negative() {
        let tiny = tiny();
        let october = Range {
            from: utc("2026-10-01T00:00:00Z"),
            to: utc("2026-11-01T00:00:00Z"),
        };
        let page = overview_in(&tiny.ctx(), &october, &all()).unwrap();

        assert!(
            page.kpis
                .blocked_by_company
                .iter()
                .all(|entry| entry.hours.is_sign_positive()),
            "blocked hours {:?}",
            page.kpis.blocked_by_company
        );
        assert!(
            page.kpis.avoidable_hours.is_sign_positive(),
            "avoidable hours {}",
            page.kpis.avoidable_hours
        );
    }

    /// No Anthropic entry in `blocked_by_company` means there is no blocked
    /// finding to show, not a finding of 0 h (review, 1.3.0).
    #[test]
    fn a_missing_anthropic_blocked_entry_has_no_finding() {
        let tiny = tiny();
        let october = Range {
            from: utc("2026-10-01T00:00:00Z"),
            to: utc("2026-11-01T00:00:00Z"),
        };
        let page = overview_in(&tiny.ctx(), &october, &all()).unwrap();
        assert!(
            page.kpis
                .blocked_by_company
                .iter()
                .all(|entry| entry.company != Company::Anthropic),
            "the range has no Anthropic blocked hours: {:?}",
            page.kpis.blocked_by_company
        );
        assert_eq!(finding_blocked(&page.kpis, &page.weeks), None);
    }

    /// P-2 counts an OpenAI subscription's finished week as an account week, so
    /// the Codex week joins the page as a row without a price; every Claude
    /// figure stays what it was.
    #[test]
    fn provider_records_add_a_provider_week_but_change_no_claude_figure() {
        let without = tiny();
        let with = tiny();
        append_codex_records(&with.history, utc("2026-11-14T09:00:00Z"));

        let before = overview_in(&without.ctx(), &two_weeks(), &all()).expect("the overview");
        let mut after = overview_in(&with.ctx(), &two_weeks(), &all()).expect("the overview");
        let provider_row = after
            .weeks
            .iter_mut()
            .find_map(|week| {
                let index = week
                    .accounts
                    .iter()
                    .position(|row| row.id == "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13")?;
                Some(week.accounts.remove(index))
            })
            .expect("the Codex week is an account week on the page");
        assert_eq!(provider_row.used_percent, 12.0);
        assert_eq!(provider_row.used, None, "the subscription has no price");
        assert_eq!(provider_row.expired, None);

        assert_eq!(
            after, before,
            "a Codex record changed a Claude figure of the overview"
        );
    }

    /// A priced provider subscription fills its This-week row like an account's
    /// row (decision P-2 7): the running week's use, its money, its reset. The
    /// API value stays as it was.
    #[test]
    fn this_week_rows_for_providers_carry_subscription_money() {
        let mut tiny = tiny();
        let codex = codex_ref();
        tiny.provider_subscriptions = vec![provider_subscription(&codex, "OA", None)];
        codex_prices(&mut tiny.prices);
        tiny.history
            .append(&Sample {
                t: utc("2026-11-17T09:00:00Z"),
                account: codex.clone(),
                src: Source::Poll,
                l: vec![crate::history::LimitSample {
                    k: LimitKind::WeeklyAll,
                    m: None,
                    p: 50.0,
                    r: Some(utc("2026-11-23T10:00:00Z")),
                    b: true,
                }],
            })
            .expect("the Codex reading");
        tiny.history
            .append(&Event {
                t: utc("2026-11-01T00:00:00Z"),
                kind: EventKind::PriceChanged {
                    account: codex,
                    amount: Some(23.0),
                    currency: Some("EUR".to_string()),
                },
            })
            .expect("the subscription's price");
        tiny.history
            .append(&codex_token(
                "2026-11-17T09:00:00Z",
                "gpt-6.2-codex",
                1_000_000,
            ))
            .expect("a Codex record of this week");
        let card = this_week_card(&tiny.ctx(), &all()).expect("the This-week card");
        let openai = card
            .providers
            .iter()
            .find(|row| row.name == "OA")
            .expect("the OA row");
        assert_eq!(openai.used, Some(50.0), "the running week's use so far");
        assert_eq!(openai.resets_at, Some(utc("2026-11-23T10:00:00Z")));
        assert_eq!(openai.money_missing, None);
        assert!(
            close(openai.money.expect("money"), 50.0 * percent_value(23.0)),
            "the used share of the week's 23 EUR: {:?}",
            openai.money
        );
        assert_eq!(openai.api, Some(10.0), "the API value stays");
    }

    /// Each OpenAI login gets its own This-week row, summing its own records
    /// from its own running week's start, with that week's use and money
    /// (decision P-2 6, review F): OA and OA2, never one OpenAI row.
    #[test]
    fn two_openai_logins_have_one_this_week_row_each() {
        let mut tiny = tiny();
        let oa = codex_ref();
        let oa2 = AccountRef {
            acc: "5d2e7a1c-3b4f-4a6e-9c8d-1f2a3b4c5d6e".to_string(),
            org: crate::providers::CODEX_ORG.to_string(),
        };
        tiny.provider_subscriptions = vec![
            provider_subscription(&oa, "OA", Some(23.0)),
            provider_subscription(&oa2, "OA2", Some(30.0)),
        ];
        codex_prices(&mut tiny.prices);
        for (account, percent, reset) in [
            (&oa, 50.0, "2026-11-23T10:00:00Z"),
            (&oa2, 20.0, "2026-11-24T10:00:00Z"),
        ] {
            tiny.history
                .append(&Sample {
                    t: utc("2026-11-17T09:00:00Z"),
                    account: account.clone(),
                    src: Source::Poll,
                    l: vec![crate::history::LimitSample {
                        k: LimitKind::WeeklyAll,
                        m: None,
                        p: percent,
                        r: Some(utc(reset)),
                        b: true,
                    }],
                })
                .expect("a running week");
        }
        tiny.history
            .append(&Event {
                t: utc("2026-11-17T13:00:00Z"),
                kind: EventKind::ProviderSwitch {
                    from: Some(oa.clone()),
                    to: oa2.clone(),
                    by: crate::history::By::User,
                },
            })
            .expect("the switch to OA2");
        tiny.history
            .append(&codex_token(
                "2026-11-17T12:00:00Z",
                "gpt-6.2-codex",
                1_000_000,
            ))
            .expect("OA's record");
        tiny.history
            .append(&codex_token(
                "2026-11-17T14:00:00Z",
                "gpt-6.2-codex",
                1_000_000,
            ))
            .expect("OA2's record");

        let card = this_week_card(&tiny.ctx(), &all()).expect("the This-week card");
        assert_eq!(card.providers.len(), 2, "providers: {:?}", card.providers);
        let oa_row = card
            .providers
            .iter()
            .find(|row| row.name == "OA")
            .expect("the OA row");
        assert_eq!(oa_row.used, Some(50.0));
        assert_eq!(oa_row.api, Some(10.0), "OA's own 12:00 record");
        assert!(
            close(oa_row.money.expect("money"), 50.0 * percent_value(23.0)),
            "OA's used share: {:?}",
            oa_row.money
        );
        let oa2_row = card
            .providers
            .iter()
            .find(|row| row.name == "OA2")
            .expect("the OA2 row");
        assert_eq!(oa2_row.used, Some(20.0));
        assert_eq!(oa2_row.api, Some(10.0), "OA2's own 14:00 record");
        assert!(
            close(oa2_row.money.expect("money"), 20.0 * percent_value(30.0)),
            "OA2's used share: {:?}",
            oa2_row.money
        );
    }

    /// A provider row whose subscription has no running window still says why
    /// its money is missing, in words for that subscription (decision P-2 7,
    /// review E): never four `None` fields without a reason.
    #[test]
    fn a_provider_row_without_a_running_window_says_why() {
        let mut tiny = tiny();
        let oa = codex_ref();
        let ollama = crate::money::ollama_subscription();
        tiny.provider_subscriptions = vec![
            provider_subscription(&oa, "OA", Some(23.0)),
            provider_subscription(&ollama, "OL", Some(20.0)),
        ];

        let card = this_week_card(&tiny.ctx(), &all()).expect("the This-week card");
        let oa_row = card
            .providers
            .iter()
            .find(|row| row.name == "OA")
            .expect("the OA row");
        assert_eq!(oa_row.used, None);
        assert_eq!(
            oa_row.money_missing.as_deref(),
            Some("no reading of this week's usage")
        );
        let ollama_row = card
            .providers
            .iter()
            .find(|row| row.name == "OL")
            .expect("the OL row");
        assert_eq!(ollama_row.used, None);
        assert_eq!(
            ollama_row.money_missing.as_deref(),
            Some("no monthly allowance in Ollama's reading")
        );
    }

    /// RF5: Ollama's missing month has one reason, decided by the Valuer. A
    /// subscription with a price and no `monthly_credits` reading says "no
    /// monthly allowance" on the Overview and through `Valuer::subscription`.
    #[test]
    fn ollama_without_a_month_says_the_same_reason_everywhere() {
        let mut tiny = tiny();
        let ollama = crate::money::ollama_subscription();
        tiny.provider_subscriptions = vec![provider_subscription(&ollama, "OL", Some(20.0))];

        let card = this_week_card(&tiny.ctx(), &all()).expect("the This-week card");
        let row = card
            .providers
            .iter()
            .find(|row| row.name == "OL")
            .expect("the OL row");
        let valuer = Valuer::build(
            &tiny.history,
            &Range {
                from: tiny.now - Duration::days(7),
                to: tiny.now,
            },
            &tiny.prices,
            &tiny.fx,
            &tiny.settings,
            &tiny.accounts,
            &tiny.provider_subscriptions,
            &tiny.warnings,
        )
        .expect("a valuer");
        let from_valuer = valuer
            .subscription(
                &ollama,
                &LimitKind::Other("monthly_credits".to_string()),
                tiny.now,
                1.0,
            )
            .expect_err("no month window");
        assert_eq!(row.money_missing, Some(from_valuer.to_string()));
        assert_eq!(
            row.money_missing.as_deref(),
            Some("no monthly allowance in Ollama's reading")
        );
    }

    /// A window that ended above 100 % used more than it was offered. Its whole
    /// use counts in the used share, so the two shares the page shows pass
    /// 100 % together (here 82.7 % + 18.8 %): nothing is cut, and the rule on
    /// `Kpis::used_percent` says so.
    #[test]
    fn a_window_above_100_percent_counts_its_whole_use_in_the_used_share() {
        let tiny = tiny();
        tiny.history
            .append(&window(
                &tiny.lb_ref(),
                "2026-11-02T17:00:00Z",
                "2026-11-09T17:00:00Z",
                105.0,
            ))
            .expect("LB's window at 105 %");
        let page = overview_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let used = page.kpis.used_percent.expect("the weeks have prices");
        let expired = page.kpis.expired_percent.expect("the weeks have prices");
        assert!(close(used, 82.65625), "the used share: {used} %");
        assert!(close(expired, 18.75), "the expired share: {expired} %");
        assert!(
            used + expired > 100.0,
            "the whole use counts, nothing is cut"
        );
    }

    /// The KPI's expired share and the "Share that expired, per week" chart's
    /// share of the same week follow one rule: the expired money over the money
    /// the week's windows offered. A week a reset split offers two windows, so
    /// both say 35 % (70 % of one window over two).
    #[test]
    fn the_overview_share_and_the_weeks_own_share_are_the_same_number() {
        let tiny = tiny();
        let dir = tempdir().expect("a temp history");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append_many(&[
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-02T00:00:00Z")),
                    end: utc("2026-11-05T12:00:00Z"),
                    final_percent: 62.0,
                    lost: None,
                    last_sample: utc("2026-11-05T11:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: true,
                    from: None,
                },
                WindowRecord {
                    account: tiny.lb_ref(),
                    k: LimitKind::WeeklyAll,
                    m: None,
                    start: Some(utc("2026-11-05T12:00:00Z")),
                    end: utc("2026-11-09T12:00:00Z"),
                    final_percent: 30.0,
                    lost: Some(70.0),
                    last_sample: utc("2026-11-09T11:58:00Z"),
                    plan: Some("Max".to_string()),
                    uncertain: false,
                    reset: false,
                    from: Some(0.0),
                },
            ])
            .expect("the two windows of the reset week");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };
        let one_week = Range {
            from: utc("2026-11-02T00:00:00Z"),
            to: utc("2026-11-09T00:00:00Z"),
        };

        let page = overview_in(&ctx, &one_week, &all()).unwrap();
        let first = week(&page, "2026-11-02T00:00:00Z");
        let kpi = page.kpis.expired_percent.expect("the week has a price");
        let own = first.expired_share.expect("the week has a price");
        assert!(close(kpi, 35.0), "the KPI's expired share: {kpi} %");
        assert!(
            close(kpi, own),
            "the KPI says {kpi} %, the week's own share says {own} %"
        );
    }
}
