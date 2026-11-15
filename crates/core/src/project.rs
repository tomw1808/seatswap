//! From a folder to the project's identity (spec §3.6): the address of the
//! repository's remote "origin", or the folder's name. Only `.git` and the
//! git `config` are read; nothing is executed.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

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

/// Walk up from `cwd` to the first folder that has `.git` (a folder, or a file "gitdir: <path>" as in worktrees
/// and submodules: follow it to the config). Read `[remote "origin"] url` from its `config`. Nothing is executed.
///
/// A `cwd` inside `home` is looked at only below `home`: `home` itself and the folders above it are never a
/// repository root, so a home folder that is itself a git repository (dotfiles kept in git) does not name every
/// folder under it. A `cwd` outside `home` is looked at through all its ancestors.
///
/// Only the repository's own `config` is read: `[include]`d files (`includeIf` among them) are not followed, so a
/// repository whose origin lives only in an included file is named by its folder, like one without a remote.
///
/// A `cwd` that is not absolute is never looked up on disk: Claude Code writes absolute folders, and the result
/// must not depend on the folder seatswap runs in. It gets the folder rule of `top_folder` on its text; an empty
/// one is `Folder("~")`.
pub fn identify(cwd: &Path, home: &Path) -> ProjectKey {
    if cwd.is_absolute() {
        if let Some(root) = repository_root(cwd, home) {
            if let Some(url) = origin_url(&root) {
                if let Some(remote) = normalize_remote(&url) {
                    return ProjectKey::Repo(remote);
                }
            }
            return ProjectKey::Folder(folder_name(&root));
        }
    }
    ProjectKey::Folder(top_folder(cwd, home))
}

/// `<host>/<path>` of a git remote: host lower-cased, no protocol, no user or
/// token, no port, no ".git" at the end, no trailing "/". `None` for a local
/// remote (a `file://` URL or a plain path) and for anything that is not one of
/// the known remote forms.
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    // A query or a fragment never belongs to a git address; cut it before
    // parsing, so a token in it cannot reach the result.
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let url = &url[..end];
    if url.is_empty() || is_plain_path(url) {
        return None;
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(
            scheme.to_ascii_lowercase().as_str(),
            "https" | "http" | "ssh" | "git"
        ) {
            return None;
        }
        let (authority, path) = split_path(rest);
        let host = host_without_port(strip_user(authority))?;
        host_and_path(host, path)
    } else {
        // scp-like: [user@]host:path
        let (authority, path) = url.split_once(':')?;
        let host = strip_user(authority);
        if host.is_empty() {
            return None;
        }
        host_and_path(host, path)
    }
}

/// A Windows path that starts with a lower-case drive letter gets it in upper case (`c:\x` → `C:\x`); every other
/// text comes back unchanged. On Windows Claude Code writes the same folder in both spellings, even inside one
/// transcript (checked 15 November 2026). `sessions` and `transcripts` pass every `cwd` they read through this.
pub fn canonical_cwd(cwd: &str) -> String {
    let bytes = cwd.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_lowercase()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        let mut out = String::with_capacity(cwd.len());
        out.push(bytes[0].to_ascii_uppercase() as char);
        out.push_str(&cwd[1..]);
        out
    } else {
        cwd.to_string()
    }
}

/// The folder of the repository `cwd` belongs to: the first ancestor that has a
/// `.git`. The ancestors are looked at even when `cwd` no longer exists (a
/// deleted worktree folder). When `cwd` is inside `home`, the walk stops before
/// `home`: only the folders strictly below `home` can be a repository root.
fn repository_root(cwd: &Path, home: &Path) -> Option<PathBuf> {
    let cwd_parts: Vec<Component> = cwd.components().collect();
    let home_parts: Vec<Component> = home.components().collect();
    let inside_home =
        cwd_parts.len() >= home_parts.len() && cwd_parts[..home_parts.len()] == home_parts[..];
    let mut depth = cwd_parts.len();
    for ancestor in cwd.ancestors() {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        if inside_home && depth <= home_parts.len() {
            break;
        }
        if fs::metadata(ancestor.join(".git")).is_ok() {
            return Some(ancestor.to_path_buf());
        }
        depth -= 1;
    }
    None
}

/// The `url` of the remote "origin" of the repository at `root`, as written in
/// its config.
fn origin_url(root: &Path) -> Option<String> {
    let gitdir = git_dir(&root.join(".git"))?;
    let config = config_file(&gitdir)?;
    first_origin_url(&read_text(&config)?)
}

/// The folder holding the repository's data: `.git` itself, or what a `.git`
/// file's "gitdir: <path>" names. A relative path is relative to the folder
/// holding the `.git` file.
fn git_dir(dot_git: &Path) -> Option<PathBuf> {
    let metadata = fs::metadata(dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    if !metadata.is_file() {
        return None;
    }
    let text = read_text(dot_git)?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    if target.is_absolute() {
        Some(target.to_path_buf())
    } else {
        Some(dot_git.parent()?.join(target))
    }
}

/// The config file of `gitdir`. A worktree's git dir has none of its own: its
/// `commondir` file names the folder that holds it (`../..` for the first
/// worktree), and without one the parent of the `worktrees` folder does.
fn config_file(gitdir: &Path) -> Option<PathBuf> {
    let own = gitdir.join("config");
    if own.is_file() {
        return Some(own);
    }
    if let Some(common) = read_text(&gitdir.join("commondir")) {
        let common = common.trim();
        if !common.is_empty() {
            let config = resolve(gitdir, common).join("config");
            if config.is_file() {
                return Some(config);
            }
        }
    }
    let worktrees = gitdir.parent()?;
    if worktrees.file_name() == Some(OsStr::new("worktrees")) {
        let config = worktrees.parent()?.join("config");
        if config.is_file() {
            return Some(config);
        }
    }
    None
}

fn resolve(base: &Path, target: &str) -> PathBuf {
    let target = Path::new(target);
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    }
}

fn read_text(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The first `url` line of the `[remote "origin"]` section of a git config.
/// Lines starting with `#` or `;` are comments; the key is case-insensitive,
/// the subsection name `origin` is not.
fn first_origin_url(text: &str) -> Option<String> {
    let mut in_origin = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            in_origin = is_origin_header(line);
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("url") {
            continue;
        }
        return Some(unquote(value.trim()).to_string());
    }
    None
}

/// `[remote "origin"]`, with the whitespace git writes. The section name is
/// matched like git matches it (not case-sensitively); the remote name is.
fn is_origin_header(line: &str) -> bool {
    let Some((inner, _)) = line.strip_prefix('[').and_then(|rest| rest.split_once(']')) else {
        return false;
    };
    let mut words = inner.split_whitespace();
    let Some(section) = words.next() else {
        return false;
    };
    if !section.eq_ignore_ascii_case("remote") {
        return false;
    }
    let Some(name) = words.next() else {
        return false;
    };
    unquote(name) == "origin"
}

fn unquote(text: &str) -> &str {
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

/// A local remote, not an address: a `file://` URL (never here; the scheme
/// check drops it) or a plain path.
fn is_plain_path(text: &str) -> bool {
    if text.starts_with('/') || text.starts_with('.') || text.starts_with('~') {
        return true;
    }
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn split_path(rest: &str) -> (&str, &str) {
    rest.split_once('/').unwrap_or((rest, ""))
}

fn strip_user(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

fn host_without_port(authority: &str) -> Option<&str> {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']').map(|(host, _)| host)?
    } else {
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
    };
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn host_and_path(host: &str, path: &str) -> Option<String> {
    let path = path.trim_start_matches('/');
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}

fn folder_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Outside a repository: the first folder below `home` (or "~" for `home`
/// itself); outside `home`: the last folder of `cwd`; an empty `cwd`: "~".
/// Compared component-wise.
fn top_folder(cwd: &Path, home: &Path) -> String {
    if cwd.as_os_str().is_empty() {
        return "~".to_string();
    }
    let cwd_parts: Vec<Component> = cwd.components().collect();
    let home_parts: Vec<Component> = home.components().collect();
    if cwd_parts == home_parts {
        return "~".to_string();
    }
    if cwd_parts.len() > home_parts.len() && cwd_parts[..home_parts.len()] == home_parts[..] {
        if let Some(Component::Normal(name)) = cwd_parts.get(home_parts.len()) {
            return name.to_string_lossy().into_owned();
        }
    }
    if let Some(Component::Normal(name)) = cwd_parts.last() {
        return name.to_string_lossy().into_owned();
    }
    cwd.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write_config(git: &Path, config: &str) {
        fs::create_dir_all(git).unwrap();
        fs::write(git.join("config"), config).unwrap();
    }

    fn repo_config(url: &str) -> String {
        format!(
            "[core]\n\trepositoryformatversion = 0\n\
             [remote \"origin\"]\n\turl = {url}\n\
             \tfetch = +refs/heads/*:refs/remotes/origin/*\n"
        )
    }

    #[test]
    fn https_and_ssh_remotes_are_the_same_project() {
        for url in [
            "https://github.com/acme/acme-kb.git",
            "git@github.com:acme/acme-kb.git",
            "ssh://git@github.com/acme/acme-kb",
            "https://github.com/acme/acme-kb/",
            "ssh://git@github.com:22/acme/acme-kb",
        ] {
            assert_eq!(
                normalize_remote(url),
                Some("github.com/acme/acme-kb".to_string()),
                "{url}"
            );
        }
        assert_eq!(normalize_remote("file:///home/sam/atlas"), None);
        assert_eq!(normalize_remote("/home/sam/atlas"), None);
    }

    #[test]
    fn credentials_in_the_url_are_dropped() {
        assert_eq!(
            normalize_remote("https://sam:ghp_x@github.com/sam/atlas.git"),
            Some("github.com/sam/atlas".to_string())
        );
    }

    #[test]
    fn a_subfolder_belongs_to_its_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let repo = home.join("projects/atlas");
        write_config(
            &repo.join(".git"),
            &repo_config("https://github.com/sam/atlas.git"),
        );
        let cwd = repo.join("engine/tests");
        fs::create_dir_all(&cwd).unwrap();
        assert_eq!(
            identify(&cwd, home),
            ProjectKey::Repo("github.com/sam/atlas".to_string())
        );
    }

    #[test]
    fn a_repository_without_a_remote_is_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let repo = home.join("projects/scratch");
        write_config(
            &repo.join(".git"),
            "[core]\n\trepositoryformatversion = 0\n\
             [remote \"upstream\"]\n\turl = https://github.com/sam/scratch.git\n",
        );
        assert_eq!(
            identify(&repo, home),
            ProjectKey::Folder("scratch".to_string())
        );
    }

    #[test]
    fn outside_a_repository_it_is_the_top_folder_under_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cwd = home.join("notes/2026/x");
        fs::create_dir_all(&cwd).unwrap();
        assert_eq!(
            identify(&cwd, home),
            ProjectKey::Folder("notes".to_string())
        );
        assert_eq!(identify(home, home), ProjectKey::Folder("~".to_string()));
    }

    #[test]
    fn a_worktree_follows_its_gitdir_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let main_repo = home.join("projects/atlas");
        write_config(
            &main_repo.join(".git"),
            &repo_config("https://github.com/sam/atlas.git"),
        );
        let worktree = home.join("projects/atlas-w1");
        fs::create_dir_all(&worktree).unwrap();
        let gitdir = main_repo.join(".git/worktrees/w1");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        assert_eq!(
            identify(&worktree, home),
            ProjectKey::Repo("github.com/sam/atlas".to_string())
        );

        // a commondir file names the folder that holds the config
        let other = home.join("projects/other");
        write_config(
            &other.join(".git"),
            &repo_config("https://github.com/sam/other.git"),
        );
        fs::write(gitdir.join("commondir"), "../../../../other/.git\n").unwrap();
        assert_eq!(
            identify(&worktree, home),
            ProjectKey::Repo("github.com/sam/other".to_string())
        );
    }

    #[test]
    fn a_missing_folder_does_not_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert_eq!(
            identify(&home.join("gone/deeper"), home),
            ProjectKey::Folder("gone".to_string())
        );
    }

    #[test]
    fn a_deleted_worktree_folder_belongs_to_its_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let repo = home.join("projects/atlas");
        write_config(
            &repo.join(".git"),
            &repo_config("https://github.com/sam/atlas.git"),
        );
        assert_eq!(
            identify(&repo.join(".claude/worktrees/w1"), home),
            ProjectKey::Repo("github.com/sam/atlas".to_string())
        );
    }

    #[test]
    fn a_lower_case_drive_letter_is_made_upper_case() {
        assert_eq!(canonical_cwd(r"c:\Users\sam\atlas"), r"C:\Users\sam\atlas");
        assert_eq!(canonical_cwd("c:/Users/sam/atlas"), "C:/Users/sam/atlas");
        assert_eq!(canonical_cwd(r"C:\x"), r"C:\x");
        assert_eq!(canonical_cwd("/home/sam/c:"), "/home/sam/c:");
    }

    // ---- proof tests from the review of Task 5 ----

    /// The walk up from `cwd` does not stop at the home folder. A home that is
    /// itself a git repository (dotfiles kept in git) therefore swallows every
    /// folder under it: `~/notes/2026/x` is reported as the home repository
    /// (its remote, or a folder named after the home folder) instead of the
    /// first folder under home.
    #[test]
    fn a_home_folder_that_is_a_git_repository_does_not_swallow_every_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        write_config(
            &home.join(".git"),
            "[core]\n\trepositoryformatversion = 0\n",
        );
        let cwd = home.join("notes/2026/x");
        fs::create_dir_all(&cwd).unwrap();
        assert_eq!(
            identify(&cwd, &home),
            ProjectKey::Folder("notes".to_string()),
            "a folder under home is the first folder under home, not the home repository"
        );
    }

    /// The same, with a home repository that has a remote: the folder under it
    /// becomes that remote.
    #[test]
    fn a_home_folder_with_a_remote_does_not_name_every_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        write_config(
            &home.join(".git"),
            &repo_config("https://github.com/sam/dotfiles.git"),
        );
        let cwd = home.join("notes/2026/x");
        fs::create_dir_all(&cwd).unwrap();
        assert_eq!(
            identify(&cwd, &home),
            ProjectKey::Folder("notes".to_string()),
            "a folder under home is the first folder under home, not the home repository"
        );
    }

    /// The remote's address is the *first* url of the section: that is what
    /// `git remote get-url origin` prints and what `git clone` and `git fetch`
    /// use. A second url line (from `git remote set-url --add`) is a further
    /// address, not the remote's one.
    #[test]
    fn the_first_url_of_the_origin_is_the_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let repo = home.join("projects/atlas");
        write_config(
            &repo.join(".git"),
            "[remote \"origin\"]\n\turl = https://github.com/sam/old.git\n\
             \turl = https://github.com/sam/atlas.git\n",
        );
        assert_eq!(
            identify(&repo, home),
            ProjectKey::Repo("github.com/sam/old".to_string()),
            "git remote get-url origin prints the first url"
        );
    }

    /// `[include]` is not followed: its rules (`includeIf` among them) are a
    /// second config reader, and the spec names the folder for "no remote".
    #[test]
    fn an_included_config_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let repo = home.join("projects/atlas");
        let git = repo.join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(
            git.join("remote.config"),
            "[remote \"origin\"]\n\turl = https://github.com/sam/atlas.git\n",
        )
        .unwrap();
        fs::write(git.join("config"), "[include]\n\tpath = remote.config\n").unwrap();
        assert_eq!(
            identify(&repo, home),
            ProjectKey::Folder("atlas".to_string()),
            "an included config is not read: no remote in the repository's own config"
        );
    }

    /// A `cwd` that is not absolute is never looked up on disk: the folder
    /// rule works on its text alone. The current folder is deliberately not
    /// changed; the tempdir's `.git` is a repository the relative path must
    /// not reach.
    #[test]
    fn a_relative_cwd_is_never_looked_up() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_config(
            &home.join(".git"),
            &repo_config("https://github.com/sam/dotfiles.git"),
        );
        fs::create_dir_all(home.join("engine")).unwrap();
        for cwd in [Path::new("."), Path::new("engine")] {
            assert!(
                matches!(identify(cwd, home), ProjectKey::Folder(_)),
                "{cwd:?} must not be looked up on disk"
            );
        }
        assert_eq!(
            identify(Path::new(""), home),
            ProjectKey::Folder("~".to_string()),
            "an empty cwd is the home folder"
        );
    }

    /// A token in the URL never reaches the result.
    #[test]
    fn a_token_in_the_query_never_reaches_the_result() {
        assert_eq!(
            normalize_remote("https://github.com/acme/acme-kb.git?access_token=ghp_secret"),
            Some("github.com/acme/acme-kb".to_string()),
            "the query with the token must not be part of the project's address"
        );
    }

    /// A submodule: its `.git` is a file that names the folder under
    /// `.git/modules`, and that folder holds the config with the remote.
    #[test]
    fn a_submodule_follows_its_gitdir_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let repo = home.join("projects/atlas");
        write_config(
            &repo.join(".git"),
            &repo_config("https://github.com/sam/atlas.git"),
        );
        let submodule = repo.join("vendor/lib");
        fs::create_dir_all(&submodule).unwrap();
        write_config(
            &repo.join(".git/modules/vendor/lib"),
            &repo_config("https://github.com/sam/lib.git"),
        );
        fs::write(
            submodule.join(".git"),
            "gitdir: ../../.git/modules/vendor/lib\n",
        )
        .unwrap();
        assert_eq!(
            identify(&submodule, home),
            ProjectKey::Repo("github.com/sam/lib".to_string())
        );
    }

    /// GitLab subgroups, Azure DevOps and Bitbucket with a port: the host is
    /// lower-cased, the port is dropped, the path is kept whole.
    #[test]
    fn the_other_hosts_keep_their_path() {
        assert_eq!(
            normalize_remote("https://gitlab.com/group/sub/project.git"),
            Some("gitlab.com/group/sub/project".to_string())
        );
        assert_eq!(
            normalize_remote("https://dev.azure.com/org/project/_git/repo"),
            Some("dev.azure.com/org/project/_git/repo".to_string())
        );
        assert_eq!(
            normalize_remote("ssh://git@ssh.dev.azure.com/v3/org/project/repo"),
            Some("ssh.dev.azure.com/v3/org/project/repo".to_string())
        );
        assert_eq!(
            normalize_remote("git@ssh.dev.azure.com:v3/org/project/repo"),
            Some("ssh.dev.azure.com/v3/org/project/repo".to_string())
        );
        assert_eq!(
            normalize_remote("https://org@dev.azure.com:443/org/project/_git/repo"),
            Some("dev.azure.com/org/project/_git/repo".to_string())
        );
        assert_eq!(
            normalize_remote("ssh://git@bitbucket.example.com:7999/proj/repo.git"),
            Some("bitbucket.example.com/proj/repo".to_string())
        );
    }

    /// No form of a token in the URL reaches the result.
    #[test]
    fn no_form_of_a_token_reaches_the_result() {
        for url in [
            "https://ghp_secret@github.com/acme/acme-kb.git",
            "https://sam:ghp_secret@github.com/acme/acme-kb.git",
            "https://oauth2:ghp_secret@github.com/acme/acme-kb.git",
            "ghp_secret@github.com:acme/acme-kb.git",
            "ssh://git:ghp_secret@github.com/acme/acme-kb.git",
        ] {
            let result = normalize_remote(url).unwrap_or_default();
            assert!(
                !result.contains("ghp_secret"),
                "{url} must not carry the token: {result}"
            );
            assert_eq!(result, "github.com/acme/acme-kb", "{url}");
        }
    }
}
