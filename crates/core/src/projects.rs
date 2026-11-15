//! Projects and organizations (spec §3.6): names, joins, organizations and the
//! shared project log. Everything is computed from the history's session
//! records of every host plus the append-only project log; nothing is stored
//! in `accounts.json`. One writer per host, one line per edit, and the same
//! names on every host once the log has arrived.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::history::{AccountRef, Hosted, Kind, Record, SessionRecord};
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
                if org == into {
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
    #[serde(skip)]
    assigned: BTreeMap<ProjectId, Option<String>>,
    #[serde(skip)]
    joins: BTreeMap<ProjectId, (ProjectId, usize)>,
    #[serde(skip)]
    org_join: BTreeMap<String, (String, usize)>,
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
        let mut assigned: BTreeMap<ProjectId, Option<String>> = BTreeMap::new();
        let mut joins: BTreeMap<ProjectId, (ProjectId, usize)> = BTreeMap::new();
        let mut org_join: BTreeMap<String, (String, usize)> = BTreeMap::new();
        let mut org_names: BTreeMap<String, String> = BTreeMap::new();
        // The per-account organization edits in the order they apply, after
        // every `OrgJoin` is resolved: the latest record for an account wins.
        let mut account_org_edits: Vec<(AccountRef, Option<String>)> = Vec::new();
        for (order, hosted) in ordered.into_iter().enumerate() {
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
                    assigned.insert(key.clone(), org.clone());
                }
                ProjectEdit::OrgRename { org, name } => {
                    org_names.insert(org.clone(), name.clone());
                }
                ProjectEdit::OrgJoin { org, into } => {
                    if org == into {
                        // Refused by `validate`; a hand-written line moves
                        // nothing rather than joining an organization into
                        // itself.
                        continue;
                    }
                    break_join_cycle(&mut org_join, org, into);
                    org_join.insert(org.clone(), (into.clone(), order));
                }
                ProjectEdit::AccountOrg { account, org } => {
                    account_org_edits.push((account.clone(), org.clone()));
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
            let org = match assigned.get(&root) {
                Some(org) => org.clone(),
                None => default_org(&root.key),
            };
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

        // An organization is its name; a join or a rename does not change the
        // identity a later edit can name. Every `OrgJoin` is resolved first,
        // so each identity has its root.
        let mut identities: BTreeSet<String> = BTreeSet::new();
        for project in &projects {
            if let Some(org) = &project.org {
                identities.insert(org.clone());
            }
        }
        for account in accounts {
            if let Some(org) = &account.org_name {
                if !org.trim().is_empty() {
                    identities.insert(org.clone());
                }
            }
        }
        identities.extend(org_names.keys().cloned());
        identities.extend(org_join.keys().cloned());
        identities.extend(org_join.values().map(|(into, _)| into.clone()));
        identities.extend(account_org_edits.iter().filter_map(|(_, org)| org.clone()));

        // One rule decides an account's organization. It starts in the root of
        // its `org_name` (none when it has no `org_name`). Then each
        // `AccountOrg` record, in log order, puts that one account in the
        // record's root, or in none. Nothing else moves it, so an account only
        // another host stores keeps its own organization.
        let mut account_org: BTreeMap<AccountRef, String> = BTreeMap::new();
        for account in accounts {
            if let Some(org) = &account.org_name {
                if !org.trim().is_empty() {
                    account_org.insert(AccountRef::of(account), join_root(&org_join, org));
                }
            }
        }
        for (account, org) in &account_org_edits {
            match org {
                Some(org) => {
                    account_org.insert(account.clone(), join_root(&org_join, org));
                }
                None => {
                    account_org.remove(account);
                }
            }
        }

        let mut by_root: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for identity in identities {
            by_root
                .entry(join_root(&org_join, &identity))
                .or_default()
                .push(identity);
        }

        // A project belongs to the organization its owner (or its assignment)
        // resolves to, so the counts below are per shown organization.
        for project in &mut projects {
            if let Some(org) = &project.org {
                project.org = Some(join_root(&org_join, org));
            }
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
                .unwrap_or_else(|| root.clone());
            let project_count = projects
                .iter()
                .filter(|project| project.org.as_deref() == Some(root.as_str()))
                .count();
            orgs.push(Org {
                name,
                accounts: accounts_in
                    .remove(&root)
                    .map(|own| own.into_iter().collect())
                    .unwrap_or_default(),
                id: root,
                projects: project_count as u32,
            });
        }
        orgs.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

        // Assigned organizations first, then by name, as the mock-up lists them.
        let org_label = |org: &Option<String>| match org {
            Some(identity) => org_names
                .get(identity)
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
        let org = match self.assigned.get(&root) {
            Some(org) => org.clone(),
            None => default_org(&root.key),
        };
        project.org = org.map(|org| join_root(&self.org_join, &org));
        project
    }
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
fn default_name(key: &ProjectKey) -> String {
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
}
