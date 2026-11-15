//! The Overview page (spec §3.1): what the chosen range used, paid and let
//! expire, the hours blocked, the switches and what they cost, each week by
//! account and by project, the marks of the range, and the findings. Everything
//! here only reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::burn::cost::switch_cost;
use crate::burn::rates;
use crate::burn::{Rates, RATES_DAYS};
use crate::history::{
    AccountRef, Event, EventKind, Hosted, Hosts, Range, Read, Sample, TokenRecord,
};
use crate::money::{Valuer, WindowMoney};
use crate::projects::ProjectId;

use super::{
    account_abbr, account_id, account_position, account_weeks_valued, accounts_without_price,
    blocked_by_week, blocked_spans, date_text, hours_text, lost_percent, money_of, money_text,
    percent_text, range_of, switch_counts, top_three, week_columns, week_of_record, working_rhythm,
    AccountWeek, Ctx, Query, StatsError, SwitchCounts,
};

/// The Overview page's numbers for one range (spec §3.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    /// The settings' currency; `None` until one is set.
    pub currency: Option<String>,
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

/// The five numbers of the page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Kpis {
    /// Money used of the priced account weeks: `paid − expired`.
    pub used: f64,
    /// Money paid for the priced account weeks: `100 × pv`.
    pub paid: f64,
    /// `lost × pv` of the priced account weeks.
    pub expired: f64,
    /// The expired money per account id, largest first.
    pub expired_by_account: Vec<(String, f64)>,
    /// The working hours blocked in the range.
    pub blocked_hours: f64,
    /// The part of `blocked_hours` in which other quota expired.
    pub avoidable_hours: f64,
    pub switches: SwitchCounts,
    /// What one switch costs the account in use at the range's end, as a
    /// percent of its 5 h window; `None` when the rates or the cost are missing.
    pub switch_cost_percent_of_window: Option<f64>,
}

/// One calendar week of the range, with the account weeks that fall in it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewWeek {
    pub week: DateTime<Utc>,
    pub accounts: Vec<WeekAccount>,
    /// The projects' subscription value, the three largest then `other`.
    pub projects: Vec<(String, f64)>,
    /// The working hours blocked in this week.
    pub blocked_hours: f64,
    /// `expired / paid × 100` of the priced account weeks of this week;
    /// `None` when no account week of the week has a price.
    pub expired_share: Option<f64>,
}

/// One account's week as the Overview shows it.
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

/// A moment the page marks on its chart: an account was added, or auto-burn
/// was switched on or off.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    pub at: DateTime<Utc>,
    pub label: String,
}

pub fn overview(ctx: &Ctx, q: &Query) -> Result<Overview, StatsError> {
    let range = range_of(q.range, ctx)?;
    overview_in(ctx, &range, q)
}

/// The Overview of `range`; the page's [`overview`] takes the range from its
/// [`Query`], the tests name one.
pub(crate) fn overview_in(ctx: &Ctx, range: &Range, q: &Query) -> Result<Overview, StatsError> {
    let valuer = Valuer::build(
        ctx.history,
        range,
        ctx.prices,
        ctx.fx,
        ctx.settings,
        ctx.accounts,
    )?;
    let finished = account_weeks_valued(ctx, range, q, &valuer)?;
    let spans = blocked_spans(ctx, range, q)?;
    let rhythm = working_rhythm(ctx, range)?;
    let currency = ctx.settings.currency.clone();

    let events = ctx.history.read::<Event>(&Range::ALL, &Hosts::All)?;
    let reading = reading_range(range, &finished);
    let samples = ctx.history.read::<Sample>(&reading, &Hosts::All)?;
    let tokens = ctx.history.read::<TokenRecord>(&reading, &Hosts::All)?;

    let without_price = accounts_without_price(ctx, &finished);

    // The page shows every calendar week of the range, with or without an
    // account week; the blocked hours of all of them add up to the KPI.
    let mut weeks_by_start: BTreeMap<DateTime<Utc>, OverviewWeek> = week_columns(range, ctx.offset)
        .into_iter()
        .map(|week| (week, empty_week(week)))
        .collect();

    // The money of every priced account week; a week without a price is left
    // out of the sums and its account is named in `without_price`.
    let mut used = 0.0;
    let mut paid = 0.0;
    let mut expired = 0.0;
    let mut priced_weeks: BTreeMap<AccountRef, u32> = BTreeMap::new();
    let mut expired_by_account: BTreeMap<AccountRef, f64> = BTreeMap::new();
    let mut money: BTreeMap<DateTime<Utc>, WindowMoney> = BTreeMap::new();
    for week in &finished {
        let week_money = money_of(week);
        if let Some(week_money) = week_money {
            used += week_money.used;
            paid += week_money.paid;
            expired += week_money.expired;
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
                used_percent: week.window.final_percent,
                used: week_money.map(|money| money.used),
                expired_percent: lost_percent(&week.window)?,
                expired: week_money.map(|money| money.expired),
                uncertain: week.window.uncertain,
            });
    }

    let mut kpis = Kpis {
        used,
        paid,
        expired,
        expired_by_account: expired_by_account
            .into_iter()
            .map(|(reference, expired)| (account_id(ctx, &reference), expired))
            .collect(),
        blocked_hours: spans.iter().fold(0.0, |sum, span| sum + span.hours),
        avoidable_hours: spans
            .iter()
            .fold(0.0, |sum, span| sum + span.avoidable_hours),
        switches: switch_counts(&events.records, range, q.host.as_deref()),
        switch_cost_percent_of_window: switch_cost_percent(ctx, range, &events, &samples, &tokens),
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

    for (week, hours) in blocked_by_week(&spans, &rhythm, ctx.offset) {
        weeks_by_start
            .entry(week)
            .or_insert_with(|| empty_week(week))
            .blocked_hours += hours;
    }

    for (start, week) in weeks_by_start.iter_mut() {
        if let Some(week_money) = money.get(start) {
            if week_money.paid > 0.0 {
                week.expired_share = Some(week_money.expired / week_money.paid * 100.0);
            }
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

    let mut marks = Vec::new();
    for hosted in &events.records {
        let at = hosted.record.t;
        if at < range.from || at >= range.to {
            continue;
        }
        match &hosted.record.kind {
            EventKind::AccountAdded { account, .. } => {
                let label = account_abbr(ctx, account);
                marks.push(Mark {
                    at,
                    label: format!("{label} added"),
                });
            }
            EventKind::AutoBurnSet { on } => {
                let label = if *on { "auto-burn on" } else { "auto-burn off" };
                marks.push(Mark {
                    at,
                    label: label.to_string(),
                });
            }
            _ => {}
        }
    }

    let weeks: Vec<OverviewWeek> = weeks_by_start.into_values().collect();
    let mut findings = Vec::new();
    if let Some(text) = finding_expired(ctx, &kpis, &priced_weeks, currency.as_deref()) {
        findings.push(text);
    }
    if let Some(text) = finding_blocked(&kpis, &weeks) {
        findings.push(text);
    }
    if let Some(text) = finding_auto_burn(ctx, range, &events.records, &weeks, &money) {
        findings.push(text);
    }
    if let Some(text) = finding_full(ctx, &finished) {
        findings.push(text);
    }

    Ok(Overview {
        currency,
        without_price,
        kpis,
        weeks,
        marks,
        findings,
    })
}

/// The range the token and sample reads cover: the range itself, the 14 days
/// the rates are learned over before its end, and every account week's window.
fn reading_range(range: &Range, finished: &[AccountWeek]) -> Range {
    let mut span = Range {
        from: range.from.min(range.to - Duration::days(RATES_DAYS)),
        to: range.to,
    };
    for week in finished {
        span.from = span.from.min(week.window.weekly_start());
        span.to = span.to.max(week.window.end);
    }
    span
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
/// on this host. The rule is the Settings page's (`cost.wu × s`).
fn switch_cost_percent(
    ctx: &Ctx,
    range: &Range,
    events: &Read<Event>,
    samples: &Read<Sample>,
    tokens: &Read<TokenRecord>,
) -> Option<f64> {
    let mut learned: BTreeMap<AccountRef, Rates> = BTreeMap::new();
    for account in ctx.accounts {
        let reference = AccountRef::of(account);
        if let Some(rates) = rates::rates(
            &reference,
            &samples.records,
            &tokens.records,
            &events.records,
            range.to,
        ) {
            learned.insert(reference, rates);
        }
    }
    let cost = switch_cost(
        &events.records,
        &samples.records,
        &tokens.records,
        &learned,
        range,
    )?;
    let timelines = rates::timelines(&events.records);
    let active = timelines.get(ctx.history.host())?.at(range.to)?;
    let window = learned.get(active)?.s?;
    Some(cost.wu * window)
}

/// The first finding: the account with the most expired money, if above zero;
/// no money text without a currency.
fn finding_expired(
    ctx: &Ctx,
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

/// The second finding: the blocked time, over the weeks the page shows.
fn finding_blocked(kpis: &Kpis, weeks: &[OverviewWeek]) -> Option<String> {
    if kpis.blocked_hours <= 0.0 || weeks.is_empty() {
        return None;
    }
    let noun = if weeks.len() == 1 { "week" } else { "weeks" };
    Some(format!(
        "You were blocked {} in {} {noun}; {} of them while other quota expired.",
        hours_text(kpis.blocked_hours),
        weeks.len(),
        hours_text(kpis.avoidable_hours)
    ))
}

/// The third finding: since auto-burn was switched on and stayed on, with a
/// full week on each side. Its expired share needs priced weeks, not a
/// currency of its own.
fn finding_auto_burn(
    ctx: &Ctx,
    range: &Range,
    events: &[Hosted<Event>],
    weeks: &[OverviewWeek],
    money: &BTreeMap<DateTime<Utc>, WindowMoney>,
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
    let (blocked_before, expired_before) = week_averages(&before, money)?;
    let (blocked_after, expired_after) = week_averages(&after, money)?;
    Some(format!(
        "Since auto-burn ({}): {} blocked a week, {} expired. Before: {}, {}.",
        date_text(at, ctx.offset),
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
/// week list is empty or none of its weeks has a price.
fn week_averages(
    weeks: &[&OverviewWeek],
    money: &BTreeMap<DateTime<Utc>, WindowMoney>,
) -> Option<(f64, f64)> {
    if weeks.is_empty() {
        return None;
    }
    let mut blocked = 0.0;
    let mut paid = 0.0;
    let mut expired = 0.0;
    for week in weeks {
        blocked += week.blocked_hours;
        if let Some(week_money) = money.get(&week.week) {
            paid += week_money.paid;
            expired += week_money.expired;
        }
    }
    if paid <= 0.0 {
        return None;
    }
    Some((blocked / weeks.len() as f64, expired / paid * 100.0))
}

/// 4. The account with the most full weeks.
fn finding_full(ctx: &Ctx, finished: &[AccountWeek]) -> Option<String> {
    let mut fullest: Option<(&crate::store::Account, u32, u32)> = None;
    for account in ctx.accounts {
        let reference = AccountRef::of(account);
        let total = finished
            .iter()
            .filter(|week| week.account == reference)
            .count() as u32;
        let full = finished
            .iter()
            .filter(|week| week.account == reference && week.window.final_percent >= 100.0)
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

    use crate::stats::tests::{all, tiny, utc};
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
        assert_eq!(page.kpis.blocked_hours, 2.0);
        assert_eq!(page.kpis.avoidable_hours, 2.0);
        assert_eq!(page.kpis.switches.user, 1);
        assert_eq!(page.kpis.switches.cli, 0);
        assert_eq!(page.kpis.switches.auto, 1);
        assert_eq!(page.kpis.switches.agent, 1);
        assert_eq!(page.kpis.switches.external, 0);
        assert_eq!(page.currency.as_deref(), Some("EUR"));
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
                == "You were blocked 2 h in 2 weeks; 2 h of them while other quota expired."),
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
                    plan: Some("Team 5x".to_string()),
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
        assert_eq!(page.kpis.blocked_hours, 0.0);
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
            close(shown, page.kpis.blocked_hours),
            "the weeks show {shown} h, the page says {} h",
            page.kpis.blocked_hours
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
            page.kpis.blocked_hours.is_sign_positive(),
            "blocked hours {}",
            page.kpis.blocked_hours
        );
        assert!(
            page.kpis.avoidable_hours.is_sign_positive(),
            "avoidable hours {}",
            page.kpis.avoidable_hours
        );
    }
}
