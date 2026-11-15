//! The agent skill file (spec §6.4): the one place that writes, replaces and
//! removes `<claude config dir>/skills/seatswap/SKILL.md`. The command line and
//! the settings page both call it, so the file is handled the same way.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::Serialize;

use seatswap_core::atomic::{self, Access};
use seatswap_core::paths::ClaudePaths;

/// The skill shipped with the program (spec §6.4).
pub(crate) const AGENT_SKILL: &str = include_str!("../assets/agent-skill/SKILL.md");

/// What the skill file looks like now (or after an install/uninstall).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillStatus {
    pub path: PathBuf,
    /// The file exists, whether or not its content is seatswap's.
    pub installed: bool,
    /// The file exists and its content is not the bundled text.
    pub differs: bool,
}

/// Where the skill lives and how it differs from the bundled one.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error(
        "{path} already exists and differs from seatswap's bundled skill; use --force to replace it"
    )]
    DiffersOnInstall { path: PathBuf },
    #[error("{path} differs from seatswap's bundled skill; use --force to remove it")]
    DiffersOnUninstall { path: PathBuf },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl SkillError {
    fn io(path: PathBuf, source: io::Error) -> SkillError {
        SkillError::Io { path, source }
    }
}

/// The skill file's folder and path: `<skills dir>/seatswap/SKILL.md`.
fn locations(claude: &ClaudePaths) -> (PathBuf, PathBuf) {
    let folder = claude.skills_dir().join("seatswap");
    let path = folder.join("SKILL.md");
    (folder, path)
}

/// What the skill file is now. A missing file is `installed: false`, not an
/// error; any other read failure names the path.
pub fn status(claude: &ClaudePaths) -> Result<SkillStatus, SkillError> {
    let (_, path) = locations(claude);
    match fs::read(&path) {
        Ok(bytes) => Ok(SkillStatus {
            path,
            installed: true,
            differs: bytes != AGENT_SKILL.as_bytes(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SkillStatus {
            path,
            installed: false,
            differs: false,
        }),
        Err(source) => Err(SkillError::io(path, source)),
    }
}

/// Write the bundled skill. An existing file that already holds it is left
/// untouched; one that differs is an error unless `force`.
pub fn install(claude: &ClaudePaths, force: bool) -> Result<SkillStatus, SkillError> {
    let (folder, path) = locations(claude);
    let bundled = AGENT_SKILL.as_bytes();
    match fs::read(&path) {
        Ok(existing) if existing == bundled => {
            return Ok(SkillStatus {
                path,
                installed: true,
                differs: false,
            });
        }
        Ok(_) if !force => return Err(SkillError::DiffersOnInstall { path }),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(SkillError::io(path, source)),
    }
    atomic::ensure_private_dir(&folder).map_err(|source| SkillError::io(folder, source))?;
    atomic::write_atomic(&path, bundled, Access::Private)
        .map_err(|source| SkillError::io(path.clone(), source))?;
    Ok(SkillStatus {
        path,
        installed: true,
        differs: false,
    })
}

/// Remove the skill file and its folder when it is empty. A missing file is
/// `installed: false` and no error; a file that differs is an error unless
/// `force`.
pub fn uninstall(claude: &ClaudePaths, force: bool) -> Result<SkillStatus, SkillError> {
    let (folder, path) = locations(claude);
    let existing = match fs::read(&path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(SkillStatus {
                path,
                installed: false,
                differs: false,
            });
        }
        Err(source) => return Err(SkillError::io(path, source)),
    };
    if existing != AGENT_SKILL.as_bytes() && !force {
        return Err(SkillError::DiffersOnUninstall { path });
    }
    fs::remove_file(&path).map_err(|source| SkillError::io(path.clone(), source))?;
    match fs::remove_dir(&folder) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(source) => return Err(SkillError::io(folder, source)),
    }
    Ok(SkillStatus {
        path,
        installed: false,
        differs: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> (tempfile::TempDir, ClaudePaths) {
        let tmp = tempfile::tempdir().expect("a temp folder");
        let claude = ClaudePaths::for_config_dir(&tmp.path().join("claude"), tmp.path());
        (tmp, claude)
    }

    #[test]
    fn status_sees_a_missing_file_as_not_installed() {
        let (_tmp, claude) = claude();
        let status = status(&claude).expect("the status");
        assert!(!status.installed);
        assert!(!status.differs);
        assert_eq!(status.path, claude.skills_dir().join("seatswap/SKILL.md"));
    }

    #[test]
    fn install_is_idempotent_and_force_replaces() {
        let (_tmp, claude) = claude();
        let first = install(&claude, false).expect("the skill is installed");
        assert!(first.installed);
        assert!(!first.differs);
        let modified = fs::metadata(&first.path)
            .expect("metadata")
            .modified()
            .expect("the modification time");
        install(&claude, false).expect("a second install writes nothing");
        assert_eq!(
            fs::metadata(&first.path)
                .expect("metadata")
                .modified()
                .expect("the modification time"),
            modified
        );

        fs::write(&first.path, "another skill").expect("a changed skill");
        assert!(matches!(
            install(&claude, false),
            Err(SkillError::DiffersOnInstall { .. })
        ));
        install(&claude, true).expect("--force replaces it");
        assert_eq!(
            fs::read_to_string(&first.path).expect("the skill"),
            AGENT_SKILL
        );
    }

    #[test]
    fn uninstall_removes_the_file_and_the_empty_folder() {
        let (_tmp, claude) = claude();
        let status = install(&claude, false).expect("the skill is installed");
        let folder = status.path.parent().expect("the skill folder");
        let removed = uninstall(&claude, false).expect("the skill is removed");
        assert!(!removed.installed);
        assert!(!status.path.exists());
        assert!(!folder.exists());
    }

    #[test]
    fn uninstall_keeps_an_edited_file_unless_forced() {
        let (_tmp, claude) = claude();
        let status = install(&claude, false).expect("the skill is installed");
        fs::write(&status.path, "my own skill").expect("an edited skill");
        assert!(matches!(
            uninstall(&claude, false),
            Err(SkillError::DiffersOnUninstall { .. })
        ));
        assert!(status.path.is_file());
        uninstall(&claude, true).expect("--force removes it");
        assert!(!status.path.exists());
    }
}
