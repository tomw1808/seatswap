//! Paths the app's tests share. A test that uses the made-up home folder builds
//! it, and every path below it, here, so the same test spells its paths and its
//! expected `~` text the way the platform does.

use std::path::{Path, PathBuf};

/// The made-up home folder: `/home/sam` on Unix, `C:\Users\sam` on Windows.
const HOME: &str = if cfg!(windows) {
    r"C:\Users\sam"
} else {
    "/home/sam"
};

/// The made-up home folder the tests use.
pub(crate) fn home() -> &'static Path {
    Path::new(HOME)
}

/// `rest`, written with `/`, below `base`, joined by the platform's own rules.
pub(crate) fn under(base: &Path, rest: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rest.split('/').filter(|part| !part.is_empty()) {
        path.push(part);
    }
    path
}

/// `rest`, written with `/`, below [`home`].
pub(crate) fn at_home(rest: &str) -> PathBuf {
    under(home(), rest)
}

/// `rest`, written with `/`, as the `~` text the panel and status line show:
/// `~/projects/atlas` on Unix, `~\projects\atlas` on Windows.
pub(crate) fn tilde(rest: &str) -> String {
    if rest.is_empty() {
        "~".to_string()
    } else {
        format!(
            "~{}{}",
            std::path::MAIN_SEPARATOR,
            rest.replace('/', std::path::MAIN_SEPARATOR_STR)
        )
    }
}
