//! The Projects page (spec §3.6): the tree of organizations, projects and
//! folders, the organization × subscription matrix with its sentences, the
//! tokens per percent of each account, and one project's detail. Everything
//! here only reads.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use crate::history::{AccountRef, Hosted, Hosts, Range, TokenRecord};
use crate::live_sessions::Values;
use crate::money::{usd_rate, Company, Fx, Missing, Valuer};
use crate::project::ProjectKey;
use crate::projects::{Project, ProjectId};
use crate::store::{Account, Settings};
use crate::transcripts::Tokens;

use super::{
    account_abbr, account_chosen, account_id, account_weeks_valued, add_opt_value, add_value,
    full_half, join_names, keep_warnings, money_text, output_by_account_week, percent_text,
    range_of, record_chosen, subscription_shares, thousands, week_start, windows_span, Ctx, Query,
    StatsError,
};

/// The level of a row of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    Org,
    Project,
    Folder,
}

/// One row of the tree table.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub kind: NodeKind,
    pub level: u8,
    pub name: String,
    /// The project's key on project rows; `None` on organization and folder
    /// rows.
    pub key: Option<ProjectKey>,
    /// The organization's account ids on organization rows; empty otherwise.
    pub org_accounts: Vec<String>,
    pub tokens: Tokens,
    /// This node's API value as a share of the page's API value, in %; `None`
    /// when the node's own value or the page's total is missing (an unknown
    /// share is not a measured zero), or when the page's total is zero.
    pub share: Option<f64>,
    /// The two values of the node's records, built with [`Values::of`]: a
    /// missing half names its reason, and the API half names its currency.
    pub values: Values,
    /// Each subscription's share of this node's API value, in %, largest first;
    /// ties in the chips' order ([`account_position`]). The id is a stored
    /// Claude account's id ([`account_id`]) or a provider subscription's
    /// `account.acc`; `None` is work without a known subscription (a Claude
    /// record without an account, a provider record without a named
    /// subscription). Empty when the node's API value is unknown, 0 or partial
    /// ([`full_half`]); a part whose API half is partial or unknown is left out.
    pub by_subscription: Vec<(Option<String>, f64)>,
    /// The distinct sessions of the node's records.
    pub sessions: u32,
}

/// The Projects page of one range.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsPage {
    /// Tree order: an organization, its projects, their folders, then the next
    /// organization.
    pub nodes: Vec<Node>,
    pub matrix: Matrix,
    /// The output tokens per percent of a finished account week, per chosen
    /// account; an account without a finished week of the range is left out.
    pub tokens_per_percent: Vec<(String, f64)>,
    /// The tokens of the Claude records whose account is unknown; a record of
    /// another provider belongs to its provider subscription and is not
    /// counted here.
    pub unknown_account_tokens: Tokens,
    /// The distinct models whose API value is missing because they have no
    /// price, sorted.
    pub without_price: Vec<String>,
    /// What the page says is not in the tree, in the order the plan gives it.
    pub notes: Vec<String>,
    /// The dollar rate the page's API values are converted with, as
    /// `{ "value": 0.9048, "date": "2026-11-13", "currency": "EUR",
    /// "varies": false }`; `null` when the API values are already in USD or no
    /// rate of the settings' currency exists.
    pub usd_rate: Option<UsdRate>,
}

/// The dollar rate of the page's API values: units of the rate's currency per
/// 1 USD, with the day the looked-up rate is from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsdRate {
    /// Units of [`UsdRate::currency`] per 1 USD, rounded to four decimals.
    pub value: f64,
    /// The day the looked-up rate is from.
    pub date: Option<NaiveDate>,
    /// The currency the rate converts into: the settings' currency the rate is
    /// for. The page writes this one, never another view's currency.
    pub currency: String,
    /// Whether the rate of the range's days is not one value all along: the
    /// valuer converts each record at its own day's rate, so the page says
    /// "each day's rate" when this is true.
    pub varies: bool,
}

/// The organization × subscription matrix.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Matrix {
    /// The organizations' shown names, in the tree's order.
    pub orgs: Vec<String>,
    /// The chosen Claude accounts in [`Ctx::accounts`]' order, then the chosen
    /// provider subscriptions in the chips' order (stored order), with the ids
    /// [`Node::by_subscription`] writes.
    pub subscriptions: Vec<String>,
    /// The subscription value per organization and subscription:
    /// `cells[org][subscription]`; `None` when a record of it has no subscription
    /// value (a provider subscription without a price), and a measured
    /// `Some(0.0)` when it has no record there.
    pub cells: Vec<Vec<Option<f64>>>,
    /// Each organization's cells added up over the subscriptions, as the
    /// [`Values::sum`] of their records: `row_totals[org]`. A cell without a
    /// value makes the sum partial (R3), never unknown.
    pub row_totals: Vec<Values>,
    /// Each subscription's cells added up over the organizations:
    /// `column_totals[subscription]`.
    pub column_totals: Vec<Values>,
    /// Every cell added up.
    pub total: Values,
    pub sentences: Vec<String>,
}

/// One project, all of its records of the range, independent of the host.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    pub name: String,
    pub tokens: Tokens,
    /// The agent answers as a share of all answers, in %; 0 when the records
    /// have no answers.
    pub agent_share: f64,
    /// Each host's share of the API value, in %, largest first.
    pub hosts: Vec<(String, f64)>,
    /// The two values of the project's records, built with [`Values::of`].
    pub values: Values,
    /// Each subscription's share of the project's API value, in %, by the same
    /// rule as [`Node::by_subscription`].
    pub by_subscription: Vec<(Option<String>, f64)>,
    /// The project's API value as a share of the page's API value, in %; `None`
    /// when the project's own value or the page's total is missing (an unknown
    /// share is not a measured zero), or when the page's total is zero.
    pub share: Option<f64>,
    /// The output tokens per calendar week and model, weeks oldest first and
    /// models by tokens.
    pub weekly_by_model: WeeklyByModel,
    pub sessions: u32,
}

/// The output tokens per calendar week and model.
type WeeklyByModel = Vec<(DateTime<Utc>, Vec<(String, u64)>)>;

/// The Projects page of the range the query names.
pub fn projects_page<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
) -> Result<ProjectsPage, StatsError> {
    let range = range_of(q.range, ctx)?;
    projects_page_in(ctx, &range, q)
}

/// The Projects page of `range`; the page's [`projects_page`] takes the range
/// from its [`Query`], the tests name one.
pub(crate) fn projects_page_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
) -> Result<ProjectsPage, StatsError> {
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
    let weeks = account_weeks_valued(ctx, range, q, &valuer)?;
    let read = keep_warnings(ctx.warnings, || {
        ctx.history.read::<TokenRecord>(range, &Hosts::All)
    })?;

    // The chosen records of the range, each valued once, under the project it
    // belongs to after the project log's joins.
    let mut entries: Vec<Entry> = Vec::new();
    let mut projects: BTreeMap<ProjectId, Project> = BTreeMap::new();
    let mut sums: BTreeMap<ProjectId, Sums> = BTreeMap::new();
    for hosted in &read.records {
        if let Some(host) = &q.host {
            if host != &hosted.host {
                continue;
            }
        }
        let value = valuer.value(&hosted.host, &hosted.record);
        if !record_chosen(ctx, q, &value) {
            continue;
        }
        let values = Values::of(std::slice::from_ref(&value), ctx.settings, ctx.fx, ctx.now);
        let project = ctx
            .projects
            .of(&ProjectId::of(&hosted.record.project, &hosted.host));
        let root = project.key.clone();
        projects.entry(root.clone()).or_insert(project);
        let entry = Entry {
            host: &hosted.host,
            record: &hosted.record,
            value,
            values,
            root: root.clone(),
        };
        sums.entry(root).or_default().add(&entry);
        entries.push(entry);
    }

    let page_values = {
        let parts: Vec<&Values> = entries.iter().map(|entry| &entry.values).collect();
        Values::sum(&parts)
    };

    // The organizations with records, by API value, the unassigned ones last.
    let mut org_members: BTreeMap<Option<String>, Vec<ProjectId>> = BTreeMap::new();
    for (key, project) in &projects {
        org_members
            .entry(project.org.clone())
            .or_default()
            .push(key.clone());
    }
    let mut orgs: Vec<OrgRow> = Vec::new();
    for (identity, members) in org_members {
        let mut org_sums = Sums::default();
        for member in &members {
            if let Some(project_sums) = sums.get(member) {
                org_sums.merge(project_sums);
            }
        }
        orgs.push(OrgRow {
            name: org_name(ctx, &identity),
            identity,
            members,
            sums: org_sums,
        });
    }
    orgs.sort_by(|a, b| {
        a.identity
            .is_none()
            .cmp(&b.identity.is_none())
            .then_with(|| api_order(a.sums.api(), b.sums.api()))
            .then_with(|| a.name.cmp(&b.name))
    });

    let chosen_accounts: Vec<&Account> = ctx
        .accounts
        .iter()
        .filter(|account| account_chosen(ctx, q, Some(&AccountRef::of(account))))
        .collect();
    // The matrix's columns: the chosen Claude accounts in the stored accounts'
    // order, then the chosen provider subscriptions in the chips' order.
    let mut chosen_subscriptions: Vec<AccountRef> = chosen_accounts
        .iter()
        .map(|account| AccountRef::of(account))
        .collect();
    chosen_subscriptions.extend(
        ctx.provider_subscriptions
            .iter()
            .filter(|subscription| account_chosen(ctx, q, Some(&subscription.account)))
            .map(|subscription| subscription.account.clone()),
    );
    let currency = ctx.settings.currency.as_deref().map(str::to_string);

    let mut nodes = Vec::new();
    let mut matrix_orgs = Vec::new();
    let mut cells = Vec::new();
    let mut row_totals = Vec::new();
    let mut column_values: Vec<Option<Values>> = vec![None; chosen_subscriptions.len()];
    let mut sentences = Vec::new();
    for org in &orgs {
        let org_accounts = match &org.identity {
            Some(identity) => ctx
                .projects
                .orgs
                .iter()
                .find(|known| &known.id == identity)
                .map(|known| {
                    known
                        .accounts
                        .iter()
                        .map(|account| account_id(ctx, account))
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        nodes.push(node_from(
            ctx,
            NodeLabel {
                kind: NodeKind::Org,
                name: org.name.clone(),
                key: None,
                org_accounts,
            },
            &org.sums,
            &page_values,
        ));
        matrix_orgs.push(org.name.clone());
        cells.push(org_cells(org, &chosen_subscriptions));
        // The row total is the sum of the records of its cells: every chosen
        // subscription that has records in this organization, partial halves
        // and their reasons kept (R3). A subscription with no record here adds
        // nothing, the same as its measured 0 cell.
        let mut row_parts: Vec<&Values> = Vec::new();
        for (column, subscription) in chosen_subscriptions.iter().enumerate() {
            if let Some(values) = org.sums.by_subscription.get(&Some(subscription.clone())) {
                row_parts.push(values);
                add_opt_value(&mut column_values[column], values);
            }
        }
        row_totals.push(Values::sum(&row_parts));
        sentences.extend(org_sentences(ctx, org, &orgs, currency.as_deref()));

        let mut members = org.members.clone();
        members.sort_by(|a, b| {
            api_order(api_of(&sums, a), api_of(&sums, b))
                .then_with(|| projects[a].name.cmp(&projects[b].name))
                .then_with(|| a.cmp(b))
        });
        for key in members {
            let project = &projects[&key];
            nodes.push(node_from(
                ctx,
                NodeLabel {
                    kind: NodeKind::Project,
                    name: project.name.clone(),
                    key: Some(project.key.key.clone()),
                    org_accounts: Vec::new(),
                },
                &sums[&key],
                &page_values,
            ));
            let total_folders: usize = project.folders.values().map(Vec::len).sum();
            if total_folders > 1 {
                let mut folders: BTreeMap<String, Sums> = BTreeMap::new();
                for entry in entries.iter().filter(|entry| entry.root == key) {
                    let name = folder_name(project, entry.host, &entry.record.cwd);
                    folders.entry(name).or_default().add(entry);
                }
                let mut names: Vec<String> = folders.keys().cloned().collect();
                names.sort_by(|a, b| {
                    api_order(folders[a].api(), folders[b].api()).then_with(|| a.cmp(b))
                });
                for name in names {
                    nodes.push(node_from(
                        ctx,
                        NodeLabel {
                            kind: NodeKind::Folder,
                            name: name.clone(),
                            key: None,
                            org_accounts: Vec::new(),
                        },
                        &folders[&name],
                        &page_values,
                    ));
                }
            }
        }
    }

    // The ratios divide each window by its own span, which can reach past the
    // range; the reads that serve them cover every chosen window whole.
    let week_span = windows_span(&weeks, range);
    let week_tokens = keep_warnings(ctx.warnings, || {
        ctx.history.read::<TokenRecord>(&week_span, &Hosts::All)
    })?;
    let mut week_records: Vec<(AccountRef, &TokenRecord)> = Vec::new();
    for hosted in &week_tokens.records {
        if q.host.as_ref().is_some_and(|host| host != &hosted.host) {
            continue;
        }
        let value = valuer.value(&hosted.host, &hosted.record);
        let Some(account) = value.account else {
            continue;
        };
        if !account_chosen(ctx, q, Some(&account)) {
            continue;
        }
        week_records.push((account, &hosted.record));
    }
    let output_by_week = output_by_account_week(
        &weeks,
        week_records
            .iter()
            .map(|(account, record)| (account, *record)),
    );
    let mut tokens_per_percent = Vec::new();
    for account in &chosen_accounts {
        let reference = AccountRef::of(account);
        let account_weeks: Vec<_> = weeks
            .iter()
            .filter(|week| week.account == reference)
            .collect();
        let percent: f64 = account_weeks.iter().map(|week| week.used).sum();
        if account_weeks.is_empty() || percent <= 0.0 {
            continue;
        }
        let out: u64 = output_by_week
            .iter()
            .filter(|((week_account, _), _)| week_account == &reference)
            .map(|(_, out)| *out)
            .sum();
        tokens_per_percent.push((account.id.clone(), out as f64 / percent));
    }

    let mut unknown_account_tokens = Tokens::default();
    let mut readings_tokens = Tokens::default();
    let mut has_readings = false;
    for entry in &entries {
        if entry.value.company == Company::Anthropic && entry.value.account.is_none() {
            unknown_account_tokens += Tokens::from(entry.record);
        }
        if entry.value.account_from_readings {
            has_readings = true;
            readings_tokens += Tokens::from(entry.record);
        }
    }

    let mut without_price = BTreeSet::new();
    for entry in &entries {
        if let Some(Missing::NoPrice { model }) = &entry.value.api_missing {
            without_price.insert(model.clone());
        }
    }

    let mut notes = vec![NO_TRANSCRIPT.to_string()];
    let unknown_total = unknown_account_tokens.input
        + unknown_account_tokens.out
        + unknown_account_tokens.cw
        + unknown_account_tokens.cr;
    if unknown_total != 0 {
        notes.push(format!(
            "{} tokens have no known account.",
            thousands(unknown_total as i64)
        ));
    }
    // R4: a record whose account came from the usage readings counts as that
    // account everywhere on the page; only this note names the readings.
    if has_readings {
        let readings_total =
            readings_tokens.input + readings_tokens.out + readings_tokens.cw + readings_tokens.cr;
        notes.push(format!(
            "{} tokens: account taken from the usage readings.",
            thousands(readings_total as i64)
        ));
    }
    // A record with a known account can still have no subscription value (a
    // Claude week without a reading, a provider subscription without a price);
    // the page says how many tokens that leaves out and why.
    let mut without_subscription = Tokens::default();
    let mut reasons = BTreeSet::new();
    let mut has_unvalued = false;
    for entry in &entries {
        if entry.value.account.is_some() && entry.value.subscription.is_none() {
            has_unvalued = true;
            without_subscription += Tokens::from(entry.record);
            if let Some(missing) = &entry.value.subscription_missing {
                reasons.insert(missing.to_string());
            }
        }
    }
    if has_unvalued {
        let total = without_subscription.input
            + without_subscription.out
            + without_subscription.cw
            + without_subscription.cr;
        notes.push(format!(
            "{} tokens have no subscription value: {}.",
            thousands(total as i64),
            reasons.into_iter().collect::<Vec<String>>().join("; ")
        ));
    }

    let column_totals: Vec<Values> = column_values
        .into_iter()
        .map(|values| values.unwrap_or_else(|| Values::sum(&[])))
        .collect();
    let total = {
        let parts: Vec<&Values> = row_totals.iter().collect();
        Values::sum(&parts)
    };
    Ok(ProjectsPage {
        nodes,
        matrix: Matrix {
            orgs: matrix_orgs,
            subscriptions: chosen_subscriptions
                .iter()
                .map(|subscription| account_id(ctx, subscription))
                .collect(),
            row_totals,
            column_totals,
            total,
            cells,
            sentences,
        },
        tokens_per_percent,
        unknown_account_tokens,
        without_price: without_price.into_iter().collect(),
        notes,
        usd_rate: page_usd_rate(ctx, range),
    })
}

/// The dollar rate the page's API values are in, at the range's end: from the
/// same `settings` and `fx` the page's [`Valuer`] is built from, through
/// [`crate::money::usd_rate`] — the function that turns every record's USD value
/// into the settings' currency. `None` when the API values are already in USD,
/// or when the settings name no currency or no rate of it.
fn page_usd_rate<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, range: &Range) -> Option<UsdRate> {
    let currency = ctx.settings.currency.as_deref()?;
    if currency == "USD" {
        return None;
    }
    let rate = usd_rate(ctx.settings, ctx.fx, last_day(range))?;
    Some(UsdRate {
        value: round_rate(rate.value),
        date: rate.date,
        currency: currency.to_string(),
        varies: rate_varies(ctx.settings, ctx.fx, range),
    })
}

/// A rate as the page writes it: four decimals, rounded in Rust so the page
/// never adds a rounding of its own.
fn round_rate(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Whether the rate of the range's days is not one value all along. Every day
/// asks [`crate::money::usd_rate`] — the same source the valuer asks — so a
/// looked-up rate changes when the days do not all use the same `fx` point.
/// Days are `range.from` .. `range.to` (the range's end is exclusive), so a
/// point on the end's midnight is not in the range.
fn rate_varies(settings: &Settings, fx: &Fx, range: &Range) -> bool {
    let from = range.from.date_naive();
    let to = last_day(range);
    let Some(first) = usd_rate(settings, fx, from) else {
        return false;
    };
    let mut day = from;
    loop {
        match usd_rate(settings, fx, day) {
            Some(rate) if rate.value != first.value => return true,
            Some(_) => {}
            None => return false,
        }
        if day >= to {
            return false;
        }
        let Some(next) = day.succ_opt() else {
            return false;
        };
        day = next;
    }
}

/// The last calendar day a record of `range` can have: `to` itself, or the day
/// before it when `to` is its midnight (the range's end is exclusive).
fn last_day(range: &Range) -> NaiveDate {
    let day = range.to.date_naive();
    if range.to.time() == chrono::NaiveTime::MIN {
        day.pred_opt().unwrap_or(day)
    } else {
        day
    }
}

/// The detail of the project `key`, over all hosts: a folder key is per host,
/// so every host's folder of that name counts.
pub fn project_detail<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    q: &Query,
    key: &ProjectKey,
) -> Result<ProjectDetail, StatsError> {
    let range = range_of(q.range, ctx)?;
    project_detail_in(ctx, &range, q, key)
}

/// The detail of `key` in `range`; the page's [`project_detail`] takes the
/// range from its [`Query`], the tests name one.
pub(crate) fn project_detail_in<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    range: &Range,
    q: &Query,
    key: &ProjectKey,
) -> Result<ProjectDetail, StatsError> {
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
    let read = keep_warnings(ctx.warnings, || {
        ctx.history.read::<TokenRecord>(range, &Hosts::All)
    })?;
    // The records of the chosen host, and among them the project's own.
    let in_host =
        |hosted: &&Hosted<TokenRecord>| q.host.as_ref().is_none_or(|host| host == &hosted.host);
    let in_project = |hosted: &&Hosted<TokenRecord>| {
        let detail = ctx.projects.of(&ProjectId::of(key, &hosted.host));
        let record = ctx
            .projects
            .of(&ProjectId::of(&hosted.record.project, &hosted.host));
        record.key == detail.key
    };
    let matching: Vec<_> = read
        .records
        .iter()
        .filter(in_host)
        .filter(in_project)
        .collect();
    let known = ctx.projects.projects.iter().any(|project| {
        &project.key.key == key || project.joined.iter().any(|joined| &joined.key.key == key)
    });
    if matching.is_empty() && !known {
        return Err(StatsError::Message(format!(
            "unknown project {}",
            key_text(key)
        )));
    }

    let name = match matching.first() {
        Some(hosted) => ctx.projects.of(&ProjectId::of(key, &hosted.host)).name,
        None => ctx
            .projects
            .projects
            .iter()
            .find(|project| {
                &project.key.key == key
                    || project.joined.iter().any(|joined| &joined.key.key == key)
            })
            .map(|project| project.name.clone())
            .unwrap_or_else(|| {
                ctx.projects
                    .of(&ProjectId::of(key, ctx.history.host()))
                    .name
            }),
    };

    let mut sums = Sums::default();
    // The page's whole API value, over the same host and account choice, so the
    // project's share is its part of all use and not of another span.
    let mut page_values: Option<Values> = None;
    let mut answers = 0u64;
    let mut agent_answers = 0u64;
    let mut hosts: BTreeMap<String, Values> = BTreeMap::new();
    let mut weekly: BTreeMap<DateTime<Utc>, BTreeMap<String, u64>> = BTreeMap::new();
    for hosted in read.records.iter().filter(in_host) {
        let value = valuer.value(&hosted.host, &hosted.record);
        if !record_chosen(ctx, q, &value) {
            continue;
        }
        let values = Values::of(std::slice::from_ref(&value), ctx.settings, ctx.fx, ctx.now);
        add_opt_value(&mut page_values, &values);
        if !in_project(&hosted) {
            continue;
        }
        let entry = Entry {
            host: &hosted.host,
            record: &hosted.record,
            value,
            values,
            root: ProjectId::of(&hosted.record.project, &hosted.host),
        };
        sums.add(&entry);
        answers += u64::from(hosted.record.answers);
        agent_answers += u64::from(hosted.record.agent_answers);
        add_value(&mut hosts, hosted.host.clone(), &entry.values);
        *weekly
            .entry(week_start(hosted.record.t, &ctx.zone))
            .or_default()
            .entry(hosted.record.model.clone())
            .or_insert(0) += hosted.record.out;
    }

    let host_values = {
        let parts: Vec<&Values> = hosts.values().collect();
        Values::sum(&parts)
    };
    // A host's share divides the host's value by all hosts' value; both must be
    // full halves (R3), so a partial one leaves the share out.
    let mut host_shares: Vec<(String, f64)> = hosts
        .iter()
        .filter_map(|(host, values)| {
            share_of(values, Some(&host_values)).map(|share| (host.clone(), share))
        })
        .collect();
    host_shares.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let weekly_by_model: WeeklyByModel = weekly
        .into_iter()
        .map(|(week, models)| {
            let mut models: Vec<(String, u64)> = models.into_iter().collect();
            models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            (week, models)
        })
        .collect();

    let values = sums.values();
    let by_subscription = subscription_shares(
        ctx,
        &values,
        sums.by_subscription.iter().map(|(subscription, values)| {
            (
                subscription
                    .as_ref()
                    .map(|subscription| account_id(ctx, subscription)),
                values,
            )
        }),
    );
    Ok(ProjectDetail {
        name,
        tokens: sums.tokens,
        agent_share: if answers > 0 {
            agent_answers as f64 / answers as f64 * 100.0
        } else {
            0.0
        },
        hosts: host_shares,
        values,
        by_subscription,
        share: share_of(&sums.values(), page_values.as_ref()),
        weekly_by_model,
        sessions: sums.sessions.len() as u32,
    })
}

/// One chosen token record of the page.
struct Entry<'a> {
    host: &'a str,
    record: &'a TokenRecord,
    /// The record's two values, from [`Values::of`].
    value: crate::money::Valued,
    values: Values,
    /// The project the record belongs to after the log's joins.
    root: ProjectId,
}

/// The sums of some records: every half is a [`Values`] sum, so a missing part
/// makes it unknown with its reason and two currencies are never added.
#[derive(Default)]
struct Sums {
    tokens: Tokens,
    values: Option<Values>,
    /// The records' two values by subscription: a Claude account, a provider
    /// subscription, or `None` for work without a known subscription. The API
    /// shares and the subscription values are the same accumulation.
    by_subscription: BTreeMap<Option<AccountRef>, Values>,
    sessions: BTreeSet<String>,
}

impl Sums {
    fn add(&mut self, entry: &Entry) {
        let record = entry.record;
        self.tokens += Tokens::from(record);
        self.sessions.insert(record.session.clone());
        add_opt_value(&mut self.values, &entry.values);
        add_value(
            &mut self.by_subscription,
            entry.value.account.clone(),
            &entry.values,
        );
    }

    fn merge(&mut self, other: &Sums) {
        self.tokens += other.tokens;
        if let Some(values) = &other.values {
            add_opt_value(&mut self.values, values);
        }
        for (subscription, values) in &other.by_subscription {
            add_value(&mut self.by_subscription, subscription.clone(), values);
        }
        self.sessions.extend(other.sessions.iter().cloned());
    }

    fn values(&self) -> Values {
        match &self.values {
            Some(values) => values.clone(),
            None => Values::sum(&[]),
        }
    }

    fn api(&self) -> Option<f64> {
        self.values().api
    }
}

/// One organization of the tree, with its projects and their sums.
struct OrgRow {
    identity: Option<String>,
    name: String,
    members: Vec<ProjectId>,
    sums: Sums,
}

/// What a node row is, before its numbers are added.
struct NodeLabel {
    kind: NodeKind,
    name: String,
    key: Option<ProjectKey>,
    org_accounts: Vec<String>,
}

fn node_from<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    label: NodeLabel,
    sums: &Sums,
    page: &Values,
) -> Node {
    let level = match label.kind {
        NodeKind::Org => 0,
        NodeKind::Project => 1,
        NodeKind::Folder => 2,
    };
    let values = sums.values();
    let by_subscription = subscription_shares(
        ctx,
        &values,
        sums.by_subscription.iter().map(|(subscription, values)| {
            (
                subscription
                    .as_ref()
                    .map(|subscription| account_id(ctx, subscription)),
                values,
            )
        }),
    );
    Node {
        kind: label.kind,
        level,
        name: label.name,
        key: label.key,
        org_accounts: label.org_accounts,
        tokens: sums.tokens,
        share: share_of(&values, Some(page)),
        values,
        by_subscription,
        sessions: sums.sessions.len() as u32,
    }
}

/// A value as a share of the page's total, in %: unknown when either value is
/// missing or the total is zero, never a made-up 0 %. A share, ratio or percent
/// is computed only from halves that are not partial (R3); a partial half is a
/// lower bound, so it gives no share.
fn share_of(values: &Values, page: Option<&Values>) -> Option<f64> {
    let (api, _) = full_half(
        values.api,
        values.api_partial,
        values.api_missing.as_deref(),
    );
    let (total, _) = page
        .map(|page| full_half(page.api, page.api_partial, page.api_missing.as_deref()))
        .unwrap_or((None, None));
    match (api, total) {
        (Some(api), Some(total)) if total > 0.0 => Some(api / total * 100.0),
        _ => None,
    }
}

/// The subscription value of the chosen subscriptions in one organization, in
/// the subscriptions' order; `None` when a record of it has no subscription
/// value, and a measured zero when it has no record there.
fn org_cells(org: &OrgRow, subscriptions: &[AccountRef]) -> Vec<Option<f64>> {
    subscriptions
        .iter()
        .map(
            |subscription| match org.sums.by_subscription.get(&Some(subscription.clone())) {
                Some(values) => {
                    full_half(
                        values.subscription,
                        values.subscription_partial,
                        values.subscription_missing.as_deref(),
                    )
                    .0
                }
                None => Some(0.0),
            },
        )
        .collect()
}

/// The sentences of one organization that has accounts of its own. A share,
/// ratio or percent is computed only from halves that are not partial (R3): a
/// sentence whose figure would come from a partial value is left out, as it was
/// before R3 when the value was missing.
fn org_sentences<Tz: TimeZone + Copy>(
    ctx: &Ctx<Tz>,
    org: &OrgRow,
    orgs: &[OrgRow],
    currency: Option<&str>,
) -> Vec<String> {
    let Some(identity) = &org.identity else {
        return Vec::new();
    };
    let Some(known) = ctx.projects.orgs.iter().find(|known| &known.id == identity) else {
        return Vec::new();
    };
    if known.accounts.is_empty() {
        return Vec::new();
    }
    let values = org.sums.values();
    let (total, _) = full_half(
        values.subscription,
        values.subscription_partial,
        values.subscription_missing.as_deref(),
    );
    let Some(total) = total else {
        return Vec::new();
    };
    let own: BTreeSet<AccountRef> = known.accounts.iter().cloned().collect();
    let own_parts: Vec<&Values> = own
        .iter()
        .filter_map(|account| org.sums.by_subscription.get(&Some(account.clone())))
        .collect();
    let own_sub = if own_parts.is_empty() {
        0.0
    } else {
        let sum = Values::sum(&own_parts);
        let (value, _) = full_half(
            sum.subscription,
            sum.subscription_partial,
            sum.subscription_missing.as_deref(),
        );
        match value {
            Some(value) => value,
            None => return Vec::new(),
        }
    };
    let share = if total > 0.0 {
        own_sub / total * 100.0
    } else {
        0.0
    };
    let abbrs: Vec<String> = known
        .accounts
        .iter()
        .map(|account| account_abbr(ctx, account))
        .collect();
    let names = join_names(&abbrs);
    let (verb, noun) = if known.accounts.len() == 1 {
        ("is", "subscription")
    } else {
        ("are", "subscriptions")
    };
    let mut sentences = vec![format!(
        "{names} {verb} {}'s {noun}. It covered {} of the work for {}.",
        org.name,
        percent_text(share),
        org.name
    )];

    if share < 100.0 {
        if let Some(currency) = currency {
            // The sentences speak of an organization's own Claude accounts only;
            // provider subscriptions have their own matrix columns.
            let others: Vec<(&AccountRef, &Values)> = org
                .sums
                .by_subscription
                .iter()
                .filter_map(|(account, values)| account.as_ref().map(|account| (account, values)))
                .filter(|(account, _)| account.is_claude() && !own.contains(*account))
                .collect();
            let other_parts: Vec<&Values> = others.iter().map(|(_, values)| *values).collect();
            let amount = if other_parts.is_empty() {
                Some(0.0)
            } else {
                let sum = Values::sum(&other_parts);
                full_half(
                    sum.subscription,
                    sum.subscription_partial,
                    sum.subscription_missing.as_deref(),
                )
                .0
            };
            if let Some(amount) = amount.filter(|amount| *amount > 0.0) {
                let mut others: Vec<(&AccountRef, f64)> = others
                    .iter()
                    .filter_map(|(account, values)| {
                        values
                            .subscription
                            .filter(|value| *value > 0.0)
                            .map(|value| (*account, value))
                    })
                    .collect();
                others.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
                let names: Vec<String> = others
                    .iter()
                    .map(|(account, _)| account_abbr(ctx, account))
                    .collect();
                sentences[0].push_str(&format!(
                    " The rest ran on {}: {} of their value.",
                    join_names(&names),
                    money_text(amount, currency)
                ));
            }
        }
    }

    if let Some(currency) = currency {
        let mut other_orgs: Vec<String> = Vec::new();
        let mut carried_parts: Vec<&Values> = Vec::new();
        let mut carried_partial = false;
        for other in orgs {
            if other.identity == org.identity {
                continue;
            }
            let parts: Vec<&Values> = own
                .iter()
                .filter_map(|account| other.sums.by_subscription.get(&Some(account.clone())))
                .collect();
            if parts.is_empty() {
                continue;
            }
            let sum = Values::sum(&parts);
            if sum.subscription_partial {
                carried_partial = true;
                continue;
            }
            let (value, _) = full_half(
                sum.subscription,
                sum.subscription_partial,
                sum.subscription_missing.as_deref(),
            );
            if value.is_some_and(|value| value > 0.0) {
                carried_parts.extend(parts);
                other_orgs.push(other.name.clone());
            }
        }
        let carried = if carried_partial || carried_parts.is_empty() {
            None
        } else {
            let sum = Values::sum(&carried_parts);
            full_half(
                sum.subscription,
                sum.subscription_partial,
                sum.subscription_missing.as_deref(),
            )
            .0
        };
        if let Some(carried) = carried.filter(|carried| *carried > 0.0) {
            if !other_orgs.is_empty() {
                sentences.push(format!(
                    "The other way round: {names} carried {} of work for {}.",
                    money_text(carried, currency),
                    join_names(&other_orgs)
                ));
            }
        }
    }
    sentences
}

/// The shown name of an organization identity; a project without one is under
/// `Unassigned`.
fn org_name<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, identity: &Option<String>) -> String {
    match identity {
        Some(identity) => ctx
            .projects
            .orgs
            .iter()
            .find(|org| &org.id == identity)
            .map(|org| org.name.clone())
            .unwrap_or_else(|| identity.clone()),
        None => "Unassigned".to_string(),
    }
}

fn api_of(sums: &BTreeMap<ProjectId, Sums>, key: &ProjectId) -> Option<f64> {
    sums.get(key).and_then(|sums| sums.api())
}

/// Largest API value first, a node without one last.
fn api_order(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.total_cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// What a record's folder is when it lies at its host's root.
const TOP_FOLDER: &str = "(top folder)";

/// The folder of `cwd` below the project's root on `host`: the first path part,
/// or [`TOP_FOLDER`].
pub(crate) fn folder_name(project: &Project, host: &str, cwd: &str) -> String {
    let folders = project.folders.get(host).map(Vec::as_slice).unwrap_or(&[]);
    let root = common_parent(folders);
    let parts = path_parts(cwd);
    if root.is_empty() || parts.len() <= root.len() || parts[..root.len()] != root[..] {
        return TOP_FOLDER.to_string();
    }
    parts[root.len()].to_string()
}

/// The common parent of some folders, as path parts.
fn common_parent(folders: &[String]) -> Vec<&str> {
    let mut folders = folders.iter().map(|folder| path_parts(folder));
    let Some(first) = folders.next() else {
        return Vec::new();
    };
    let mut common = first;
    for parts in folders {
        let shared = common
            .iter()
            .zip(&parts)
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(shared);
        if common.is_empty() {
            break;
        }
    }
    common
}

/// A path as its parts, for Unix and Windows folders.
fn path_parts(path: &str) -> Vec<&str> {
    path.split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect()
}

/// What the page says about use that leaves no transcript.
const NO_TRANSCRIPT: &str = "Use that leaves no transcript on a host with seatswap (claude.ai, the phone, Claude Desktop, a host without a shared history) is in the percent but in no project.";

fn key_text(key: &ProjectKey) -> String {
    match key {
        ProjectKey::Repo(repo) => format!("repository {repo}"),
        ProjectKey::Folder(folder) => format!("folder {folder}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    use crate::history::{Event, EventKind, HistoryDir, LimitSample, Sample, Source, WindowRecord};
    use crate::projects::{ProjectEdit, ProjectsLogRecord};
    use crate::stats::tests::{
        all, append_codex_records, codex_ref, provider_subscription, tiny, utc,
    };
    use crate::stats::Range;

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

    fn atlas() -> ProjectKey {
        ProjectKey::Repo("github.com/sam/atlas".to_string())
    }

    /// The project log line that makes AC acme's subscription.
    fn with_acme_account(tiny: &crate::stats::tests::Tiny) -> crate::projects::Projects {
        tiny.history
            .append(&ProjectsLogRecord {
                t: utc("2026-11-01T00:00:00Z"),
                edit: ProjectEdit::AccountOrg {
                    account: tiny.ac_ref(),
                    org: Some("acme".to_string()),
                },
            })
            .expect("the acme account link");
        let (projects, warnings) =
            crate::projects::load(&tiny.history, &tiny.accounts).expect("the projects");
        assert!(warnings.is_empty(), "project read warnings: {warnings:?}");
        projects
    }

    /// A token record the test adds to the fixture's history.
    fn record(
        t: &str,
        session: &str,
        cwd: &str,
        project: ProjectKey,
        model: &str,
        out: u64,
    ) -> TokenRecord {
        TokenRecord {
            t: utc(t),
            minutes: 60,
            session: session.to_string(),
            cwd: cwd.to_string(),
            project,
            model: model.to_string(),
            input: 0,
            out,
            cw: 0,
            cr: 0,
            answers: 1,
            agent_answers: 0,
        }
    }

    fn node<'a>(page: &'a ProjectsPage, name: &str) -> &'a Node {
        page.nodes
            .iter()
            .find(|node| node.name == name)
            .unwrap_or_else(|| panic!("the node {name}"))
    }

    #[test]
    fn the_tree() {
        let tiny = tiny();
        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let names: Vec<&str> = page.nodes.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["sam", "atlas", "engine", "frontend", "acme", "acme-kb"]
        );
        assert_eq!(page.nodes[0].kind, NodeKind::Org);
        assert_eq!(page.nodes[0].level, 0);
        let acme = node(&page, "acme");
        assert_eq!(acme.kind, NodeKind::Org);
        assert_eq!(acme.level, 0);

        let atlas = node(&page, "atlas");
        assert_eq!(atlas.kind, NodeKind::Project);
        assert_eq!(atlas.level, 1);
        assert_eq!(atlas.key, Some(self::atlas()));
        assert_eq!(atlas.values.api, Some(120.0));
        assert!(close(atlas.values.subscription.unwrap(), 103.846));
        assert!(close(atlas.share.unwrap(), 85.714));
        assert_eq!(atlas.sessions, 3);

        let engine = node(&page, "engine");
        assert_eq!(engine.kind, NodeKind::Folder);
        assert_eq!(engine.level, 2);
        assert_eq!(engine.key, None);
        assert_eq!(engine.values.api, Some(60.0));
        let frontend = node(&page, "frontend");
        assert_eq!(frontend.values.api, Some(60.0));

        let acme_kb = node(&page, "acme-kb");
        assert_eq!(acme_kb.kind, NodeKind::Project);
        assert_eq!(acme_kb.level, 1);
        assert!(close(acme_kb.values.subscription.unwrap(), 16.154));
        assert_eq!(acme_kb.sessions, 1);
    }

    #[test]
    fn by_subscription_of_atlas() {
        let tiny = tiny();
        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let atlas = node(&page, "atlas");
        assert_eq!(atlas.by_subscription.len(), 2);
        let lb = atlas
            .by_subscription
            .iter()
            .find(|(account, _)| account.as_deref() == Some(&tiny.lb().id))
            .expect("LB's share");
        let ac = atlas
            .by_subscription
            .iter()
            .find(|(account, _)| account.as_deref() == Some(&tiny.ac().id))
            .expect("AC's share");
        assert!(close(lb.1, 50.0), "LB {}", lb.1);
        assert!(close(ac.1, 50.0), "AC {}", ac.1);
    }

    #[test]
    fn the_matrix_subscription_columns() {
        let tiny = tiny();
        let projects = with_acme_account(&tiny);
        let ctx = Ctx {
            projects: &projects,
            ..tiny.ctx()
        };
        let page = projects_page_in(&ctx, &two_weeks(), &all()).unwrap();

        let matrix = &page.matrix;
        assert_eq!(matrix.orgs, vec!["sam", "acme"]);
        assert_eq!(
            matrix.subscriptions,
            vec![tiny.lb().id.clone(), tiny.ac().id.clone()]
        );
        let cell = |org: &str, subscription: &str| -> Option<f64> {
            let org = matrix
                .orgs
                .iter()
                .position(|name| name == org)
                .expect("the organization");
            let subscription = matrix
                .subscriptions
                .iter()
                .position(|id| id == subscription)
                .expect("the subscription");
            matrix.cells[org][subscription]
        };
        assert!(close(
            cell("sam", &tiny.lb().id).expect("LB's cell"),
            74.769
        ));
        assert!(close(
            cell("sam", &tiny.ac().id).expect("AC's cell"),
            29.077
        ));
        assert!(close(
            cell("acme", &tiny.ac().id).expect("AC's acme cell"),
            16.154
        ));
        assert_eq!(cell("acme", &tiny.lb().id), Some(0.0));
    }

    #[test]
    fn the_matrix_totals_are_the_sums_of_its_cells() {
        let tiny = tiny();
        let projects = with_acme_account(&tiny);
        let ctx = Ctx {
            projects: &projects,
            ..tiny.ctx()
        };
        let page = projects_page_in(&ctx, &two_weeks(), &all()).unwrap();
        let matrix = &page.matrix;

        let sum_of = |cells: &[Option<f64>]| -> f64 {
            cells
                .iter()
                .map(|cell| cell.expect("the fixture's cells all have a value"))
                .sum()
        };
        let org = |name: &str| {
            matrix
                .orgs
                .iter()
                .position(|org| org == name)
                .expect("the organization")
        };
        let subscription = |id: &str| {
            matrix
                .subscriptions
                .iter()
                .position(|subscription| subscription == id)
                .expect("the subscription")
        };

        for (row, total) in matrix.cells.iter().zip(&matrix.row_totals) {
            assert!(close(total.subscription.unwrap(), sum_of(row)));
            assert!(
                !total.subscription_partial,
                "every cell of the row is known"
            );
        }
        for column in 0..matrix.subscriptions.len() {
            let cells: Vec<Option<f64>> = matrix.cells.iter().map(|row| row[column]).collect();
            assert!(close(
                matrix.column_totals[column].subscription.unwrap(),
                sum_of(&cells)
            ));
        }
        let all: Vec<Option<f64>> = matrix.cells.iter().flatten().copied().collect();
        assert!(close(matrix.total.subscription.unwrap(), sum_of(&all)));

        // The fixture's values: sam 74.769 + 29.077, acme 16.154; LB 74.769,
        // AC 29.077 + 16.154, and every cell 120 EUR together.
        assert!(close(
            matrix.row_totals[org("sam")].subscription.unwrap(),
            74.769 + 29.077
        ));
        assert!(close(
            matrix.row_totals[org("acme")].subscription.unwrap(),
            16.154
        ));
        assert!(close(
            matrix.column_totals[subscription(&tiny.lb().id)]
                .subscription
                .unwrap(),
            74.769
        ));
        assert!(close(
            matrix.column_totals[subscription(&tiny.ac().id)]
                .subscription
                .unwrap(),
            29.077 + 16.154
        ));
        assert!(close(matrix.total.subscription.unwrap(), 120.0));

        // The page reads these keys: `rowTotals`, `columnTotals` and `total`,
        // each a `Values` (J-4 draws them with `valueNode`).
        let json = serde_json::to_value(&page).expect("the page's JSON shape");
        assert!(json["matrix"]["rowTotals"].is_array());
        assert!(json["matrix"]["rowTotals"][0].is_object());
        assert!(json["matrix"]["columnTotals"].is_array());
        assert!(json["matrix"]["columnTotals"][0].is_object());
        assert!(json["matrix"]["total"].is_object());
    }

    #[test]
    fn the_matrix_sentences() {
        let tiny = tiny();
        let projects = with_acme_account(&tiny);
        let ctx = Ctx {
            projects: &projects,
            ..tiny.ctx()
        };
        let page = projects_page_in(&ctx, &two_weeks(), &all()).unwrap();

        assert_eq!(
            page.matrix.sentences,
            vec![
                "AC is acme's subscription. It covered 100 % of the work for acme.".to_string(),
                "The other way round: AC carried 29 EUR of work for sam.".to_string(),
            ]
        );
    }

    #[test]
    fn tokens_per_percent() {
        let tiny = tiny();
        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let lb = page
            .tokens_per_percent
            .iter()
            .find(|(id, _)| id == &tiny.lb().id)
            .expect("LB's tokens per percent");
        assert!(close(lb.1, 3_000_000.0 / 180.0), "LB {}", lb.1);
    }

    #[test]
    fn the_usd_rate_is_the_pages_rate_with_its_date() {
        let mut tiny = tiny();
        let date = NaiveDate::from_ymd_opt(2026, 11, 13).expect("the rate's date");
        tiny.fx.take(date, 0.9048);

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(
            page.usd_rate,
            Some(UsdRate {
                value: 0.9048,
                date: Some(date),
                currency: "EUR".to_string(),
                // The fixture's 1:1 point of 1 Jan is another rate in this range.
                varies: true,
            })
        );
        // The page reads `usdRate.value`, `usdRate.date`, `usdRate.currency` and
        // `usdRate.varies`.
        let json = serde_json::to_value(&page).expect("the page's JSON shape");
        assert_eq!(
            json["usdRate"],
            serde_json::json!({
                "value": 0.9048,
                "date": "2026-11-13",
                "currency": "EUR",
                // The fixture's 1:1 point of 1 Jan is another rate in this range.
                "varies": true,
            })
        );
    }

    #[test]
    fn tokens_without_an_account_count_for_the_project_only() {
        let tiny = tiny();
        let engine = "/home/sam/projects/atlas/engine";
        tiny.history
            .append(&record(
                "2026-11-01T09:00:00Z",
                "s0",
                engine,
                atlas(),
                "claude-opus-5-5",
                1_000_000,
            ))
            .expect("a record before the first live event");
        let range = Range {
            from: utc("2026-11-01T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };

        let page = projects_page_in(&tiny.ctx(), &range, &all()).unwrap();
        let atlas = node(&page, "atlas");
        assert_eq!(atlas.tokens.out, 7_000_000);
        assert!(
            atlas.values.subscription.is_some(),
            "the known part of the subscription stays: {:?}",
            atlas.values
        );
        assert!(
            atlas.values.subscription_partial,
            "the record without an account makes the subscription sum partial"
        );
        assert_eq!(
            atlas.values.subscription_missing.as_deref(),
            Some("no account was in use at that time")
        );
        assert_eq!(page.unknown_account_tokens.out, 1_000_000);
        assert!(
            page.notes
                .iter()
                .any(|note| note == "1,000,000 tokens have no known account."),
            "notes: {:#?}",
            page.notes
        );
    }

    #[test]
    fn a_model_without_a_price_is_named() {
        let tiny = tiny();
        let engine = "/home/sam/projects/atlas/engine";
        tiny.history
            .append(&record(
                "2026-11-15T09:00:00Z",
                "s5",
                engine,
                atlas(),
                "claude-x",
                1_000_000,
            ))
            .expect("a record without a price");

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert_eq!(page.without_price, vec!["claude-x".to_string()]);
        let atlas = node(&page, "atlas");
        assert_eq!(atlas.tokens.out, 7_000_000);
        assert!(
            atlas.values.api.is_some(),
            "the priced records' known part stays: {:?}",
            atlas.values
        );
        assert!(
            atlas.values.api_partial,
            "one record without a price makes the sum partial"
        );
        assert_eq!(
            atlas.values.api_missing.as_deref(),
            Some("no price for claude-x")
        );
        assert!(
            atlas.values.subscription.is_some(),
            "the priced records' known part stays"
        );
        assert!(
            atlas.values.subscription_partial,
            "the unpriced record's week has no subscription value either"
        );
        assert!(
            page.notes.iter().any(|note| note
                == "4,000,000 tokens have no subscription value: no price for claude-x; that week also has tokens of claude-x, which has no price."),
            "notes: {:#?}",
            page.notes
        );
    }

    #[test]
    fn the_detail_of_atlas() {
        let tiny = tiny();
        let detail = project_detail_in(&tiny.ctx(), &two_weeks(), &all(), &atlas()).unwrap();

        assert_eq!(detail.name, "atlas");
        assert_eq!(detail.tokens.out, 6_000_000);
        assert_eq!(detail.hosts, vec![("atlas".to_string(), 100.0)]);
        assert_eq!(detail.sessions, 3);
        assert!(close(detail.values.subscription.unwrap(), 103.846));
        assert_eq!(detail.values.api, Some(120.0));

        assert_eq!(detail.weekly_by_model.len(), 2);
        assert_eq!(
            detail.weekly_by_model[0],
            (
                utc("2026-11-02T00:00:00Z"),
                vec![("claude-opus-5-5".to_string(), 2_000_000)]
            )
        );
        assert_eq!(
            detail.weekly_by_model[1],
            (
                utc("2026-11-09T00:00:00Z"),
                vec![("claude-opus-5-5".to_string(), 4_000_000)]
            )
        );
    }

    #[test]
    fn a_projects_share_is_its_part_of_all_use() {
        let tiny = tiny();
        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let all_use: f64 = page
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Project)
            .filter_map(|node| node.values.api)
            .sum();

        let atlas_detail = project_detail_in(&tiny.ctx(), &two_weeks(), &all(), &atlas()).unwrap();
        let atlas_share = atlas_detail.values.api.unwrap() / all_use * 100.0;
        assert!(
            close(atlas_detail.share.unwrap(), atlas_share),
            "atlas {:?} is not {} of all use",
            atlas_detail.share,
            atlas_share
        );
        assert!(close(atlas_detail.share.unwrap(), 120.0 / 140.0 * 100.0));
        assert_eq!(
            atlas_detail.share,
            node(&page, "atlas").share,
            "the detail shows the tree's share of all use"
        );

        let acme_kb = ProjectKey::Repo("github.com/acme/acme-kb".to_string());
        let kb_detail = project_detail_in(&tiny.ctx(), &two_weeks(), &all(), &acme_kb).unwrap();
        let kb_share = kb_detail.values.api.unwrap() / all_use * 100.0;
        assert!(close(kb_detail.share.unwrap(), kb_share));
        assert!(close(kb_detail.share.unwrap(), 20.0 / 140.0 * 100.0));
    }

    #[test]
    fn an_unknown_project_is_an_error() {
        let tiny = tiny();
        let key = ProjectKey::Repo("github.com/sam/missing".to_string());

        let error = project_detail_in(&tiny.ctx(), &two_weeks(), &all(), &key).unwrap_err();
        assert!(
            error.to_string().contains("github.com/sam/missing"),
            "the error names the project: {error}"
        );
    }

    #[test]
    fn the_account_filter_applies() {
        let tiny = tiny();
        let query = Query {
            range: crate::stats::RangeKey::All,
            accounts: vec![tiny.lb().id.clone()],
            host: None,
        };
        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &query).unwrap();

        assert!(
            page.nodes.iter().all(|node| node.name != "acme-kb"),
            "acme-kb is on the page: {:#?}",
            page.nodes.iter().map(|node| &node.name).collect::<Vec<_>>()
        );
        let atlas = node(&page, "atlas");
        assert_eq!(atlas.values.api, Some(60.0));
    }

    /// Codex's price: 10 USD per million output tokens, at the fixture's
    /// rate of 1.
    fn price_codex(tiny: &mut crate::stats::tests::Tiny) {
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

    /// A Codex record on atlas with no stored OpenAI subscription names no
    /// subscription: it is the `None` part of `by_subscription`, not a Claude
    /// account, and it is not in the unknown account's tokens or in a matrix
    /// cell. The account chips choose each provider by its subscription
    /// (decision P-2 8): choosing LB alone leaves only LB's own work.
    #[test]
    fn codex_work_without_a_named_subscription_is_unknown() {
        let mut tiny = tiny();
        price_codex(&mut tiny);
        let before = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        tiny.history
            .append(&record(
                "2026-11-10T10:00:00Z",
                "019a7c3e-4b2a-7d3f-9e6a-b8c52f0a1d7e",
                "/home/sam/projects/atlas/engine",
                atlas(),
                "gpt-6.1-sol",
                3_000_000,
            ))
            .expect("a Codex record");

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let atlas_node = node(&page, "atlas");
        assert_eq!(
            atlas_node.values.api,
            Some(150.0),
            "120 EUR of Claude, 30 of Codex"
        );
        assert_eq!(
            atlas_node.by_subscription.len(),
            3,
            "{:?}",
            atlas_node.by_subscription
        );
        let unknown = atlas_node
            .by_subscription
            .iter()
            .find(|(subscription, _)| subscription.is_none())
            .expect("the Codex work's unknown subscription");
        assert!(close(unknown.1, 20.0), "the unknown part {}", unknown.1);
        assert!(atlas_node
            .by_subscription
            .iter()
            .filter(|(subscription, _)| subscription.is_some())
            .all(|(_, share)| close(*share, 40.0)));
        let json = serde_json::to_value(atlas_node).expect("the node's JSON shape");
        assert_eq!(
            json["bySubscription"],
            serde_json::json!([
                [tiny.lb().id.clone(), 40.0],
                [tiny.ac().id.clone(), 40.0],
                [serde_json::Value::Null, 20.0],
            ]),
            "the chip ids, and null for work without a known subscription"
        );
        assert_eq!(page.unknown_account_tokens, before.unknown_account_tokens);
        assert_eq!(page.matrix, before.matrix, "Codex is in no matrix cell");
        assert!(
            !page.notes.iter().any(|note| note.contains("OpenAI")),
            "the matrix now has the provider's column: {:#?}",
            page.notes
        );
        assert!(!before.notes.iter().any(|note| note.contains("OpenAI")));

        let only_lb = Query {
            range: crate::stats::RangeKey::All,
            accounts: vec![tiny.lb().id.clone()],
            host: None,
        };
        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &only_lb).unwrap();
        let atlas_node = node(&page, "atlas");
        assert_eq!(
            atlas_node.values.api,
            Some(60.0),
            "only LB's own work: the Codex work's subscription is none the query names"
        );
        assert_eq!(atlas_node.by_subscription.len(), 1);
        let detail = project_detail_in(&tiny.ctx(), &two_weeks(), &only_lb, &atlas()).unwrap();
        assert_eq!(detail.values.api, Some(60.0));
    }

    /// A Codex record on atlas with a stored OpenAI subscription is part of OA
    /// everywhere: a part of the node's `by_subscription`, the matrix's OA
    /// column and the row total over the subscriptions (R2).
    #[test]
    fn provider_work_is_a_part_of_its_subscription() {
        let mut tiny = tiny();
        price_codex(&mut tiny);
        let oa = codex_ref();
        tiny.provider_subscriptions = vec![provider_subscription(&oa, "OA", Some(23.0))];
        // The OpenAI subscription's week of 7 Nov .. 14 Nov, with 12 % used.
        append_codex_records(&tiny.history, utc("2026-11-14T09:00:00Z"));
        tiny.history
            .append(&record(
                "2026-11-10T10:00:00Z",
                "019a7c3e-4b2a-7d3f-9e6a-b8c52f0a1d7e",
                "/home/sam/projects/atlas/engine",
                atlas(),
                "gpt-6.1-sol",
                1_000_000,
            ))
            .expect("a Codex record");

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let atlas_node = node(&page, "atlas");
        assert_eq!(
            atlas_node.values.api,
            Some(130.0),
            "120 EUR of Claude, 10 of Codex"
        );
        let oa_share = atlas_node
            .by_subscription
            .iter()
            .find(|(subscription, _)| subscription.as_deref() == Some(&oa.acc))
            .expect("OA's share of atlas");
        assert!(close(oa_share.1, 10.0 / 130.0 * 100.0), "OA {}", oa_share.1);

        let matrix = &page.matrix;
        assert_eq!(
            matrix.subscriptions,
            vec![tiny.lb().id.clone(), tiny.ac().id.clone(), oa.acc.clone()],
            "the provider subscription follows the accounts, in the chips' order"
        );
        let sam = matrix
            .orgs
            .iter()
            .position(|org| org == "sam")
            .expect("sam's row");
        let oa_column = matrix
            .subscriptions
            .iter()
            .position(|subscription| subscription == &oa.acc)
            .expect("OA's column");
        // 12 % of one week's share of the 23 EUR monthly price.
        let oa_week = 23.0 * 12.0 / 52.0;
        let oa_used = oa_week * 12.0 / 100.0;
        assert!(
            close(matrix.cells[sam][oa_column].expect("OA's cell"), oa_used),
            "OA's cell: {:?}",
            matrix.cells[sam][oa_column]
        );
        assert!(
            close(
                matrix.row_totals[sam].subscription.expect("the row total"),
                74.769 + 29.077 + oa_used
            ),
            "the provider column counts in the row total: {:?}",
            matrix.row_totals[sam]
        );
        assert!(!matrix.row_totals[sam].subscription_partial);

        let detail = project_detail_in(&tiny.ctx(), &two_weeks(), &all(), &atlas()).unwrap();
        let detail_share = detail
            .by_subscription
            .iter()
            .find(|(subscription, _)| subscription.as_deref() == Some(&oa.acc))
            .expect("OA's share of the project");
        assert!(close(detail_share.1, oa_share.1));
    }

    /// An Ollama record whose subscription has no price makes its cell unknown,
    /// never a measured 0; the row and the grand total keep the known part and
    /// say why they are partial (R3).
    #[test]
    fn an_unpriced_provider_cell_is_none_and_the_row_total_is_partial() {
        let mut tiny = tiny();
        let ollama = crate::money::ollama_subscription();
        tiny.provider_subscriptions = vec![provider_subscription(&ollama, "OL", None)];
        tiny.prices.take(
            BTreeMap::from([(
                "ollama-cloud/deepseek-v4.1-flash".to_string(),
                crate::money::ModelPrice {
                    input: 1.0,
                    output: 10.0,
                    cache_write: 0.0,
                    cache_read: 0.0,
                    tiered: false,
                },
            )]),
            utc("2026-01-01T00:00:00Z"),
        );
        let month = |t: &str, percent: f64| Sample {
            t: utc(t),
            account: ollama.clone(),
            src: Source::Poll,
            l: vec![LimitSample {
                k: crate::api::LimitKind::Other("monthly_credits".to_string()),
                m: None,
                p: percent,
                r: Some(utc("2026-12-01T00:00:00Z")),
                b: false,
            }],
        };
        tiny.history
            .append(&month("2026-11-02T00:00:00Z", 10.0))
            .expect("Ollama's first reading");
        tiny.history
            .append(&month("2026-11-15T00:00:00Z", 60.0))
            .expect("Ollama's newest reading");
        tiny.history
            .append(&record(
                "2026-11-10T10:00:00Z",
                "5d3b8c2a-6e1f-4b7a-9c4d-2f8e1a3b5c70",
                "/home/sam/projects/atlas/engine",
                atlas(),
                "deepseek-v4.1-flash:cloud",
                1_000_000,
            ))
            .expect("an Ollama record");

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let matrix = &page.matrix;
        let sam = matrix
            .orgs
            .iter()
            .position(|org| org == "sam")
            .expect("sam's row");
        let ol_column = matrix
            .subscriptions
            .iter()
            .position(|subscription| subscription == &ollama.acc)
            .expect("OL's column");
        assert_eq!(
            matrix.cells[sam][ol_column], None,
            "a subscription without a price has no cell value, not 0"
        );
        assert!(
            close(
                matrix.row_totals[sam].subscription.expect("the known part"),
                74.769 + 29.077
            ),
            "the row keeps its known part: {:?}",
            matrix.row_totals[sam]
        );
        assert!(
            matrix.row_totals[sam].subscription_partial,
            "the unpriced cell makes the row total partial"
        );
        assert_eq!(
            matrix.row_totals[sam].subscription_missing.as_deref(),
            Some("the account has no monthly price")
        );
        assert!(
            close(matrix.total.subscription.expect("the known part"), 120.0),
            "the grand total keeps its known part: {:?}",
            matrix.total
        );
        assert!(
            matrix.total.subscription_partial,
            "the unpriced cell makes the grand total partial"
        );
        assert_eq!(
            matrix.total.subscription_missing.as_deref(),
            Some("the account has no monthly price")
        );
    }

    /// R4: the note names the work whose account was taken from the usage
    /// readings, with the tokens written as the other notes write them.
    #[test]
    fn work_from_the_readings_is_named_in_the_notes() {
        let tiny = tiny();
        let weekly = |t: &str, account: &AccountRef, percent: f64| Sample {
            t: utc(t),
            account: account.clone(),
            src: Source::Poll,
            l: vec![LimitSample {
                k: crate::api::LimitKind::WeeklyAll,
                m: None,
                p: percent,
                r: Some(utc("2026-11-18T10:00:00Z")),
                b: false,
            }],
        };
        // AC's week rises around the record, before the first live event at
        // 08:00; LB's does not rise.
        tiny.history
            .append_many(&[
                weekly("2026-11-02T06:20:00Z", &tiny.ac_ref(), 41.0),
                weekly("2026-11-02T06:40:00Z", &tiny.ac_ref(), 43.0),
                weekly("2026-11-02T06:20:00Z", &tiny.lb_ref(), 10.0),
                weekly("2026-11-02T06:40:00Z", &tiny.lb_ref(), 10.0),
            ])
            .expect("the readings around the record");
        let mut work = record(
            "2026-11-02T06:25:00Z",
            "0c4d9e2a-8b1f-4a6c-9d3e-5f7a1b2c4d60",
            "/home/sam/projects/atlas/engine",
            atlas(),
            "claude-opus-5-5",
            1_000_000,
        );
        work.minutes = 5;
        tiny.history
            .append(&work)
            .expect("the work of the readings");

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        assert!(
            page.notes
                .iter()
                .any(|note| note == "1,000,000 tokens: account taken from the usage readings."),
            "notes: {:#?}",
            page.notes
        );
        assert_eq!(
            page.unknown_account_tokens,
            Tokens::default(),
            "the record has an account, so it is not unknown work"
        );
    }

    // ---- proof tests of the review ----

    /// The project log joins a second project into atlas: from then on the two
    /// keys are one project. The tree counts the joined key's records under
    /// atlas, so the detail of atlas must count them too.
    #[test]
    fn the_detail_of_a_joined_project_counts_the_joined_records() {
        let tiny = tiny();
        let web = ProjectKey::Repo("github.com/sam/atlas-web".to_string());
        tiny.history
            .append(&ProjectsLogRecord {
                t: utc("2026-11-01T00:00:00Z"),
                edit: ProjectEdit::Join {
                    key: crate::projects::ProjectId::of(&web, "atlas"),
                    into: crate::projects::ProjectId::of(&atlas(), "atlas"),
                },
            })
            .expect("the join");
        tiny.history
            .append(&record(
                "2026-11-11T10:00:00Z",
                "s9",
                "/home/sam/projects/atlas/engine",
                web.clone(),
                "claude-opus-5-5",
                1_000_000,
            ))
            .expect("a record of the joined key");
        let (projects, warnings) =
            crate::projects::load(&tiny.history, &tiny.accounts).expect("the projects");
        assert!(warnings.is_empty(), "project read warnings: {warnings:?}");
        let ctx = Ctx {
            projects: &projects,
            ..tiny.ctx()
        };

        let page = projects_page_in(&ctx, &two_weeks(), &all()).unwrap();
        let atlas_row = node(&page, "atlas");
        assert_eq!(
            atlas_row.tokens.out, 7_000_000,
            "the tree counts the joined record"
        );
        assert_eq!(
            atlas_row.values.api,
            Some(140.0),
            "the tree counts the joined record"
        );

        let detail = project_detail_in(&ctx, &two_weeks(), &all(), &atlas()).unwrap();
        assert_eq!(
            detail.tokens.out, 7_000_000,
            "the detail of the project counts the records of the key joined into it"
        );
        assert_eq!(
            detail.values.api,
            Some(140.0),
            "the detail counts the joined record"
        );
    }
    /// A ratio divides a window by its own span: Last week with AC's window
    /// reaching two days past the range counts the record of 17 Nov too.
    #[test]
    fn tokens_per_percent_count_the_whole_window() {
        let tiny = tiny();
        tiny.history
            .append(&crate::history::WindowRecord {
                account: tiny.ac_ref(),
                k: crate::api::LimitKind::WeeklyAll,
                m: None,
                start: Some(utc("2026-11-11T00:00:00Z")),
                end: utc("2026-11-18T00:00:00Z"),
                final_percent: 100.0,
                lost: Some(0.0),
                last_sample: utc("2026-11-17T23:58:00Z"),
                plan: Some("Team Premium".to_string()),
                uncertain: false,
                reset: false,
                from: None,
            })
            .expect("AC's window reaching past the range");
        tiny.history
            .append(&record(
                "2026-11-17T10:00:00Z",
                "s7",
                "/home/sam/projects/atlas/engine",
                atlas(),
                "claude-opus-5-5",
                2_000_000,
            ))
            .expect("the record after the range's end");
        let last_week = Range {
            from: utc("2026-11-09T00:00:00Z"),
            to: utc("2026-11-16T00:00:00Z"),
        };

        let page = projects_page_in(&tiny.ctx(), &last_week, &all()).unwrap();
        let ac = page
            .tokens_per_percent
            .iter()
            .find(|(id, _)| id == &tiny.ac().id)
            .expect("AC's tokens per percent");
        assert!(
            close(ac.1, 5_000_000.0 / 190.0),
            "AC's two windows of the week use 90 % and 100 %: {}",
            ac.1
        );
    }

    /// A record in the first window of a week that a reset split in two counts
    /// for that week: the token of 3 Nov is in the 2 Nov .. 5 Nov 12:00 window,
    /// while the week's used percent is the sum of both windows (92).
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
            .append(&record(
                "2026-11-03T10:00:00Z",
                "s0",
                "/home/sam/ledger",
                atlas(),
                "claude-opus-5-5",
                1_000_000,
            ))
            .expect("the token in the first window");
        let ctx = Ctx {
            history: &history,
            ..tiny.ctx()
        };

        // The range starts after the token and before the first window's
        // midpoint; the whole week counts, so the token counts too.
        let page = projects_page_in(
            &ctx,
            &Range {
                from: utc("2026-11-03T12:00:00Z"),
                to: utc("2026-11-21T00:00:00Z"),
            },
            &all(),
        )
        .unwrap();
        let lb = page
            .tokens_per_percent
            .iter()
            .find(|(id, _)| id == &tiny.lb().id)
            .expect("LB's tokens per percent");
        assert!(
            close(lb.1, 1_000_000.0 / 92.0),
            "the token of 3 Nov lies in the first window and the week uses 92 %: {}",
            lb.1
        );
    }

    // ---- proof tests of the review of T-5 ----

    /// A total of parts where one part is missing keeps the known part and says
    /// so (R3): it is partial, not a measured zero and not unknown. A record of
    /// a model without a price leaves its account's cell without a subscription
    /// value, so the row, the column and the total that hold it are partial.
    #[test]
    fn a_missing_cell_makes_the_totals_partial() {
        let tiny = tiny();
        let projects = with_acme_account(&tiny);
        let ctx = Ctx {
            projects: &projects,
            ..tiny.ctx()
        };
        // The record is of 15 Nov, when AC was the live account.
        tiny.history
            .append(&record(
                "2026-11-15T09:00:00Z",
                "s5",
                "/home/sam/projects/atlas/engine",
                atlas(),
                "claude-x",
                1_000_000,
            ))
            .expect("a record without a price");

        let page = projects_page_in(&ctx, &two_weeks(), &all()).unwrap();
        let matrix = &page.matrix;
        let org = |name: &str| {
            matrix
                .orgs
                .iter()
                .position(|org| org == name)
                .expect("the organization")
        };
        let subscription = |id: &str| {
            matrix
                .subscriptions
                .iter()
                .position(|subscription| subscription == id)
                .expect("the subscription")
        };
        let sam = org("sam");
        let acme = org("acme");
        let ac = subscription(&tiny.ac().id);
        let lb = subscription(&tiny.lb().id);
        let reason = "that week also has tokens of claude-x, which has no price";

        assert_eq!(
            matrix.cells[sam][ac], None,
            "the cell of the account whose week has no value is unknown"
        );
        assert!(
            close(
                matrix.row_totals[sam].subscription.expect("the known part"),
                74.769
            ),
            "the row keeps its known part: {:?}",
            matrix.row_totals[sam]
        );
        assert!(
            matrix.row_totals[sam].subscription_partial,
            "a row with an unknown cell is partial, not 0 and not unknown"
        );
        assert_eq!(
            matrix.row_totals[sam].subscription_missing.as_deref(),
            Some(reason)
        );
        assert!(
            close(
                matrix.column_totals[ac]
                    .subscription
                    .expect("the known part"),
                16.154
            ),
            "the column keeps its known part: {:?}",
            matrix.column_totals[ac]
        );
        assert!(
            matrix.column_totals[ac].subscription_partial,
            "a column with an unknown cell is partial"
        );
        assert!(
            close(
                matrix.total.subscription.expect("the known part"),
                74.769 + 16.154
            ),
            "the grand total keeps its known part: {:?}",
            matrix.total
        );
        assert!(
            matrix.total.subscription_partial,
            "the total of a matrix with an unknown cell is partial"
        );
        assert_eq!(matrix.total.subscription_missing.as_deref(), Some(reason));
        // The parts that are known keep their measured values.
        assert!(close(matrix.row_totals[acme].subscription.unwrap(), 16.154));
        assert!(close(
            matrix.column_totals[lb].subscription.unwrap(),
            74.769
        ));
    }

    /// The matrix's row total is the organization's subscription value: the same
    /// number the tree shows on the organization's row.
    #[test]
    fn the_matrix_row_total_is_the_organizations_subscription_value() {
        let tiny = tiny();
        let projects = with_acme_account(&tiny);
        let ctx = Ctx {
            projects: &projects,
            ..tiny.ctx()
        };

        let page = projects_page_in(&ctx, &two_weeks(), &all()).unwrap();
        assert!(!page.matrix.orgs.is_empty(), "the matrix has organizations");
        for (index, org) in page.matrix.orgs.iter().enumerate() {
            let row = node(&page, org);
            assert_eq!(row.kind, NodeKind::Org, "{org} is an organization");
            // The two are added up in another order (by subscription and by
            // record), so they can differ in the last bit; the page writes the
            // same money.
            assert!(
                close(
                    page.matrix.row_totals[index]
                        .subscription
                        .expect("the fixture's row values"),
                    row.values.subscription.unwrap()
                ),
                "the matrix's row total of {org} is the value the tree shows for it"
            );
        }
    }

    /// A range with no record has no organization, no cell and no money: the
    /// matrix is empty and a project's share of all use is nothing.
    #[test]
    fn an_empty_range_has_no_organizations_and_no_share() {
        let tiny = tiny();
        let empty = Range {
            from: utc("2026-12-01T00:00:00Z"),
            to: utc("2026-12-15T00:00:00Z"),
        };

        let page = projects_page_in(&tiny.ctx(), &empty, &all()).unwrap();
        assert!(page.nodes.is_empty(), "no record, no node");
        assert!(page.matrix.orgs.is_empty(), "no record, no organization");
        assert!(page.matrix.row_totals.is_empty(), "no row, no row total");
        assert!(
            page.matrix
                .column_totals
                .iter()
                .all(|total| total.subscription == Some(0.0) && !total.subscription_partial),
            "no cell, no money: a measured zero"
        );
        assert_eq!(
            page.matrix.total.subscription,
            Some(0.0),
            "no cell, no money"
        );

        let detail = project_detail_in(&tiny.ctx(), &empty, &all(), &atlas()).unwrap();
        assert_eq!(
            detail.values.api,
            Some(0.0),
            "no record is a known 0, not an unknown value"
        );
        assert_eq!(detail.share, None, "no use in the range, no share");
        assert_eq!(detail.sessions, 0);
    }

    /// The only project of a range is all of its use: its share is 100 %.
    #[test]
    fn one_project_is_the_whole_use() {
        let tiny = tiny();
        // Only the record of 8 Nov is in this range; it is of acme-kb.
        let day = Range {
            from: utc("2026-11-08T00:00:00Z"),
            to: utc("2026-11-09T00:00:00Z"),
        };
        let acme_kb = ProjectKey::Repo("github.com/acme/acme-kb".to_string());

        let page = projects_page_in(&tiny.ctx(), &day, &all()).unwrap();
        assert_eq!(page.nodes.len(), 2, "the organization and its project");
        assert!(
            close(node(&page, "acme-kb").share.unwrap(), 100.0),
            "the only project is all the use: {:?}",
            node(&page, "acme-kb").share
        );

        let detail = project_detail_in(&tiny.ctx(), &day, &all(), &acme_kb).unwrap();
        assert!(
            close(detail.share.unwrap(), 100.0),
            "the only project is all the use: {:?}",
            detail.share
        );
        assert_eq!(detail.sessions, 1);
    }

    /// A project with no record in the range is worth a known 0 and has a 0
    /// share of the use that is there.
    #[test]
    fn a_project_without_use_in_the_range_has_a_zero_share() {
        let tiny = tiny();
        // Only the record of 8 Nov is in this range; it is of acme-kb.
        let day = Range {
            from: utc("2026-11-08T00:00:00Z"),
            to: utc("2026-11-09T00:00:00Z"),
        };

        let detail = project_detail_in(&tiny.ctx(), &day, &all(), &atlas()).unwrap();
        assert_eq!(
            detail.values.api,
            Some(0.0),
            "no record is a known 0, not an unknown value"
        );
        assert_eq!(
            detail.share,
            Some(0.0),
            "no use in the range is a known 0 share"
        );
        assert_eq!(detail.sessions, 0);
    }

    // ---- proof tests of the fix of T-5 ----

    /// A project whose model has no price keeps the known part of its API value
    /// (R3), but a share divides only full halves, so its share of all use is
    /// unknown, never a made-up 0 %: the page writes "—".
    #[test]
    fn a_project_without_a_price_has_no_share() {
        let tiny = tiny();
        tiny.history
            .append(&record(
                "2026-11-15T09:00:00Z",
                "s5",
                "/home/sam/projects/atlas/engine",
                atlas(),
                "claude-x",
                1_000_000,
            ))
            .expect("a record without a price");

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let atlas_row = node(&page, "atlas");
        assert!(
            atlas_row.values.api.is_some(),
            "the priced part stays known"
        );
        assert!(atlas_row.values.api_partial, "one part without a price");
        assert_eq!(
            atlas_row.share, None,
            "an unknown share is unknown, not 0 %"
        );

        let detail = project_detail_in(&tiny.ctx(), &two_weeks(), &all(), &atlas()).unwrap();
        assert!(detail.values.api.is_some(), "the priced part stays known");
        assert!(detail.values.api_partial, "one part without a price");
        assert_eq!(detail.share, None, "an unknown share is unknown, not 0 %");

        // The page reads the JSON `null`: a tree row's `share` and the detail's.
        let json = serde_json::to_value(&page).expect("the page's JSON shape");
        let atlas_json = json["nodes"]
            .as_array()
            .expect("the nodes")
            .iter()
            .find(|node| node["name"] == "atlas")
            .expect("atlas's row");
        assert!(atlas_json["share"].is_null());
        let detail_json = serde_json::to_value(&detail).expect("the detail's JSON shape");
        assert!(detail_json["share"].is_null());
    }

    /// A host's share and the organizations' sentences divide a value; a partial
    /// half is a lower bound, so neither is a figure and both are left out (R3).
    #[test]
    fn a_partial_value_leaves_out_a_host_share_and_the_sentences() {
        let tiny = tiny();
        tiny.history
            .append(&record(
                "2026-11-15T09:00:00Z",
                "s5",
                "/home/sam/projects/atlas/engine",
                atlas(),
                "claude-x",
                1_000_000,
            ))
            .expect("a record without a price");

        let detail = project_detail_in(&tiny.ctx(), &two_weeks(), &all(), &atlas()).unwrap();
        assert!(detail.values.api_partial, "{:?}", detail.values);
        assert!(
            detail.hosts.is_empty(),
            "a host's share of a partial value is no figure: {:?}",
            detail.hosts
        );

        let projects = with_acme_account(&tiny);
        let ctx = Ctx {
            projects: &projects,
            ..tiny.ctx()
        };
        let full = |amount: f64| Values {
            api: Some(0.0),
            api_currency: Some("EUR".to_string()),
            api_missing: None,
            api_partial: false,
            subscription: Some(amount),
            currency: Some("EUR".to_string()),
            subscription_missing: None,
            subscription_partial: false,
        };
        let partial = |amount: f64| Values {
            api: Some(0.0),
            api_currency: Some("EUR".to_string()),
            api_missing: None,
            api_partial: false,
            subscription: Some(amount),
            currency: Some("EUR".to_string()),
            subscription_missing: Some("no price for claude-x".to_string()),
            subscription_partial: true,
        };
        let mut sums = Sums {
            values: Some(full(100.0)),
            ..Sums::default()
        };
        sums.by_subscription.insert(Some(tiny.ac_ref()), full(29.0));
        sums.by_subscription
            .insert(Some(tiny.lb_ref()), partial(71.0));
        let acme = OrgRow {
            identity: Some("acme".to_string()),
            name: "acme".to_string(),
            members: Vec::new(),
            sums,
        };
        let mut sam_sums = Sums::default();
        sam_sums
            .by_subscription
            .insert(Some(tiny.ac_ref()), partial(71.0));
        let sam = OrgRow {
            identity: Some("sam".to_string()),
            name: "sam".to_string(),
            members: Vec::new(),
            sums: sam_sums,
        };

        let orgs = vec![acme, sam];
        let sentences = org_sentences(&ctx, &orgs[0], &orgs, Some("EUR"));
        assert_eq!(
            sentences,
            vec!["AC is acme's subscription. It covered 29 % of the work for acme.".to_string()],
            "a sentence whose figure would come from a partial value is left out"
        );
    }

    /// The rate says which currency it converts into, so the page does not take
    /// it from another view's settings.
    #[test]
    fn the_usd_rate_carries_its_currency() {
        let mut tiny = tiny();
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 13).expect("the rate's date"),
            0.9048,
        );

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let rate = page.usd_rate.as_ref().expect("the page's rate");
        assert_eq!(rate.currency, "EUR", "the settings' currency");

        let json = serde_json::to_value(&page).expect("the page's JSON shape");
        assert_eq!(json["usdRate"]["currency"], "EUR");
    }

    /// Rust rounds the rate to four decimals before sending it; the page writes
    /// that value and adds no rounding of its own.
    #[test]
    fn the_usd_rate_value_is_rounded_to_four_decimals() {
        let mut tiny = tiny();
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 13).expect("the rate's date"),
            0.90486,
        );

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let rate = page.usd_rate.as_ref().expect("the page's rate");
        assert_eq!(rate.value, 0.9049, "the rate is rounded to four decimals");

        let json = serde_json::to_value(&page).expect("the page's JSON shape");
        assert_eq!(json["usdRate"]["value"], 0.9049);
    }

    /// A range that crosses a change of the looked-up rate says so: the valuer
    /// converted each record at its own day's rate.
    #[test]
    fn the_rate_varies_across_a_rate_change() {
        let mut tiny = tiny();
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 5).expect("the first rate's date"),
            0.9000,
        );
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 10).expect("the second rate's date"),
            0.9500,
        );

        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();
        let rate = page.usd_rate.as_ref().expect("the page's rate");
        assert!(rate.varies, "the range has two rates: {:?}", rate);
        assert_eq!(rate.value, 0.95, "the latest rate of the range");

        let json = serde_json::to_value(&page).expect("the page's JSON shape");
        assert_eq!(json["usdRate"]["varies"], true);
    }

    /// With look-ups off the page has no rate at all, so `rate_varies` asks the
    /// one `usd_rate` source (which answers `None`) instead of reading the stored
    /// points itself.
    #[test]
    fn a_rate_is_not_read_when_look_ups_are_off() {
        let mut tiny = tiny();
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 5).expect("the first rate's date"),
            0.9000,
        );
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 10).expect("the second rate's date"),
            0.9500,
        );
        tiny.settings.look_up_prices = false;

        assert!(!rate_varies(&tiny.settings, &tiny.fx, &two_weeks()));
    }

    /// A range inside one stored rate uses that one rate all along.
    #[test]
    fn the_rate_does_not_vary_inside_one_rate() {
        let mut tiny = tiny();
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 5).expect("the first rate's date"),
            0.9000,
        );
        tiny.fx.take(
            NaiveDate::from_ymd_opt(2026, 11, 10).expect("the second rate's date"),
            0.9500,
        );
        let week = Range {
            from: utc("2026-11-05T00:00:00Z"),
            to: utc("2026-11-10T00:00:00Z"),
        };

        let page = projects_page_in(&tiny.ctx(), &week, &all()).unwrap();
        let rate = page.usd_rate.as_ref().expect("the page's rate");
        assert!(!rate.varies, "one rate for every day: {:?}", rate);
        assert_eq!(rate.value, 0.9, "that one rate");

        let json = serde_json::to_value(&page).expect("the page's JSON shape");
        assert_eq!(json["usdRate"]["varies"], false);
    }
}
