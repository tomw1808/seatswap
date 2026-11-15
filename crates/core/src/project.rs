//! From a folder to the project's identity (spec §3.6): the address of the
//! repository's remote "origin", or the folder's name. Only `.git` and the
//! git `config` are read; nothing is executed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum ProjectKey {
    /// "github.com/acme/acme-kb": host/path of the remote "origin", lower-cased
    /// host, no protocol, no user, no ".git"
    Repo(String),
    /// no remote: the repository's folder name; outside a repository: the first
    /// folder under home, or "~"
    Folder(String),
}
