//! The Projects page (spec §3.6): the tree of organizations, projects and
//! folders, the organization × account matrix with its sentences, the tokens
//! per percent of each account, and one project's detail. Everything here only
//! reads.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;

use crate::history::{AccountRef, Hosts, Range, TokenRecord};
use crate::live_sessions::Values;
use crate::money::{Missing, Valuer};
use crate::project::ProjectKey;
use crate::projects::{Project, ProjectId};
use crate::store::Account;
use crate::transcripts::Tokens;

use super::{
    account_abbr, account_chosen, account_id, account_position, account_weeks_valued,
    add_opt_value, add_value, join_names, money_text, output_by_account_week, percent_text,
    range_of, thousands, week_start, windows_span, Ctx, Query, StatsError,
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
    /// This node's API value as a share of the page's API value, in %; 0 when
    /// the page has no API value.
    pub share: f64,
    /// The two values of the node's records, built with [`Values::of`]: a
    /// missing half names its reason, and the API half names its currency.
    pub values: Values,
    /// Each account's share of this node's API value, in %, largest first;
    /// `None` is a record without a known account.
    pub by_account: Vec<(Option<String>, f64)>,
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
    /// The tokens of the records whose account is unknown.
    pub unknown_account_tokens: Tokens,
    /// The distinct models whose API value is missing because they have no
    /// price, sorted.
    pub without_price: Vec<String>,
    /// What the page says is not in the tree, in the order the plan gives it.
    pub notes: Vec<String>,
}

/// The organization × account matrix.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Matrix {
    /// The organizations' shown names, in the tree's order.
    pub orgs: Vec<String>,
    /// The chosen stored accounts' ids, in [`Ctx::accounts`]' order.
    pub accounts: Vec<String>,
    /// The subscription value per organization and account:
    /// `cells[org][account]`; `None` when a record has no subscription value.
    pub cells: Vec<Vec<Option<f64>>>,
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
    )?;
    let weeks = account_weeks_valued(ctx, range, q, &valuer)?;
    let read = ctx.history.read::<TokenRecord>(range, &Hosts::All)?;

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
        if !account_chosen(ctx, q, value.account.as_ref()) {
            continue;
        }
        let values = Values::of(std::slice::from_ref(&value), ctx.settings);
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
    let page_api = page_values.api;

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
    let currency = ctx.settings.currency.as_deref().map(str::to_string);

    let mut nodes = Vec::new();
    let mut matrix_orgs = Vec::new();
    let mut cells = Vec::new();
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
            page_api,
        ));
        matrix_orgs.push(org.name.clone());
        cells.push(org_cells(org, &chosen_accounts));
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
                page_api,
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
                        page_api,
                    ));
                }
            }
        }
    }

    // The ratios divide each window by its own span, which can reach past the
    // range; the reads that serve them cover every chosen window whole.
    let week_span = windows_span(&weeks, range);
    let week_tokens = ctx.history.read::<TokenRecord>(&week_span, &Hosts::All)?;
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
        let percent: f64 = account_weeks
            .iter()
            .map(|week| week.window.final_percent)
            .sum();
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
    for entry in &entries {
        if entry.value.account.is_none() {
            unknown_account_tokens += Tokens::from(entry.record);
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
    // A record with a known account can still have no subscription value; the
    // page says how many tokens that leaves out and why.
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

    Ok(ProjectsPage {
        nodes,
        matrix: Matrix {
            orgs: matrix_orgs,
            accounts: chosen_accounts
                .iter()
                .map(|account| account.id.clone())
                .collect(),
            cells,
            sentences,
        },
        tokens_per_percent,
        unknown_account_tokens,
        without_price: without_price.into_iter().collect(),
        notes,
    })
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
    )?;
    let read = ctx.history.read::<TokenRecord>(range, &Hosts::All)?;
    let matching: Vec<_> = read
        .records
        .iter()
        .filter(|hosted| q.host.as_ref().is_none_or(|host| host == &hosted.host))
        .filter(|hosted| {
            let detail = ctx.projects.of(&ProjectId::of(key, &hosted.host));
            let record = ctx
                .projects
                .of(&ProjectId::of(&hosted.record.project, &hosted.host));
            record.key == detail.key
        })
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
    let mut answers = 0u64;
    let mut agent_answers = 0u64;
    let mut hosts: BTreeMap<String, Values> = BTreeMap::new();
    let mut weekly: BTreeMap<DateTime<Utc>, BTreeMap<String, u64>> = BTreeMap::new();
    for hosted in matching {
        let value = valuer.value(&hosted.host, &hosted.record);
        if !account_chosen(ctx, q, value.account.as_ref()) {
            continue;
        }
        let values = Values::of(std::slice::from_ref(&value), ctx.settings);
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
    let mut host_shares: Vec<(String, f64)> = match host_values.api {
        Some(total) if total > 0.0 => hosts
            .iter()
            .filter_map(|(host, values)| values.api.map(|api| (host.clone(), api / total * 100.0)))
            .collect(),
        _ => Vec::new(),
    };
    host_shares.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let weekly_by_model: WeeklyByModel = weekly
        .into_iter()
        .map(|(week, models)| {
            let mut models: Vec<(String, u64)> = models.into_iter().collect();
            models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            (week, models)
        })
        .collect();

    Ok(ProjectDetail {
        name,
        tokens: sums.tokens,
        agent_share: if answers > 0 {
            agent_answers as f64 / answers as f64 * 100.0
        } else {
            0.0
        },
        hosts: host_shares,
        values: sums.values(),
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
    api_by_account: BTreeMap<Option<AccountRef>, Values>,
    sub_by_account: BTreeMap<AccountRef, Values>,
    sessions: BTreeSet<String>,
}

impl Sums {
    fn add(&mut self, entry: &Entry) {
        let record = entry.record;
        self.tokens += Tokens::from(record);
        self.sessions.insert(record.session.clone());
        add_opt_value(&mut self.values, &entry.values);
        add_value(
            &mut self.api_by_account,
            entry.value.account.clone(),
            &entry.values,
        );
        if let Some(account) = &entry.value.account {
            add_value(&mut self.sub_by_account, account.clone(), &entry.values);
        }
    }

    fn merge(&mut self, other: &Sums) {
        self.tokens += other.tokens;
        if let Some(values) = &other.values {
            add_opt_value(&mut self.values, values);
        }
        for (account, values) in &other.api_by_account {
            add_value(&mut self.api_by_account, account.clone(), values);
        }
        for (account, values) in &other.sub_by_account {
            add_value(&mut self.sub_by_account, account.clone(), values);
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
    page_api: Option<f64>,
) -> Node {
    let level = match label.kind {
        NodeKind::Org => 0,
        NodeKind::Project => 1,
        NodeKind::Folder => 2,
    };
    let values = sums.values();
    Node {
        kind: label.kind,
        level,
        name: label.name,
        key: label.key,
        org_accounts: label.org_accounts,
        tokens: sums.tokens,
        share: share_of(values.api, page_api),
        values,
        by_account: by_account(ctx, sums),
        sessions: sums.sessions.len() as u32,
    }
}

/// Each account's share of the node's API value, largest first; an account no
/// longer stored sorts after the stored ones.
fn by_account<Tz: TimeZone + Copy>(ctx: &Ctx<Tz>, sums: &Sums) -> Vec<(Option<String>, f64)> {
    let Some(api) = sums.api().filter(|api| *api > 0.0) else {
        return Vec::new();
    };
    let mut shares: Vec<(Option<String>, f64)> = sums
        .api_by_account
        .iter()
        .filter_map(|(account, values)| {
            values.api.map(|value| {
                (
                    account.as_ref().map(|account| account_id(ctx, account)),
                    value / api * 100.0,
                )
            })
        })
        .collect();
    shares.sort_by(|a, b| {
        b.1.total_cmp(&a.1).then_with(|| match (&a.0, &b.0) {
            (Some(a), Some(b)) => account_position(ctx, a)
                .cmp(&account_position(ctx, b))
                .then_with(|| a.cmp(b)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
    });
    shares
}

fn share_of(api: Option<f64>, page_api: Option<f64>) -> f64 {
    match (api, page_api) {
        (Some(api), Some(total)) if total > 0.0 => api / total * 100.0,
        _ => 0.0,
    }
}

/// The subscription value of the chosen accounts in one organization, in the
/// accounts' order; an account with no record there has a measured zero.
fn org_cells(org: &OrgRow, accounts: &[&Account]) -> Vec<Option<f64>> {
    accounts
        .iter()
        .map(
            |account| match org.sums.sub_by_account.get(&AccountRef::of(account)) {
                Some(values) => values.subscription,
                None => Some(0.0),
            },
        )
        .collect()
}

/// The sentences of one organization that has accounts of its own.
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
    let Some(total) = org.sums.values().subscription else {
        return Vec::new();
    };
    let own: BTreeSet<AccountRef> = known.accounts.iter().cloned().collect();
    let own_parts: Vec<&Values> = own
        .iter()
        .filter_map(|account| org.sums.sub_by_account.get(account))
        .collect();
    let own_sub = if own_parts.is_empty() {
        0.0
    } else {
        match Values::sum(&own_parts).subscription {
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
            let others: Vec<(&AccountRef, &Values)> = org
                .sums
                .sub_by_account
                .iter()
                .filter(|(account, _)| !own.contains(*account))
                .collect();
            let other_parts: Vec<&Values> = others.iter().map(|(_, values)| *values).collect();
            let amount = if other_parts.is_empty() {
                Some(0.0)
            } else {
                Values::sum(&other_parts).subscription
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
        for other in orgs {
            if other.identity == org.identity {
                continue;
            }
            let parts: Vec<&Values> = own
                .iter()
                .filter_map(|account| other.sums.sub_by_account.get(account))
                .collect();
            if parts.is_empty() {
                continue;
            }
            match Values::sum(&parts).subscription {
                Some(value) if value > 0.0 => {
                    carried_parts.extend(parts);
                    other_orgs.push(other.name.clone());
                }
                _ => {}
            }
        }
        let carried = if carried_parts.is_empty() {
            Some(0.0)
        } else {
            Values::sum(&carried_parts).subscription
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

    use crate::projects::{ProjectEdit, ProjectsLogRecord};
    use crate::stats::tests::{all, tiny, utc};
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
        assert!(close(atlas.share, 85.714));
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
    fn by_account_of_atlas() {
        let tiny = tiny();
        let page = projects_page_in(&tiny.ctx(), &two_weeks(), &all()).unwrap();

        let atlas = node(&page, "atlas");
        assert_eq!(atlas.by_account.len(), 2);
        let lb = atlas
            .by_account
            .iter()
            .find(|(account, _)| account.as_deref() == Some(&tiny.lb().id))
            .expect("LB's share");
        let ac = atlas
            .by_account
            .iter()
            .find(|(account, _)| account.as_deref() == Some(&tiny.ac().id))
            .expect("AC's share");
        assert!(close(lb.1, 50.0), "LB {}", lb.1);
        assert!(close(ac.1, 50.0), "AC {}", ac.1);
    }

    #[test]
    fn the_matrix() {
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
            matrix.accounts,
            vec![tiny.lb().id.clone(), tiny.ac().id.clone()]
        );
        let cell = |org: &str, account: &str| -> Option<f64> {
            let org = matrix
                .orgs
                .iter()
                .position(|name| name == org)
                .expect("the organization");
            let account = matrix
                .accounts
                .iter()
                .position(|id| id == account)
                .expect("the account");
            matrix.cells[org][account]
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
        assert_eq!(
            atlas.values.subscription, None,
            "the record without an account makes the subscription sum unknown"
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
        assert_eq!(
            atlas.values.api, None,
            "one record without a price makes the sum unknown"
        );
        assert_eq!(
            atlas.values.api_missing.as_deref(),
            Some("no price for claude-x")
        );
        assert_eq!(
            atlas.values.subscription, None,
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
                plan: Some("Team 5x".to_string()),
                uncertain: false,
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
            close(ac.1, 5_000_000.0 / 100.0),
            "AC's 100 % window counts the 3 M of 14 Nov and the 2 M of 17 Nov: {}",
            ac.1
        );
    }
}
