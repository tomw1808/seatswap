//! Projects and organizations (spec §3.6): names, joins, organizations and the
//! shared project log. Everything is computed from the history's session
//! records of every host plus the append-only project log; nothing is stored
//! in `accounts.json`. One writer per host, one line per edit, and the same
//! names on every host once the log has arrived.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::history::{
    AccountRef, HistoryDir, HistoryError, Hosted, Hosts, Kind, Range, Record, SessionRecord,
};
use crate::project::ProjectKey;
use crate::store::Account;

/// A project in the project log and the page: a repository is one project on every host;
/// a folder project is the folder on one host (spec §3.6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectId {
    #[serde(flatten)]
    pub key: ProjectKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl ProjectId {
    /// `host` is kept for a folder key only.
    pub fn of(key: &ProjectKey, host: &str) -> ProjectId {
        ProjectId {
            key: key.clone(),
            host: match key {
                ProjectKey::Repo(_) => None,
                ProjectKey::Folder(_) => Some(host.to_string()),
            },
        }
    }
}

/// One edit of the project log (spec §3.6): what a person changed in the
/// settings. The log is append-only and merged over all hosts; for the same
/// thing the later record wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "edit",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProjectEdit {
    /// A project's shown name, 1..=40 characters.
    Rename {
        key: ProjectId,
        name: String,
    },
    /// `key`'s tokens count for `into`.
    Join {
        key: ProjectId,
        into: ProjectId,
    },
    Unjoin {
        key: ProjectId,
    },
    /// The organization a project belongs to; `None` = unassigned.
    Assign {
        key: ProjectId,
        org: Option<String>,
    },
    /// The shown name of an organization; its identity stays `org`.
    OrgRename {
        org: String,
        name: String,
    },
    /// `org`'s accounts and projects count for `into`.
    OrgJoin {
        org: String,
        into: String,
    },
    /// One account's organization; `None` = no organization.
    AccountOrg {
        account: AccountRef,
        org: Option<String>,
    },
    /// A new organization with no projects and no accounts yet. Its identity
    /// is `name`; a later `OrgRename` can change what is shown.
    OrgAdd {
        name: String,
    },
    /// An organization's projects and accounts go back to none; their own
    /// records are not rewritten. An organization that comes from a
    /// repository owner is refused.
    OrgRemove {
        org: String,
    },
}

impl ProjectEdit {
    /// Check an edit before anything is appended. A refused edit appends
    /// nothing; the page shows the error text under the row.
    pub fn validate(&self) -> Result<(), ProjectsError> {
        match self {
            ProjectEdit::Rename { key, name } => {
                validate_project_id(key)?;
                validate_name("project", name)
            }
            ProjectEdit::OrgRename { name, .. } => validate_name("organization", name),
            ProjectEdit::Join { key, into } => {
                validate_project_id(key)?;
                validate_project_id(into)?;
                if key == into {
                    return Err(ProjectsError::JoinIntoItself { what: "project" });
                }
                Ok(())
            }
            ProjectEdit::OrgJoin { org, into } => {
                validate_identity(org)?;
                if same_organization_name(org, into) {
                    return Err(ProjectsError::JoinIntoItself {
                        what: "organization",
                    });
                }
                validate_identity(into)
            }
            ProjectEdit::Assign { key, org } => {
                validate_project_id(key)?;
                match org {
                    Some(org) => validate_identity(org),
                    None => Ok(()),
                }
            }
            ProjectEdit::AccountOrg { org, .. } => match org {
                Some(org) => validate_identity(org),
                None => Ok(()),
            },
            ProjectEdit::OrgAdd { name } => {
                if name.trim() != name {
                    return Err(ProjectsError::OrganizationNameSpace);
                }
                validate_name("organization", name)
            }
            ProjectEdit::OrgRemove { org } => validate_identity(org),
            ProjectEdit::Unjoin { key } => validate_project_id(key),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectsError {
    #[error("a {what} name must be 1 to 40 characters, not {length}")]
    NameLength { what: &'static str, length: usize },
    #[error("a {what} name must not be only spaces")]
    NameBlank { what: &'static str },
    #[error("an organization name must not be empty")]
    EmptyOrg,
    #[error("a folder project id must name the host it was seen on")]
    FolderWithoutHost,
    #[error("a repository project id is the same on every host, but has host {host}")]
    RepoHasHost { host: String },
    #[error("a {what} cannot be joined into itself")]
    JoinIntoItself { what: &'static str },
    #[error("an organization named {name} already exists")]
    OrganizationExists { name: String },
    #[error("the organization {org} does not exist")]
    UnknownOrganization { org: String },
    #[error("the organization {name} comes from the owner of {repository}; it cannot be removed")]
    OwnerOrganization { name: String, repository: String },
    #[error("an organization name must not start or end with a space")]
    OrganizationNameSpace,
    #[error(transparent)]
    History(#[from] crate::history::HistoryError),
}

/// An id keeps a host only for a folder key: a repository is the same on every
/// host, and a folder without a host is not a project.
fn validate_project_id(id: &ProjectId) -> Result<(), ProjectsError> {
    match (&id.key, &id.host) {
        (ProjectKey::Repo(_), Some(host)) => Err(ProjectsError::RepoHasHost { host: host.clone() }),
        (ProjectKey::Folder(_), None) => Err(ProjectsError::FolderWithoutHost),
        _ => Ok(()),
    }
}

/// The key an organization name is stored under: its Unicode lower case.
fn organization_key(name: &str) -> String {
    name.to_lowercase()
}

/// The one rule for whether two organization names are the same: the Unicode
/// lower case of both, compared. The page and its fixture use `toLowerCase()`
/// for the same rule.
fn same_organization_name(first: &str, second: &str) -> bool {
    organization_key(first) == organization_key(second)
}

/// The organizations one project-log record names by identity, in the order
/// the record writes them. A rename's `name` is the shown name, not an
/// identity, and a self-join is refused, so it names nothing.
fn log_organizations(edit: &ProjectEdit) -> Vec<&str> {
    match edit {
        ProjectEdit::Assign { org: Some(org), .. }
        | ProjectEdit::OrgRename { org, .. }
        | ProjectEdit::OrgRemove { org }
        | ProjectEdit::AccountOrg { org: Some(org), .. } => vec![org],
        ProjectEdit::OrgJoin { org, into } if !same_organization_name(org, into) => {
            vec![org, into]
        }
        ProjectEdit::OrgJoin { .. } => Vec::new(),
        ProjectEdit::OrgAdd { name } => vec![name],
        ProjectEdit::Rename { .. } | ProjectEdit::Join { .. } | ProjectEdit::Unjoin { .. } => {
            Vec::new()
        }
        ProjectEdit::Assign { org: None, .. } | ProjectEdit::AccountOrg { org: None, .. } => {
            Vec::new()
        }
    }
}

/// An organization identity comes from a login's `orgName` or a repository
/// owner, not from typing, so it only has to be non-blank.
fn validate_identity(org: &str) -> Result<(), ProjectsError> {
    if org.trim().is_empty() {
        return Err(ProjectsError::EmptyOrg);
    }
    Ok(())
}

/// A name is 1..=40 Unicode characters, not only spaces, and is never cut.
fn validate_name(what: &'static str, name: &str) -> Result<(), ProjectsError> {
    if name.trim().is_empty() {
        return Err(ProjectsError::NameBlank { what });
    }
    let length = name.chars().count();
    if length > 40 {
        return Err(ProjectsError::NameLength { what, length });
    }
    Ok(())
}

/// One line of the shared project log (spec §3.6: time, what, for what).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsLogRecord {
    pub t: DateTime<Utc>,
    #[serde(flatten)]
    pub edit: ProjectEdit,
}

impl Record for ProjectsLogRecord {
    const KIND: Kind = Kind::Projects;

    fn at(&self) -> DateTime<Utc> {
        self.t
    }
}

/// One project after joins: a project is a repository (or a folder without
/// one), with the folders it was seen in on each host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub key: ProjectId,
    pub name: String,
    pub repo: Option<String>,
    pub org: Option<String>,
    /// The keys joined into this one (spec §3.6), sorted.
    pub joined: Vec<JoinedKey>,
    /// host → the folders seen there, sorted.
    pub folders: BTreeMap<String, Vec<String>>,
    pub sessions: u32,
}

/// A project joined into another, with the name the core gives its key: a
/// joined key has no rename of its own, so its default name is what is shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinedKey {
    pub key: ProjectId,
    /// The key's default name (a joined key's own rename does not show).
    pub name: String,
}

impl Project {
    /// What a key that no session named looks like: its default name, its
    /// repository and owner, and nothing seen.
    fn default_of(id: &ProjectId) -> Project {
        Project {
            key: id.clone(),
            name: default_name(&id.key),
            repo: match &id.key {
                ProjectKey::Repo(repo) => Some(repo.clone()),
                ProjectKey::Folder(_) => None,
            },
            org: default_org(&id.key),
            joined: Vec::new(),
            folders: BTreeMap::new(),
            sessions: 0,
        }
    }
}

/// An organization (spec §3.6): a name for several projects and the accounts
/// that are its subscriptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Org {
    /// The identity the project log names, which a rename does not change. The
    /// page sends this back when it assigns a project or an account.
    pub id: String,
    /// The shown name: `id` unless an `OrgRename` changed it.
    pub name: String,
    pub accounts: Vec<AccountRef>,
    pub projects: u32,
    /// The repositories whose owner this organization is, sorted. One of them
    /// is why the organization cannot be removed (decision 30).
    pub owner_repositories: Vec<String>,
}

/// A stored account with the reference the history uses, so the page can write
/// one `AccountOrg` edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEntry {
    /// The store's local account id (`AccountRow.id` in the snapshot).
    pub id: String,
    pub account: AccountRef,
}

/// Everything the Projects group shows, the same on every host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Projects {
    pub projects: Vec<Project>,
    pub orgs: Vec<Org>,
    /// The stored accounts with their `AccountRef`, for the organization
    /// column of the Accounts group.
    pub account_refs: Vec<AccountEntry>,
    /// What `resolve` computed, so `of` can follow the log for a key that is
    /// not a row of `projects`: the later `Rename`/`Assign` of a key, and the
    /// joins (project and organization) it was put into.
    #[serde(skip)]
    names: BTreeMap<ProjectId, String>,
    /// An assignment with the log order that made it; `None` is the key's
    /// default organization, which is older than every record.
    #[serde(skip)]
    assigned: BTreeMap<ProjectId, (Option<String>, Option<usize>)>,
    #[serde(skip)]
    joins: BTreeMap<ProjectId, (ProjectId, usize)>,
    #[serde(skip)]
    org_join: BTreeMap<String, (String, usize)>,
    /// The first spelling seen for each organization key, so an id keeps the
    /// name as it was typed while the maps above compare lower-cased names.
    #[serde(skip)]
    org_spelling: BTreeMap<String, String>,
    /// The log order of the latest `OrgRemove` that names each identity.
    #[serde(skip)]
    org_removed: BTreeMap<String, usize>,
    /// Organization roots that come from a repository owner: an `OrgRemove`
    /// for one of them is refused.
    #[serde(skip)]
    owner_roots: BTreeSet<String>,
}

/// The projects of a history folder, all of time and every host: the project
/// log and the session records, resolved with the stored accounts. The read
/// warnings are returned with them, in the order the reads make them.
pub fn load(
    history: &HistoryDir,
    accounts: &[Account],
) -> Result<(Projects, Vec<String>), HistoryError> {
    let range = Range {
        from: DateTime::<Utc>::MIN_UTC,
        to: DateTime::<Utc>::MAX_UTC,
    };
    let log = history.read::<ProjectsLogRecord>(&range, &Hosts::All)?;
    let sessions = history.read::<SessionRecord>(&range, &Hosts::All)?;
    let mut warnings = log.warnings;
    warnings.extend(sessions.warnings);
    Ok((
        Projects::resolve(&log.records, &sessions.records, accounts),
        warnings,
    ))
}

impl Projects {
    /// Defaults from the sessions and the stored accounts, then every log
    /// record in time order (ties: host name, then the file's line order). For
    /// the same thing the later record wins; a join follows the chain, and a
    /// cycle drops the earlier of the records that form it.
    ///
    /// An account is in one organization. It starts in the root of its
    /// `org_name` (none without one); then each `AccountOrg` record, in log
    /// order, puts that account in the root of its organization, or in none.
    /// Nothing else moves an account.
    ///
    /// Organization names that differ only in case are one organization, in a
    /// 1.1.0 history too: an owner spelled `Acme` and a login organization
    /// named `acme` are the same organization, because GitHub owners are
    /// case-insensitive. The first spelling in the history's order gives its
    /// id and name; a stored account's `org_name` spells an organization only
    /// when no session or log record names it.
    pub fn resolve(
        log: &[Hosted<ProjectsLogRecord>],
        sessions: &[Hosted<SessionRecord>],
        accounts: &[Account],
    ) -> Projects {
        // What the sessions named: folders per host and distinct session ids.
        // A folder key carries its host; a repository key is the same on every
        // host (spec §3.6).
        let mut folders: BTreeMap<ProjectId, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        let mut sessions_of: BTreeMap<ProjectId, BTreeSet<String>> = BTreeMap::new();
        for hosted in sessions {
            let key = ProjectId::of(&hosted.record.project, &hosted.host);
            folders
                .entry(key.clone())
                .or_default()
                .entry(hosted.host.clone())
                .or_default()
                .insert(hosted.record.cwd.clone());
            sessions_of
                .entry(key)
                .or_default()
                .insert(hosted.record.session.clone());
        }

        // The log, in the order it applies: time, then host, then the order
        // the records arrived in (the file's line order for one host).
        let mut ordered: Vec<&Hosted<ProjectsLogRecord>> = log.iter().collect();
        ordered.sort_by(|a, b| {
            a.record
                .t
                .cmp(&b.record.t)
                .then_with(|| a.host.cmp(&b.host))
        });

        let mut names: BTreeMap<ProjectId, String> = BTreeMap::new();
        let mut assigned: BTreeMap<ProjectId, (Option<String>, Option<usize>)> = BTreeMap::new();
        let mut joins: BTreeMap<ProjectId, (ProjectId, usize)> = BTreeMap::new();
        let mut org_join: BTreeMap<String, (String, usize)> = BTreeMap::new();
        let mut org_names: BTreeMap<String, String> = BTreeMap::new();
        // The first spelling of every organization identity comes from the
        // shared data only: the sessions and the log, in the history's order
        // (time, then host, then the file's line order). A stored account is
        // local to one host, so its `org_name` is the last fallback below.
        let mut named: Vec<(DateTime<Utc>, String, String)> = Vec::new();
        for hosted in sessions {
            if let Some(owner) = default_org(&hosted.record.project) {
                named.push((hosted.record.t, hosted.host.clone(), owner));
            }
        }
        for hosted in log {
            for org in log_organizations(&hosted.record.edit) {
                named.push((hosted.record.t, hosted.host.clone(), org.to_string()));
            }
        }
        // Stable: equal times on one host keep the file's line order, and a
        // session and a log record with the same time and host keep the
        // session first, as both are read from their own file in line order.
        named.sort_by(|first, second| first.0.cmp(&second.0).then_with(|| first.1.cmp(&second.1)));
        let mut org_spelling: BTreeMap<String, String> = BTreeMap::new();
        for (_, _, org) in &named {
            org_spelling
                .entry(organization_key(org))
                .or_insert_with(|| org.clone());
        }
        // Lower-cased name: the first spelling added and the latest add's
        // order. Two adds that name the same organization (decision 30's name
        // rule) are one organization: the first spelling gives the id and the
        // name, the latest add decides whether it exists after a removal.
        let mut org_added: BTreeMap<String, (String, usize)> = BTreeMap::new();
        let mut org_removed: BTreeMap<String, usize> = BTreeMap::new();
        // The per-account organization edits in the order they apply, after
        // every `OrgJoin` is resolved: the latest record for an account wins.
        let mut account_org_edits: Vec<(AccountRef, Option<String>, Option<usize>)> = Vec::new();
        for (order, hosted) in ordered.into_iter().enumerate() {
            let position = Some(order);
            match &hosted.record.edit {
                ProjectEdit::Rename { key, name } => {
                    names.insert(key.clone(), name.clone());
                }
                ProjectEdit::Join { key, into } => {
                    if key == into {
                        // Refused by `validate`; a hand-written line moves
                        // nothing rather than joining a project into itself.
                        continue;
                    }
                    break_join_cycle(&mut joins, key, into);
                    joins.insert(key.clone(), (into.clone(), order));
                }
                ProjectEdit::Unjoin { key } => {
                    joins.remove(key);
                }
                ProjectEdit::Assign { key, org } => {
                    assigned.insert(
                        key.clone(),
                        (org.as_ref().map(|org| organization_key(org)), position),
                    );
                }
                ProjectEdit::OrgRename { org, name } => {
                    org_names.insert(organization_key(org), name.clone());
                }
                ProjectEdit::OrgJoin { org, into } => {
                    if same_organization_name(org, into) {
                        // Refused by `validate`; a hand-written line moves
                        // nothing rather than joining an organization into
                        // itself.
                        continue;
                    }
                    let from = organization_key(org);
                    let to = organization_key(into);
                    break_join_cycle(&mut org_join, &from, &to);
                    org_join.insert(from, (to, order));
                }
                ProjectEdit::AccountOrg { account, org } => {
                    account_org_edits.push((
                        account.clone(),
                        org.as_ref().map(|org| organization_key(org)),
                        position,
                    ));
                }
                ProjectEdit::OrgAdd { name } => {
                    // The first spelling is the identity; a later add of the
                    // same name only moves the latest add's order.
                    match org_added.get_mut(&organization_key(name)) {
                        Some((_, latest)) => *latest = order,
                        None => {
                            org_added.insert(organization_key(name), (name.clone(), order));
                        }
                    }
                }
                ProjectEdit::OrgRemove { org } => {
                    org_removed.insert(organization_key(org), order);
                }
            }
        }

        // Every key the sessions or the log named, grouped under the key it is
        // joined into.
        let mut keys: BTreeSet<ProjectId> = folders.keys().cloned().collect();
        keys.extend(joins.keys().cloned());
        keys.extend(joins.values().map(|(into, _)| into.clone()));

        let mut groups: BTreeMap<ProjectId, Vec<ProjectId>> = BTreeMap::new();
        for key in keys {
            groups.entry(join_root(&joins, &key)).or_default().push(key);
        }

        // A repository key named only in the log still has an owner spelling.
        for key in groups.values().flatten() {
            if let Some(owner) = default_org(&key.key) {
                org_spelling
                    .entry(organization_key(&owner))
                    .or_insert(owner);
            }
        }

        // A stored account's `org_name` is local to its host: it spells an
        // organization only when no session or log record named it.
        for account in accounts {
            if let Some(org) = &account.org_name {
                if !org.trim().is_empty() {
                    org_spelling
                        .entry(organization_key(org))
                        .or_insert_with(|| org.clone());
                }
            }
        }

        // The organizations that come from a repository owner (decision 30):
        // only a project as it resolves now counts, so a joined member is out
        // (its group's root repository is what the project shows) and a
        // project assigned elsewhere is out. An organization whose
        // repositories all went into other projects can be removed.
        let mut owner_repos: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (root, members) in &groups {
            if !members.iter().any(|key| sessions_of.contains_key(key)) {
                // Nothing was ever seen in this project: it is not a row.
                continue;
            }
            let Some(owner) = default_org(&root.key) else {
                continue;
            };
            let ProjectKey::Repo(repo) = &root.key else {
                continue;
            };
            let owner_root = join_root(&org_join, &organization_key(&owner));
            let raw_org = match assigned.get(root) {
                Some((org, _)) => org.clone(),
                None => default_org_key(&root.key),
            };
            let Some(raw_org) = raw_org else {
                continue;
            };
            let project_root = join_root(&org_join, &raw_org);
            if same_organization_name(&project_root, &owner_root) {
                owner_repos
                    .entry(owner_root)
                    .or_default()
                    .insert(repo.clone());
            }
        }
        let owner_roots: BTreeSet<String> = owner_repos.keys().cloned().collect();

        let mut projects: Vec<Project> = Vec::new();
        for (root, members) in groups {
            if !members.iter().any(|key| sessions_of.contains_key(key)) {
                // Nothing was ever seen in this project: it is not a row.
                continue;
            }
            let mut project_folders: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            let mut session_ids: BTreeSet<String> = BTreeSet::new();
            for member in &members {
                if let Some(by_host) = folders.get(member) {
                    for (host, cwds) in by_host {
                        project_folders
                            .entry(host.clone())
                            .or_default()
                            .extend(cwds.iter().cloned());
                    }
                }
                if let Some(ids) = sessions_of.get(member) {
                    session_ids.extend(ids.iter().cloned());
                }
            }
            let joined = members
                .iter()
                .filter(|member| **member != root)
                .map(|member| JoinedKey {
                    key: member.clone(),
                    name: default_name(&member.key),
                })
                .collect();
            let (raw_org, org_order) = match assigned.get(&root) {
                Some((org, order)) => (org.clone(), *order),
                None => (default_org_key(&root.key), None),
            };
            let org = surviving_org(
                raw_org.as_deref(),
                org_order,
                &org_join,
                &org_removed,
                &owner_roots,
            )
            .map(|root| spelled(&org_spelling, &root));
            projects.push(Project {
                name: names
                    .get(&root)
                    .cloned()
                    .unwrap_or_else(|| default_name(&root.key)),
                repo: match &root.key {
                    ProjectKey::Repo(repo) => Some(repo.clone()),
                    ProjectKey::Folder(_) => None,
                },
                org,
                joined,
                key: root,
                folders: project_folders
                    .into_iter()
                    .map(|(host, cwds)| (host, cwds.into_iter().collect()))
                    .collect(),
                sessions: session_ids.len() as u32,
            });
        }

        // An organization is its identity; a join or a rename does not change
        // the identity a later edit can name. Every `OrgJoin` is resolved
        // first, so each identity has its root.
        let mut identities: BTreeSet<String> = BTreeSet::new();
        for project in &projects {
            if let Some(org) = &project.org {
                identities.insert(organization_key(org));
            }
        }
        for account in accounts {
            if let Some(org) = &account.org_name {
                if !org.trim().is_empty() {
                    identities.insert(organization_key(org));
                }
            }
        }
        identities.extend(org_names.keys().cloned());
        identities.extend(org_join.keys().cloned());
        identities.extend(org_join.values().map(|(into, _)| into.clone()));
        identities.extend(
            account_org_edits
                .iter()
                .filter_map(|(_, org, _)| org.clone()),
        );

        // An `OrgAdd` makes an organization with no project and no account;
        // its identity is the first spelling added. It never makes a second
        // organization with a name that is already there (decision 30: that
        // edit is refused). The latest add decides whether the organization
        // exists after its latest removal: a later add brings it back empty.
        let shown_name = |identity: &str| {
            org_names
                .get(identity)
                .cloned()
                .unwrap_or_else(|| identity.to_string())
        };
        let mut added_roots: BTreeSet<String> = BTreeSet::new();
        for (name, latest_add) in org_added.values() {
            let key = organization_key(name);
            let root = join_root(&org_join, &key);
            if removal_order(&org_join, &org_removed, &root, &owner_roots)
                .is_some_and(|removed_at| removed_at > *latest_add)
            {
                continue;
            }
            let collides = identities.iter().any(|other| {
                *other != key
                    && (same_organization_name(other, name)
                        || same_organization_name(&shown_name(other), name))
            });
            if collides {
                continue;
            }
            identities.insert(key);
            added_roots.insert(root);
        }

        // One rule decides an account's organization. It starts in the root of
        // its `org_name` (none when it has no `org_name`, or when that
        // organization was removed). Then each `AccountOrg` record, in log
        // order, puts that one account in the record's root, or in none.
        // Nothing else moves it, so an account only another host stores keeps
        // its own organization.
        let mut account_org: BTreeMap<AccountRef, String> = BTreeMap::new();
        for account in accounts {
            if let Some(org) = &account.org_name {
                if !org.trim().is_empty() {
                    let org = organization_key(org);
                    if let Some(root) = surviving_org(
                        Some(org.as_str()),
                        None,
                        &org_join,
                        &org_removed,
                        &owner_roots,
                    ) {
                        account_org.insert(AccountRef::of(account), root);
                    }
                }
            }
        }
        for (account, org, order) in &account_org_edits {
            match org {
                Some(org) => {
                    match surviving_org(Some(org), *order, &org_join, &org_removed, &owner_roots) {
                        Some(root) => {
                            account_org.insert(account.clone(), root);
                        }
                        None => {
                            account_org.remove(account);
                        }
                    }
                }
                None => {
                    account_org.remove(account);
                }
            }
        }

        // The roots that still have a source after their latest removal: a
        // later assignment, account edit or `OrgAdd` brings the organization
        // back; a removal alone leaves none of them.
        let mut live_roots: BTreeSet<String> = added_roots;
        live_roots.extend(account_org.values().cloned());
        for project in &projects {
            if let Some(org) = &project.org {
                live_roots.insert(organization_key(org));
            }
        }

        let mut by_root: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for identity in identities {
            let root = join_root(&org_join, &identity);
            if removal_order(&org_join, &org_removed, &root, &owner_roots).is_some()
                && !live_roots.contains(&root)
            {
                continue;
            }
            by_root.entry(root).or_default().push(identity);
        }

        let mut accounts_in: BTreeMap<String, BTreeSet<AccountRef>> = BTreeMap::new();
        for (account, root) in account_org {
            accounts_in.entry(root).or_default().insert(account);
        }

        let mut orgs: Vec<Org> = Vec::new();
        for (root, _group) in by_root {
            let name = org_names
                .get(&root)
                .cloned()
                .unwrap_or_else(|| spelled(&org_spelling, &root));
            let project_count = projects
                .iter()
                .filter(|project| {
                    project
                        .org
                        .as_deref()
                        .is_some_and(|org| same_organization_name(org, &root))
                })
                .count();
            orgs.push(Org {
                name,
                accounts: accounts_in
                    .remove(&root)
                    .map(|own| own.into_iter().collect())
                    .unwrap_or_default(),
                projects: project_count as u32,
                owner_repositories: owner_repos
                    .get(&root)
                    .map(|repos| repos.iter().cloned().collect())
                    .unwrap_or_default(),
                id: spelled(&org_spelling, &root),
            });
        }
        orgs.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

        // Assigned organizations first, then by name, as the mock-up lists them.
        let org_label = |org: &Option<String>| match org {
            Some(identity) => org_names
                .get(&organization_key(identity))
                .cloned()
                .unwrap_or_else(|| identity.clone()),
            None => String::new(),
        };
        projects.sort_by(|a, b| {
            a.org
                .is_none()
                .cmp(&b.org.is_none())
                .then_with(|| org_label(&a.org).cmp(&org_label(&b.org)))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.key.cmp(&b.key))
        });

        let account_refs = accounts
            .iter()
            .map(|account| AccountEntry {
                id: account.id.clone(),
                account: AccountRef::of(account),
            })
            .collect();

        Projects {
            projects,
            orgs,
            account_refs,
            names,
            assigned,
            joins,
            org_join,
            org_spelling,
            org_removed,
            owner_roots,
        }
    }

    /// The project `id` belongs to after joins: the one it is joined into.
    /// An id no session named (a token record of a project seen only there)
    /// follows the log too: its default, with a later name or assignment, is
    /// not stored in `projects`.
    pub fn of(&self, id: &ProjectId) -> Project {
        let root = join_root(&self.joins, id);
        if let Some(project) = self.projects.iter().find(|project| project.key == root) {
            return project.clone();
        }
        let mut project = Project::default_of(&root);
        if let Some(name) = self.names.get(&root) {
            project.name = name.clone();
        }
        let (org, order) = match self.assigned.get(&root) {
            Some((org, order)) => (org.clone(), *order),
            None => (default_org_key(&root.key), None),
        };
        project.org = surviving_org(
            org.as_deref(),
            order,
            &self.org_join,
            &self.org_removed,
            &self.owner_roots,
        )
        .map(|org_root| {
            if let Some(name) = self.org_spelling.get(&org_root) {
                return name.clone();
            }
            match default_org(&root.key) {
                Some(owner) if same_organization_name(&owner, &org_root) => owner,
                _ => org_root,
            }
        });
        project
    }

    /// Check an edit against what is resolved now: `OrgAdd` refuses a name
    /// that already exists, and `OrgRemove` refuses an organization that comes
    /// from a repository owner (decision 30). The structural checks of
    /// [`ProjectEdit::validate`] run first. A refused edit appends nothing.
    pub fn validate_edit(&self, edit: &ProjectEdit) -> Result<(), ProjectsError> {
        edit.validate()?;
        match edit {
            ProjectEdit::OrgAdd { name } => {
                let exists = self.orgs.iter().any(|org| {
                    same_organization_name(&org.id, name) || same_organization_name(&org.name, name)
                });
                if exists {
                    return Err(ProjectsError::OrganizationExists {
                        name: name.to_string(),
                    });
                }
                Ok(())
            }
            ProjectEdit::OrgRemove { org } => {
                let Some(found) = self.orgs.iter().find(|found| {
                    same_organization_name(&found.id, org)
                        || same_organization_name(&found.name, org)
                }) else {
                    return Err(ProjectsError::UnknownOrganization { org: org.clone() });
                };
                if let Some(repository) = found.owner_repositories.first() {
                    return Err(ProjectsError::OwnerOrganization {
                        name: found.name.clone(),
                        repository: repository.clone(),
                    });
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// The log order of the latest `OrgRemove` that names `root`, following
/// `OrgJoin`, or `None` when no removal applies. An organization that comes
/// from a repository owner is never removed (decision 30).
fn removal_order(
    org_join: &BTreeMap<String, (String, usize)>,
    org_removed: &BTreeMap<String, usize>,
    root: &str,
    owner_roots: &BTreeSet<String>,
) -> Option<usize> {
    if owner_roots
        .iter()
        .any(|owner| same_organization_name(owner, root))
    {
        return None;
    }
    org_removed
        .iter()
        .filter(|(identity, _)| {
            let removed_root = join_root(org_join, *identity);
            same_organization_name(&removed_root, root)
        })
        .map(|(_, order)| *order)
        .max()
}

/// The root an organization source still resolves to after a removal: the
/// source itself when the removal came before it, `None` when the removal came
/// after it. `order` is `None` for a default (a login's `orgName` or a
/// repository owner), which every removal is later than.
fn surviving_org(
    org: Option<&str>,
    order: Option<usize>,
    org_join: &BTreeMap<String, (String, usize)>,
    org_removed: &BTreeMap<String, usize>,
    owner_roots: &BTreeSet<String>,
) -> Option<String> {
    let identity = org?;
    let root = join_root(org_join, &organization_key(identity));
    if let Some(removed_at) = removal_order(org_join, org_removed, &root, owner_roots) {
        match order {
            Some(source) if removed_at <= source => {}
            _ => return None,
        }
    }
    Some(root)
}

/// A `Join` of `key` into `into` closes a cycle when `into`'s chain leads back
/// to `key`. Such a record drops the earliest record that forms the cycle (the
/// later one is kept), so the chain can be followed again. Project keys and
/// organization identities use this one rule.
fn break_join_cycle<K: Ord + Clone>(joins: &mut BTreeMap<K, (K, usize)>, key: &K, into: &K) {
    loop {
        let mut edges: Vec<(K, usize)> = Vec::new();
        let mut seen: BTreeSet<K> = BTreeSet::new();
        let mut node = into.clone();
        let mut closes = node == *key;
        while !closes {
            let Some((next, order)) = joins.get(&node) else {
                break;
            };
            if !seen.insert(node.clone()) {
                // A cycle that was written by hand and does not name `key`:
                // nothing of this record's chain can be dropped for it.
                return;
            }
            edges.push((node.clone(), *order));
            node = next.clone();
            closes = node == *key;
        }
        if !closes {
            return;
        }
        let Some((earliest, _)) = edges.iter().min_by_key(|(_, order)| *order) else {
            // `into` already is `key`: there is no earlier record to drop.
            return;
        };
        let earliest = earliest.clone();
        joins.remove(&earliest);
    }
}

/// The key `key` is joined into, following the chain.
fn join_root<K: Ord + Clone>(joins: &BTreeMap<K, (K, usize)>, key: &K) -> K {
    let mut current = key.clone();
    let mut seen: BTreeSet<K> = BTreeSet::new();
    while let Some((into, _)) = joins.get(&current) {
        if !seen.insert(current.clone()) {
            break;
        }
        current = into.clone();
    }
    current
}

/// The owner and name of a repository address, the one place this split is
/// made: `github.com/acme/acme-kb` → (`Some("acme")`, `"acme-kb"`). The
/// address is already normalized by [`crate::project`]: no protocol, no
/// `.git`, host first.
fn owner_and_name(repo: &str) -> (Option<String>, String) {
    let parts: Vec<&str> = repo.split('/').filter(|part| !part.is_empty()).collect();
    match parts.split_last() {
        Some((name, rest)) => (
            rest.last().map(|owner| (*owner).to_string()),
            (*name).to_string(),
        ),
        None => (None, repo.to_string()),
    }
}

/// A key's default name: the repository's name, or the folder's name. The home
/// folder's name is `~ (home folder)`, as the mock-up shows it.
pub fn default_name(key: &ProjectKey) -> String {
    match key {
        ProjectKey::Repo(repo) => owner_and_name(repo).1,
        ProjectKey::Folder(name) if name == "~" => "~ (home folder)".to_string(),
        ProjectKey::Folder(name) => name.clone(),
    }
}

/// A key's default organization: a repository's owner; a folder has none.
fn default_org(key: &ProjectKey) -> Option<String> {
    match key {
        ProjectKey::Repo(repo) => owner_and_name(repo).0,
        ProjectKey::Folder(_) => None,
    }
}

/// A key's default organization as the lower-cased identity it is compared
/// and stored under.
fn default_org_key(key: &ProjectKey) -> Option<String> {
    default_org(key).map(|name| organization_key(&name))
}

/// The spelling an organization identity is shown and sent by: the first one
/// seen, or the key itself when the organization was never named.
fn spelled(spelling: &BTreeMap<String, String>, key: &str) -> String {
    spelling
        .get(key)
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse::<DateTime<Utc>>().expect("a test time")
    }

    fn repo(id: &str) -> ProjectId {
        ProjectId {
            key: ProjectKey::Repo(id.to_string()),
            host: None,
        }
    }

    fn folder(name: &str, host: &str) -> ProjectId {
        ProjectId::of(&ProjectKey::Folder(name.to_string()), host)
    }

    const SESSION_A: &str = "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34";
    const SESSION_B: &str = "2b7d9c1e-5a4f-4c3b-9e8d-7f6a5b4c3d2e";
    const AC_ACC: &str = "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04";
    const AC_ORG: &str = "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20";
    const PR_ACC: &str = "4d8b1f3a-7c2e-4a9d-8b6f-1e5c3a7d9b42";
    const PR_ORG: &str = "9e3c5a7f-1b2d-4e8a-9c6f-3a1d5b7e9c24";
    const LB_ACC: &str = "1a7f3c5e-9b2d-4f6a-8c1e-5d3b7a9f2c64";
    const LB_ORG: &str = "7c1e5a9b-3d2f-4a8c-9e6b-1f5d3c7a9b28";

    fn session(
        host: &str,
        at: &str,
        id: &str,
        cwd: &str,
        project: ProjectId,
    ) -> Hosted<SessionRecord> {
        Hosted {
            host: host.to_string(),
            record: SessionRecord {
                t: utc(at),
                session: id.to_string(),
                cwd: cwd.to_string(),
                project: project.key,
                entrypoint: Some("cli".to_string()),
                first: utc(at),
                last: utc(at),
                agents_started: 0,
                agents_finished: 0,
                closed: false,
            },
        }
    }

    fn edit(host: &str, at: &str, edit: ProjectEdit) -> Hosted<ProjectsLogRecord> {
        Hosted {
            host: host.to_string(),
            record: ProjectsLogRecord { t: utc(at), edit },
        }
    }

    fn account(id: &str, org_name: Option<&str>) -> Account {
        account_with(
            id,
            AC_ACC,
            AC_ORG,
            "Acme",
            "AC",
            "sam@acme.example",
            org_name,
        )
    }

    fn account_with(
        id: &str,
        account_uuid: &str,
        organization_uuid: &str,
        label: &str,
        abbr: &str,
        email: &str,
        org_name: Option<&str>,
    ) -> Account {
        serde_json::from_value(json!({
            "id": id,
            "accountUuid": account_uuid,
            "organizationUuid": organization_uuid,
            "label": label,
            "abbr": abbr,
            "color": "#3b82f6",
            "email": email,
            "orgName": org_name,
            "plan": "Team 5x",
            "oauthAccount": {
                "accountUuid": account_uuid,
                "organizationUuid": organization_uuid,
                "emailAddress": email
            },
            "addedAt": "2026-09-01T00:00:00Z",
            "loginState": "ok",
            "autoBurn": true
        }))
        .expect("a test account")
    }

    fn acme_account() -> AccountRef {
        AccountRef::of(&account("acme-local", Some("Acme Inc.")))
    }

    #[test]
    fn a_repo_gets_its_name_and_owner() {
        let key = repo("github.com/acme/acme-kb");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/acme-kb",
            key.clone(),
        )];
        let projects = Projects::resolve(&[], &sessions, &[]);
        assert_eq!(projects.projects.len(), 1);
        let project = &projects.projects[0];
        assert_eq!(project.name, "acme-kb");
        assert_eq!(project.org.as_deref(), Some("acme"));
        assert_eq!(project.repo.as_deref(), Some("github.com/acme/acme-kb"));
        assert_eq!(project.key, key);
    }

    #[test]
    fn a_folder_has_no_organization() {
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/notes",
            folder("notes", "atlas"),
        )];
        let projects = Projects::resolve(&[], &sessions, &[]);
        assert_eq!(projects.projects.len(), 1);
        assert_eq!(projects.projects[0].name, "notes");
        assert_eq!(projects.projects[0].org, None);
        assert_eq!(projects.projects[0].repo, None);
    }

    #[test]
    fn the_home_folder_has_its_own_name() {
        let key = folder("~", "atlas");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam",
            key.clone(),
        )];
        let projects = Projects::resolve(&[], &sessions, &[]);
        assert_eq!(projects.projects.len(), 1);
        assert_eq!(projects.projects[0].name, "~ (home folder)");
        assert_eq!(projects.of(&key).name, "~ (home folder)");
    }

    #[test]
    fn folders_are_listed_per_host() {
        let key = repo("github.com/sam/atlas");
        let sessions = vec![
            session(
                "atlas",
                "2026-11-14T10:00:00Z",
                SESSION_A,
                "/home/sam/projects/atlas",
                key.clone(),
            ),
            session(
                "vega",
                "2026-11-14T11:00:00Z",
                SESSION_B,
                "/home/sam/src/atlas",
                key.clone(),
            ),
        ];
        let projects = Projects::resolve(&[], &sessions, &[]);
        assert_eq!(projects.projects.len(), 1);
        let project = &projects.projects[0];
        assert_eq!(
            project.folders,
            BTreeMap::from([
                (
                    "atlas".to_string(),
                    vec!["/home/sam/projects/atlas".to_string()]
                ),
                ("vega".to_string(), vec!["/home/sam/src/atlas".to_string()]),
            ])
        );
        assert_eq!(project.sessions, 2);
    }

    #[test]
    fn a_rename_on_one_host_shows_on_both() {
        let key = repo("github.com/sam/atlas");
        let sessions = vec![
            session(
                "atlas",
                "2026-11-14T10:00:00Z",
                SESSION_A,
                "/home/sam/projects/atlas",
                key.clone(),
            ),
            session(
                "vega",
                "2026-11-14T11:00:00Z",
                SESSION_B,
                "/home/sam/src/atlas",
                key.clone(),
            ),
        ];
        let log = vec![edit(
            "vega",
            "2026-11-14T12:00:00Z",
            ProjectEdit::Rename {
                key: key.clone(),
                name: "Atlas engine".to_string(),
            },
        )];
        let projects = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(projects.projects.len(), 1);
        assert_eq!(projects.projects[0].name, "Atlas engine");
    }

    #[test]
    fn the_later_edit_wins() {
        let key = repo("github.com/sam/atlas");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/atlas",
            key.clone(),
        )];
        let rename = |host: &str, at: &str, name: &str| {
            edit(
                host,
                at,
                ProjectEdit::Rename {
                    key: key.clone(),
                    name: name.to_string(),
                },
            )
        };
        let later_b = vec![
            rename("atlas", "2026-11-14T10:00:00Z", "A"),
            rename("vega", "2026-11-14T10:05:00Z", "B"),
        ];
        assert_eq!(
            Projects::resolve(&later_b, &sessions, &[]).projects[0].name,
            "B"
        );

        let later_a = vec![
            rename("atlas", "2026-11-14T10:05:00Z", "A"),
            rename("vega", "2026-11-14T10:00:00Z", "B"),
        ];
        assert_eq!(
            Projects::resolve(&later_a, &sessions, &[]).projects[0].name,
            "A"
        );
    }

    #[test]
    fn join_moves_the_sessions() {
        let atlas = repo("github.com/sam/atlas");
        let old = folder("atlas-old", "atlas");
        let sessions = vec![
            session(
                "atlas",
                "2026-11-14T10:00:00Z",
                SESSION_A,
                "/home/sam/projects/atlas",
                atlas.clone(),
            ),
            session(
                "atlas",
                "2026-11-14T11:00:00Z",
                SESSION_B,
                "/home/sam/projects/atlas-old",
                old.clone(),
            ),
        ];
        let log = vec![edit(
            "atlas",
            "2026-11-14T12:00:00Z",
            ProjectEdit::Join {
                key: old.clone(),
                into: atlas.clone(),
            },
        )];
        let projects = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(projects.projects.len(), 1);
        let project = &projects.projects[0];
        assert_eq!(project.key, atlas);
        assert_eq!(
            project
                .joined
                .iter()
                .map(|joined| joined.key.clone())
                .collect::<Vec<ProjectId>>(),
            vec![old.clone()]
        );
        assert_eq!(project.joined[0].name, "atlas-old");
        assert_eq!(project.sessions, 2);
        assert_eq!(projects.of(&old), *project);
    }

    #[test]
    fn a_join_cycle_keeps_the_later_record() {
        let a = folder("a", "atlas");
        let b = folder("b", "atlas");
        let sessions = vec![
            session(
                "atlas",
                "2026-11-14T09:00:00Z",
                SESSION_A,
                "/home/sam/a",
                a.clone(),
            ),
            session(
                "atlas",
                "2026-11-14T09:10:00Z",
                SESSION_B,
                "/home/sam/b",
                b.clone(),
            ),
        ];
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::Join {
                    key: a.clone(),
                    into: b.clone(),
                },
            ),
            edit(
                "vega",
                "2026-11-14T10:05:00Z",
                ProjectEdit::Join {
                    key: b.clone(),
                    into: a.clone(),
                },
            ),
        ];
        let projects = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(projects.projects.len(), 1);
        let project = &projects.projects[0];
        assert_eq!(project.key, a);
        assert_eq!(
            project
                .joined
                .iter()
                .map(|joined| joined.key.clone())
                .collect::<Vec<ProjectId>>(),
            vec![b]
        );
        assert_eq!(project.sessions, 2);
    }

    #[test]
    fn an_account_org_and_a_repo_owner_can_be_joined() {
        let key = repo("github.com/acme/acme-kb");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/acme-kb",
            key.clone(),
        )];
        let accounts = vec![account("acme-local", Some("Acme Inc."))];
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:00:00Z",
            ProjectEdit::OrgJoin {
                org: "Acme Inc.".to_string(),
                into: "acme".to_string(),
            },
        )];
        let projects = Projects::resolve(&log, &sessions, &accounts);
        assert_eq!(projects.orgs.len(), 1, "{:?}", projects.orgs);
        let org = &projects.orgs[0];
        assert_eq!(org.name, "acme");
        assert_eq!(org.accounts, vec![acme_account()]);
        assert_eq!(org.projects, 1);
        assert_eq!(projects.projects[0].org.as_deref(), Some("acme"));
    }

    #[test]
    fn assign_none_makes_it_unassigned() {
        let key = repo("github.com/acme/acme-kb");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/acme-kb",
            key.clone(),
        )];
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:00:00Z",
            ProjectEdit::Assign {
                key: key.clone(),
                org: None,
            },
        )];
        let projects = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(projects.projects[0].org, None);
        assert!(projects.orgs.is_empty());
    }

    #[test]
    fn of_an_unknown_key_is_its_default() {
        let key = repo("github.com/sam/atlas");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/atlas",
            key.clone(),
        )];
        let projects = Projects::resolve(&[], &sessions, &[]);

        let unknown = repo("github.com/acme/acme-notes");
        assert_eq!(
            projects.of(&unknown),
            Project {
                key: unknown.clone(),
                name: "acme-notes".to_string(),
                repo: Some("github.com/acme/acme-notes".to_string()),
                org: Some("acme".to_string()),
                joined: Vec::new(),
                folders: BTreeMap::new(),
                sessions: 0,
            }
        );

        let unknown_folder = folder("notes", "atlas");
        assert_eq!(
            projects.of(&unknown_folder),
            Project {
                key: unknown_folder,
                name: "notes".to_string(),
                repo: None,
                org: None,
                joined: Vec::new(),
                folders: BTreeMap::new(),
                sessions: 0,
            }
        );
    }

    #[test]
    fn a_log_line_round_trips_through_its_short_keys() {
        let record = ProjectsLogRecord {
            t: utc("2026-11-14T10:00:00Z"),
            edit: ProjectEdit::Join {
                key: folder("atlas-old", "atlas"),
                into: repo("github.com/sam/atlas"),
            },
        };
        let line = serde_json::to_string(&record).expect("a log line");
        assert!(line.contains("\"edit\":\"join\""), "{line}");
        assert!(line.contains("\"t\":\"2026-11-14T10:00:00Z\""), "{line}");
        assert!(
            line.contains("\"kind\":\"folder\",\"id\":\"atlas-old\",\"host\":\"atlas\""),
            "{line}"
        );
        assert!(
            line.contains("\"into\":{\"kind\":\"repo\",\"id\":\"github.com/sam/atlas\"}"),
            "{line}"
        );
        let read: ProjectsLogRecord = serde_json::from_str(&line).expect("the line reads back");
        assert_eq!(read, record);
    }

    #[test]
    fn a_project_id_names_a_host_only_for_a_folder() {
        let folder_key = ProjectKey::Folder("notes".to_string());
        let repo_key = ProjectKey::Repo("github.com/sam/atlas".to_string());
        assert_eq!(
            ProjectId::of(&folder_key, "atlas").host.as_deref(),
            Some("atlas")
        );
        assert_eq!(ProjectId::of(&repo_key, "atlas").host, None);

        let rename = |key: ProjectId| ProjectEdit::Rename {
            key,
            name: "Notes".to_string(),
        };
        assert!(rename(ProjectId {
            key: folder_key,
            host: None,
        })
        .validate()
        .is_err());
        assert!(rename(ProjectId {
            key: repo_key,
            host: Some("atlas".to_string()),
        })
        .validate()
        .is_err());
    }

    #[test]
    fn a_name_is_one_to_forty_characters() {
        let key = folder("atlas", "atlas");
        let rename = |name: &str| ProjectEdit::Rename {
            key: key.clone(),
            name: name.to_string(),
        };
        assert!(rename("").validate().is_err());
        assert!(rename("   ").validate().is_err());
        assert!(rename(&"a".repeat(41)).validate().is_err());
        assert!(rename(&"a".repeat(40)).validate().is_ok());
        // Unicode characters, not bytes: 40 umlauts are 80 bytes.
        assert!(rename(&"ä".repeat(40)).validate().is_ok());
        assert!(rename(&"ä".repeat(41)).validate().is_err());
        assert!(ProjectEdit::Join {
            key: key.clone(),
            into: key.clone()
        }
        .validate()
        .is_err());
        assert!(ProjectEdit::OrgRename {
            org: "acme".to_string(),
            name: "   ".to_string()
        }
        .validate()
        .is_err());
        assert!(ProjectEdit::Assign {
            key: key.clone(),
            org: Some(" ".to_string())
        }
        .validate()
        .is_err());
    }

    #[test]
    fn an_org_rename_keeps_the_identity() {
        let key = repo("github.com/acme/acme-kb");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/acme-kb",
            key,
        )];
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::OrgRename {
                    org: "acme".to_string(),
                    name: "Acme Inc.".to_string(),
                },
            ),
            // A later edit still names the identity and lands on the same org.
            edit(
                "atlas",
                "2026-11-14T10:05:00Z",
                ProjectEdit::AccountOrg {
                    account: acme_account(),
                    org: Some("acme".to_string()),
                },
            ),
        ];
        let projects = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(projects.orgs.len(), 1);
        assert_eq!(projects.orgs[0].id, "acme");
        assert_eq!(projects.orgs[0].name, "Acme Inc.");
        assert_eq!(projects.orgs[0].accounts, vec![acme_account()]);
    }

    fn org_join(at: &str, org: &str, into: &str) -> Hosted<ProjectsLogRecord> {
        edit(
            "atlas",
            at,
            ProjectEdit::OrgJoin {
                org: org.to_string(),
                into: into.to_string(),
            },
        )
    }

    #[test]
    fn proof_an_account_can_leave_a_joined_default_organization() {
        // "Acme Inc." (the account's org_name) is joined into "acme"; the page
        // then moves the one account to no organization.
        let accounts = vec![account("acme-local", Some("Acme Inc."))];
        let log = vec![
            org_join("2026-11-14T10:00:00Z", "Acme Inc.", "acme"),
            edit(
                "atlas",
                "2026-11-14T10:05:00Z",
                ProjectEdit::AccountOrg {
                    account: acme_account(),
                    org: None,
                },
            ),
        ];
        let projects = Projects::resolve(&log, &[], &accounts);
        let holders: Vec<&str> = projects
            .orgs
            .iter()
            .filter(|org| org.accounts.contains(&acme_account()))
            .map(|org| org.id.as_str())
            .collect();
        assert!(holders.is_empty(), "the account is still in {holders:?}");
    }

    #[test]
    fn proof_an_org_join_cycle_keeps_the_later_record() {
        let log = vec![
            org_join("2026-11-14T10:00:00Z", "acme", "zeta"),
            org_join("2026-11-14T10:05:00Z", "zeta", "acme"),
        ];
        let projects = Projects::resolve(&log, &[], &[]);
        assert_eq!(projects.orgs.len(), 1, "{:?}", projects.orgs);
    }

    #[test]
    fn proof_an_org_cannot_be_joined_into_itself() {
        let edit = ProjectEdit::OrgJoin {
            org: "acme".to_string(),
            into: "acme".to_string(),
        };
        assert!(edit.validate().is_err());
    }

    #[test]
    fn an_organization_identity_is_not_held_to_the_name_rule() {
        // The account's orgName comes from the login, not from typing: 41
        // characters are an organization identity all the same.
        let long = format!("{}x", "Acme Inc. ".repeat(4));
        assert_eq!(long.chars().count(), 41);
        assert!(ProjectEdit::Assign {
            key: folder("atlas", "atlas"),
            org: Some(long.clone()),
        }
        .validate()
        .is_ok());
        assert!(ProjectEdit::AccountOrg {
            account: acme_account(),
            org: Some(long.clone()),
        }
        .validate()
        .is_ok());
        assert!(ProjectEdit::OrgJoin {
            org: long.clone(),
            into: "acme".to_string(),
        }
        .validate()
        .is_ok());
        assert!(ProjectEdit::OrgJoin {
            org: "acme".to_string(),
            into: long.clone(),
        }
        .validate()
        .is_ok());
        assert!(ProjectEdit::OrgRename {
            org: long.clone(),
            name: "Acme".to_string(),
        }
        .validate()
        .is_ok());

        // A 41-character orgName and the edits that name it all work.
        let key = folder("atlas", "atlas");
        let accounts = vec![account("acme-local", Some(&long))];
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::Assign {
                    key: key.clone(),
                    org: Some(long.clone()),
                },
            ),
            org_join("2026-11-14T10:05:00Z", &long, "acme"),
            edit(
                "atlas",
                "2026-11-14T10:10:00Z",
                ProjectEdit::OrgRename {
                    org: "acme".to_string(),
                    name: "Acme".to_string(),
                },
            ),
        ];
        let projects = Projects::resolve(&log, &[], &accounts);
        assert_eq!(projects.of(&key).org.as_deref(), Some("acme"));
        let org = projects
            .orgs
            .iter()
            .find(|org| org.id == "acme")
            .expect("the joined organization");
        assert_eq!(org.name, "Acme");
        assert_eq!(org.accounts, vec![acme_account()]);

        // Non-blank is still required, and a shown name is still 1..=40.
        assert!(ProjectEdit::Assign {
            key: folder("atlas", "atlas"),
            org: Some(" ".to_string()),
        }
        .validate()
        .is_err());
        assert!(ProjectEdit::AccountOrg {
            account: acme_account(),
            org: Some(" ".to_string()),
        }
        .validate()
        .is_err());
        assert!(ProjectEdit::OrgJoin {
            org: " ".to_string(),
            into: "acme".to_string(),
        }
        .validate()
        .is_err());
        assert!(ProjectEdit::OrgRename {
            org: long,
            name: "a".repeat(41),
        }
        .validate()
        .is_err());
    }

    #[test]
    fn proof_an_account_is_in_one_organization() {
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::AccountOrg {
                    account: acme_account(),
                    org: Some("acme".to_string()),
                },
            ),
            edit(
                "atlas",
                "2026-11-14T10:05:00Z",
                ProjectEdit::AccountOrg {
                    account: acme_account(),
                    org: Some("zeta".to_string()),
                },
            ),
        ];
        let projects = Projects::resolve(&log, &[], &[]);
        let holders = projects
            .orgs
            .iter()
            .filter(|org| org.accounts.contains(&acme_account()))
            .count();
        assert_eq!(holders, 1, "{:?}", projects.orgs);
    }

    #[test]
    fn proof_of_follows_a_join_to_a_project_without_sessions() {
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::Join {
                    key: folder("atlas", "atlas"),
                    into: folder("ledger", "atlas"),
                },
            ),
            edit(
                "atlas",
                "2026-11-14T10:05:00Z",
                ProjectEdit::Rename {
                    key: folder("ledger", "atlas"),
                    name: "Ledger".to_string(),
                },
            ),
        ];
        let projects = Projects::resolve(&log, &[], &[]);
        let found = projects.of(&folder("atlas", "atlas"));
        assert_eq!(found.key, folder("ledger", "atlas"));
        assert_eq!(found.name, "Ledger");
    }

    #[test]
    fn of_applies_an_assignment_and_its_org_root_without_sessions() {
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::Assign {
                    key: folder("atlas", "atlas"),
                    org: Some("Acme Inc.".to_string()),
                },
            ),
            org_join("2026-11-14T10:05:00Z", "Acme Inc.", "acme"),
        ];
        let projects = Projects::resolve(&log, &[], &[]);
        assert_eq!(
            projects.of(&folder("atlas", "atlas")).org.as_deref(),
            Some("acme")
        );
    }

    #[test]
    fn moving_one_account_keeps_an_account_only_another_host_stores() {
        // Vega stores PR with orgName "Acme Inc."; atlas does not store PR.
        // Atlas moves LB into "Acme Inc.". PR stays in it, because the one
        // account edit moves only LB.
        let pr = account_with(
            "pr-local",
            PR_ACC,
            PR_ORG,
            "Private",
            "PR",
            "sam.private@mail.example",
            Some("Acme Inc."),
        );
        let lb = account_with(
            "lb-local",
            LB_ACC,
            LB_ORG,
            "Lab",
            "LB",
            "sam@lab.example",
            None,
        );
        let accounts = vec![pr.clone(), lb.clone()];
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:00:00Z",
            ProjectEdit::AccountOrg {
                account: AccountRef::of(&lb),
                org: Some("Acme Inc.".to_string()),
            },
        )];
        let projects = Projects::resolve(&log, &[], &accounts);
        let acme = projects
            .orgs
            .iter()
            .find(|org| org.id == "Acme Inc.")
            .expect("the organization is there");
        assert!(acme.accounts.contains(&AccountRef::of(&pr)), "{acme:?}");
        assert!(acme.accounts.contains(&AccountRef::of(&lb)), "{acme:?}");
        assert_eq!(acme.accounts.len(), 2, "{acme:?}");
    }

    #[test]
    fn the_latest_account_org_record_wins() {
        // The records arrive out of order; the later one (acme) decides.
        let accounts = vec![account("acme-local", Some("Acme Inc."))];
        let log = vec![
            edit(
                "vega",
                "2026-11-14T10:05:00Z",
                ProjectEdit::AccountOrg {
                    account: acme_account(),
                    org: Some("acme".to_string()),
                },
            ),
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::AccountOrg {
                    account: acme_account(),
                    org: Some("Acme Inc.".to_string()),
                },
            ),
        ];
        let projects = Projects::resolve(&log, &[], &accounts);
        let holders: Vec<&str> = projects
            .orgs
            .iter()
            .filter(|org| org.accounts.contains(&acme_account()))
            .map(|org| org.id.as_str())
            .collect();
        assert_eq!(holders, vec!["acme"], "{:?}", projects.orgs);
    }

    #[test]
    fn folder_projects_are_per_host_until_joined() {
        let atlas = folder("notes", "atlas");
        let vega = folder("notes", "vega");
        let sessions = vec![
            session(
                "atlas",
                "2026-11-14T10:00:00Z",
                SESSION_A,
                "/home/sam/notes",
                atlas.clone(),
            ),
            session(
                "vega",
                "2026-11-14T11:00:00Z",
                SESSION_B,
                "/home/sam/notes",
                vega.clone(),
            ),
        ];
        let projects = Projects::resolve(&[], &sessions, &[]);
        assert_eq!(projects.projects.len(), 2, "{:?}", projects.projects);
        assert!(projects.projects.iter().any(|project| project.key == atlas));
        assert!(projects.projects.iter().any(|project| project.key == vega));
        assert_eq!(projects.of(&atlas).name, "notes");
        assert_eq!(projects.of(&vega).name, "notes");

        // Joining the folder on vega into the one on atlas makes one project.
        let log = vec![edit(
            "atlas",
            "2026-11-14T12:00:00Z",
            ProjectEdit::Join {
                key: vega.clone(),
                into: atlas.clone(),
            },
        )];
        let joined = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(joined.projects.len(), 1, "{:?}", joined.projects);
        let project = &joined.projects[0];
        assert_eq!(project.key, atlas);
        assert_eq!(project.sessions, 2);
        assert_eq!(project.folders.len(), 2);
        assert_eq!(project.joined.len(), 1);
        assert_eq!(project.joined[0].key, vega);
    }

    #[test]
    fn an_added_organization_is_shared_through_the_history() {
        // The line is in atlas's log; another host resolves it together with
        // its own session records and sees the organization too.
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:00:00Z",
            ProjectEdit::OrgAdd {
                name: "ledger".to_string(),
            },
        )];
        let sessions = vec![session(
            "vega",
            "2026-11-14T11:00:00Z",
            SESSION_A,
            "/home/sam/notes",
            folder("notes", "vega"),
        )];
        let projects = Projects::resolve(&log, &sessions, &[]);
        let line = serde_json::to_string(&log[0].record).expect("a log line");
        assert!(
            line.contains("\"edit\":\"orgAdd\",\"name\":\"ledger\""),
            "{line}"
        );
        let ledger = projects
            .orgs
            .iter()
            .find(|org| org.id == "ledger")
            .expect("the added organization is on every host");
        assert_eq!(ledger.name, "ledger");
        assert_eq!(ledger.projects, 0);
        assert!(ledger.accounts.is_empty());
        assert!(ledger.owner_repositories.is_empty());

        // A second add of the same name is refused and names it, whatever
        // its letters' case.
        let refused = projects
            .validate_edit(&ProjectEdit::OrgAdd {
                name: "LEDGER".to_string(),
            })
            .expect_err("an organization of that name exists");
        assert_eq!(
            refused.to_string(),
            "an organization named LEDGER already exists"
        );
    }

    #[test]
    fn removing_an_organization_returns_its_projects_and_accounts_to_none() {
        let key = repo("github.com/acme/atlas");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/atlas",
            key.clone(),
        )];
        let accounts = vec![account("acme-local", Some("Acme Inc."))];
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:00:00Z",
                ProjectEdit::Assign {
                    key: key.clone(),
                    org: Some("Acme Inc.".to_string()),
                },
            ),
            edit(
                "atlas",
                "2026-11-14T10:05:00Z",
                ProjectEdit::OrgRemove {
                    org: "Acme Inc.".to_string(),
                },
            ),
        ];
        let projects = Projects::resolve(&log, &sessions, &accounts);
        let line = serde_json::to_string(&log[1].record).expect("a log line");
        assert!(
            line.contains("\"edit\":\"orgRemove\",\"org\":\"Acme Inc.\""),
            "{line}"
        );
        assert_eq!(projects.projects[0].org, None, "{:?}", projects.projects);
        // The account's only organization was removed, so it has none. The
        // stored account itself is untouched.
        assert!(projects.orgs.is_empty(), "{:?}", projects.orgs);
        assert!(projects
            .account_refs
            .iter()
            .any(|entry| entry.account == acme_account()));
    }

    #[test]
    fn an_owner_organization_cannot_be_removed() {
        let key = repo("github.com/acme/acme-kb");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/acme-kb",
            key.clone(),
        )];
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:05:00Z",
            ProjectEdit::OrgRemove {
                org: "acme".to_string(),
            },
        )];
        let projects = Projects::resolve(&log, &sessions, &[]);
        let acme = projects
            .orgs
            .iter()
            .find(|org| org.id == "acme")
            .expect("the owner organization stays");
        assert_eq!(
            acme.owner_repositories,
            vec!["github.com/acme/acme-kb".to_string()]
        );
        assert_eq!(projects.projects[0].org.as_deref(), Some("acme"));

        let refused = projects
            .validate_edit(&ProjectEdit::OrgRemove {
                org: "acme".to_string(),
            })
            .expect_err("an owner organization is refused");
        assert_eq!(
            refused.to_string(),
            "the organization acme comes from the owner of github.com/acme/acme-kb; it cannot be removed"
        );
    }

    #[test]
    fn a_github_project_can_be_assigned_to_another_organization() {
        let key = repo("github.com/acme/atlas");
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/atlas",
            key.clone(),
        )];
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:05:00Z",
            ProjectEdit::Assign {
                key: key.clone(),
                org: Some("sam".to_string()),
            },
        )];
        let first = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(first.projects[0].org.as_deref(), Some("sam"));
        assert!(first.orgs.iter().any(|org| org.id == "sam"));

        // Resolving the same log again keeps the assignment.
        let again = Projects::resolve(&log, &sessions, &[]);
        assert_eq!(again.projects[0].org.as_deref(), Some("sam"));
        assert_eq!(again.projects, first.projects);
        assert_eq!(again.orgs, first.orgs);
    }

    fn add(host: &str, at: &str, name: &str) -> Hosted<ProjectsLogRecord> {
        edit(
            host,
            at,
            ProjectEdit::OrgAdd {
                name: name.to_string(),
            },
        )
    }

    fn remove(host: &str, at: &str, org: &str) -> Hosted<ProjectsLogRecord> {
        edit(
            host,
            at,
            ProjectEdit::OrgRemove {
                org: org.to_string(),
            },
        )
    }

    #[test]
    fn proof_an_organization_added_again_after_its_removal_is_back() {
        let log = vec![
            add("atlas", "2026-11-14T10:00:00Z", "ledger"),
            remove("atlas", "2026-11-14T10:05:00Z", "ledger"),
            add("atlas", "2026-11-14T10:10:00Z", "ledger"),
        ];
        let projects = Projects::resolve(&log, &[], &[]);
        assert!(
            projects.orgs.iter().any(|org| org.id == "ledger"),
            "add, remove, add: the organization must exist, got {:?}",
            projects.orgs
        );
    }

    #[test]
    fn proof_an_empty_organization_is_not_an_owner_organization_after_a_join() {
        // acme/atlas is joined into bob/ledger: the project belongs to bob.
        // The added organization acme holds no project, so removing it is fine.
        let atlas = repo("github.com/acme/atlas");
        let ledger = repo("github.com/bob/ledger");
        let sessions = vec![
            session(
                "atlas",
                "2026-11-14T10:00:00Z",
                SESSION_A,
                "/home/sam/atlas",
                atlas.clone(),
            ),
            session(
                "atlas",
                "2026-11-14T10:00:00Z",
                SESSION_B,
                "/home/sam/ledger",
                ledger.clone(),
            ),
        ];
        let log = vec![
            edit(
                "atlas",
                "2026-11-14T10:01:00Z",
                ProjectEdit::Join {
                    key: atlas.clone(),
                    into: ledger.clone(),
                },
            ),
            edit(
                "atlas",
                "2026-11-14T10:02:00Z",
                ProjectEdit::Assign {
                    key: ledger.clone(),
                    org: Some("sam".to_string()),
                },
            ),
            add("atlas", "2026-11-14T10:03:00Z", "acme"),
        ];
        let projects = Projects::resolve(&log, &sessions, &[]);
        let refused = projects.validate_edit(&ProjectEdit::OrgRemove {
            org: "acme".to_string(),
        });
        assert!(refused.is_ok(), "acme holds no project: {refused:?}");
    }

    #[test]
    fn proof_organization_names_differing_in_unicode_case_are_one_name() {
        let log = vec![
            add("atlas", "2026-11-14T10:00:00Z", "Ärzte"),
            add("vega", "2026-11-14T10:01:00Z", "ärzte"),
        ];
        let projects = Projects::resolve(&log, &[], &[]);
        assert_eq!(projects.orgs.len(), 1, "{:?}", projects.orgs);
        let one = projects.orgs[0].id.clone();
        let refused = projects.validate_edit(&ProjectEdit::OrgAdd {
            name: one.to_uppercase(),
        });
        assert!(refused.is_err());
    }

    #[test]
    fn proof_the_first_added_spelling_wins_not_the_alphabetical_one() {
        let log = vec![
            add("atlas", "2026-11-14T10:00:00Z", "ledger"),
            add("vega", "2026-11-14T10:01:00Z", "Ledger"),
        ];
        let projects = Projects::resolve(&log, &[], &[]);
        assert_eq!(projects.orgs.len(), 1, "{:?}", projects.orgs);
        assert_eq!(projects.orgs[0].id, "ledger");
    }

    #[test]
    fn proof_an_added_name_with_outer_spaces_is_refused() {
        // No silent trimming: the name as typed is checked, and a name with
        // outer white space is refused rather than stored as something else.
        let edit = ProjectEdit::OrgAdd {
            name: " ledger ".to_string(),
        };
        let refused = edit.validate().expect_err("outer spaces are refused");
        assert_eq!(
            refused.to_string(),
            "an organization name must not start or end with a space"
        );
        let projects = Projects::resolve(&[], &[], &[]);
        let refused = projects
            .validate_edit(&edit)
            .expect_err("validate_edit refuses it too");
        assert_eq!(
            refused.to_string(),
            "an organization name must not start or end with a space"
        );
    }

    fn acme_repo_session(owner: &str) -> (ProjectId, Vec<Hosted<SessionRecord>>) {
        let key = repo(&format!("github.com/{owner}/acme-kb"));
        let sessions = vec![session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/projects/acme-kb",
            key.clone(),
        )];
        (key, sessions)
    }

    #[test]
    fn an_owner_and_a_login_organization_in_another_case_are_one_organization() {
        // GitHub owners are case-insensitive: the repository owner `Acme` and
        // the login organization `acme` are one organization, also in a 1.1.0
        // history.
        let (_, sessions) = acme_repo_session("Acme");
        let accounts = vec![account("acme-local", Some("acme"))];
        let projects = Projects::resolve(&[], &sessions, &accounts);
        assert_eq!(projects.orgs.len(), 1, "{:?}", projects.orgs);
        let org = &projects.orgs[0];
        assert_eq!(org.id, "Acme");
        assert_eq!(org.name, "Acme");
        assert_eq!(org.projects, 1);
        assert_eq!(org.accounts, vec![acme_account()]);
        assert_eq!(
            org.owner_repositories,
            vec!["github.com/Acme/acme-kb".to_string()]
        );
        assert_eq!(projects.projects[0].org.as_deref(), Some("Acme"));
        let refused = projects
            .validate_edit(&ProjectEdit::OrgAdd {
                name: "aCmE".to_string(),
            })
            .expect_err("the one organization already exists");
        assert_eq!(
            refused.to_string(),
            "an organization named aCmE already exists"
        );
    }

    #[test]
    fn an_org_join_of_one_spelling_into_another_is_a_self_join() {
        // `OrgJoin Acme into acme` names one organization, so it is refused
        // as a self-join, exactly like a join of one spelling into itself.
        let (_, sessions) = acme_repo_session("Acme");
        let accounts = vec![account("acme-local", Some("acme"))];
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:05:00Z",
            ProjectEdit::OrgJoin {
                org: "Acme".to_string(),
                into: "acme".to_string(),
            },
        )];
        let before = Projects::resolve(&[], &sessions, &accounts);
        let refused = before
            .validate_edit(&log[0].record.edit)
            .expect_err("the two spellings are one organization");
        let itself = ProjectEdit::OrgJoin {
            org: "acme".to_string(),
            into: "acme".to_string(),
        };
        let same = before
            .validate_edit(&itself)
            .expect_err("a join into itself is refused");
        assert_eq!(refused.to_string(), same.to_string());
        // A hand-written line changes nothing: the one organization keeps its
        // name, its project and its account.
        let projects = Projects::resolve(&log, &sessions, &accounts);
        assert_eq!(projects.orgs, before.orgs);
        assert_eq!(projects.projects, before.projects);
    }

    #[test]
    fn an_org_rename_of_one_spelling_renames_the_one_organization() {
        // Renaming the login organization `acme` renames the one organization
        // that the owner `Acme` also names.
        let (_, sessions) = acme_repo_session("Acme");
        let accounts = vec![account("acme-local", Some("acme"))];
        let log = vec![edit(
            "atlas",
            "2026-11-14T10:05:00Z",
            ProjectEdit::OrgRename {
                org: "acme".to_string(),
                name: "Acme Corp".to_string(),
            },
        )];
        let projects = Projects::resolve(&log, &sessions, &accounts);
        assert_eq!(projects.orgs.len(), 1, "{:?}", projects.orgs);
        assert_eq!(projects.orgs[0].id, "Acme");
        assert_eq!(projects.orgs[0].name, "Acme Corp");
        assert_eq!(projects.projects[0].org.as_deref(), Some("Acme"));
    }

    #[test]
    fn proof_two_hosts_with_different_accounts_show_one_name() {
        // The log adds `Ledger`; one host also stores a login whose
        // organization is spelled `ledger`. The name is part of "the same
        // names on every host once the log has arrived".
        let log = vec![add("atlas", "2026-11-14T10:00:00Z", "Ledger")];
        let with_account = Projects::resolve(&log, &[], &[account("lb", Some("ledger"))]);
        let without = Projects::resolve(&log, &[], &[]);
        let names = |projects: &Projects| -> Vec<(String, String)> {
            projects
                .orgs
                .iter()
                .map(|org| (org.id.clone(), org.name.clone()))
                .collect()
        };
        assert_eq!(names(&with_account), names(&without));
    }

    #[test]
    fn the_first_shared_spelling_wins_over_a_later_session() {
        // The log names `Ledger` before the session names `ledger`: the shared
        // data gives the spelling in the history's order, not sessions first.
        let log = vec![add("atlas", "2026-11-14T10:00:00Z", "Ledger")];
        let sessions = vec![session(
            "vega",
            "2026-11-14T11:00:00Z",
            SESSION_A,
            "/home/sam/ledger",
            repo("github.com/ledger/ledger"),
        )];
        let projects = Projects::resolve(&log, &sessions, &[]);
        let org = projects
            .orgs
            .iter()
            .find(|org| organization_key(&org.id) == "ledger")
            .expect("one ledger");
        assert_eq!(org.id, "Ledger");
        assert_eq!(org.name, "Ledger");
    }

    #[test]
    fn the_earlier_session_spells_the_organization() {
        // The file lists the later session first; the history's order decides.
        let sessions = vec![
            session(
                "atlas",
                "2026-11-14T12:00:00Z",
                SESSION_A,
                "/home/sam/acme-kb",
                repo("github.com/Acme/acme-kb"),
            ),
            session(
                "vega",
                "2026-11-14T11:00:00Z",
                SESSION_B,
                "/home/sam/acme-kb",
                repo("github.com/acme/acme-kb"),
            ),
        ];
        let projects = Projects::resolve(&[], &sessions, &[]);
        assert_eq!(projects.orgs.len(), 1, "{:?}", projects.orgs);
        assert_eq!(projects.orgs[0].id, "acme");
        assert_eq!(projects.orgs[0].name, "acme");
    }

    #[test]
    fn equal_time_sessions_on_two_hosts_spell_the_same_way_in_any_read_order() {
        let atlas = session(
            "atlas",
            "2026-11-14T10:00:00Z",
            SESSION_A,
            "/home/sam/acme-kb",
            repo("github.com/acme/acme-kb"),
        );
        let vega = session(
            "vega",
            "2026-11-14T10:00:00Z",
            SESSION_B,
            "/home/sam/acme-kb",
            repo("github.com/Acme/acme-kb"),
        );
        let one = Projects::resolve(&[], &[atlas.clone(), vega.clone()], &[]);
        let two = Projects::resolve(&[], &[vega, atlas], &[]);
        assert_eq!(one.orgs, two.orgs);
        assert_eq!(one.orgs[0].id, "acme");
    }

    #[test]
    fn a_stored_account_spells_an_organization_no_shared_record_names() {
        let accounts = vec![account("lb", Some("Ledger Team"))];
        let projects = Projects::resolve(&[], &[], &accounts);
        let org = projects
            .orgs
            .iter()
            .find(|org| organization_key(&org.id) == "ledger team")
            .expect("the account's organization");
        assert_eq!(org.id, "Ledger Team");
        assert_eq!(org.name, "Ledger Team");
    }

    #[test]
    fn proof_equal_times_on_two_hosts_decide_the_same_way_in_any_read_order() {
        let a = add("vega", "2026-11-14T10:00:00Z", "ledger");
        let r = remove("atlas", "2026-11-14T10:00:00Z", "ledger");
        let one = Projects::resolve(&[a.clone(), r.clone()], &[], &[]);
        let two = Projects::resolve(&[r, a], &[], &[]);
        assert_eq!(one.orgs, two.orgs);
    }

    #[test]
    fn proof_records_naming_an_organization_in_another_case_reach_it() {
        let (key, sessions) = acme_repo_session("acme");
        let log = vec![
            add("atlas", "2026-11-14T10:00:00Z", "Ledger"),
            edit(
                "atlas",
                "2026-11-14T10:01:00Z",
                ProjectEdit::Assign {
                    key: key.clone(),
                    org: Some("LEDGER".to_string()),
                },
            ),
            edit(
                "atlas",
                "2026-11-14T10:02:00Z",
                ProjectEdit::AccountOrg {
                    account: acme_account(),
                    org: Some("ledger".to_string()),
                },
            ),
        ];
        let accounts = vec![account("acme-local", Some("Acme Inc."))];
        let projects = Projects::resolve(&log, &sessions, &accounts);
        let ledger: Vec<&Org> = projects
            .orgs
            .iter()
            .filter(|org| org.id.to_lowercase() == "ledger")
            .collect();
        assert_eq!(ledger.len(), 1, "{:?}", projects.orgs);
        assert_eq!(ledger[0].projects, 1);
        assert_eq!(ledger[0].accounts, vec![acme_account()]);
        assert_eq!(
            projects.projects[0].org.as_deref(),
            Some(ledger[0].id.as_str())
        );
        let removal = projects.validate_edit(&ProjectEdit::OrgRemove {
            org: "LEDGER".to_string(),
        });
        assert!(removal.is_ok(), "{removal:?}");
    }
}
