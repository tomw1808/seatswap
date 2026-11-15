//! The large window: its label, its areas and the one command that opens it
//! (spec §3; docs/implementation.md "app: GUI", "The large window").
//!
//! The window itself is a second Tauri webview window with its own page
//! (`window.html`). The pop-up keeps `panel.rs`: both windows exist at the same
//! time, and a `snapshot` event reaches each of them.

#[cfg(feature = "gui")]
use tauri::webview::PageLoadEvent;
#[cfg(feature = "gui")]
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Window label used by `tauri.conf.json`'s capability and by every command.
#[cfg(feature = "gui")]
pub(crate) const WINDOW_LABEL: &str = "main";

/// The four areas the window can show, in the mock-up's order.
pub(crate) const AREAS: [&str; 4] = ["sessions", "statistics", "statusline", "settings"];

/// Start size of the large window (spec §3).
#[cfg(feature = "gui")]
const WINDOW_WIDTH: f64 = 1280.0;
#[cfg(feature = "gui")]
const WINDOW_HEIGHT: f64 = 800.0;

/// `"settings/accounts"` → `("settings", Some("accounts"))`;
/// `"settings/accounts?<id>"` → `("settings", Some("accounts?<id>"))`;
/// `"settings"` → `("settings", None)`. Anything else is an error with the
/// reason: an unknown area, an empty sub-area, one that is not a plain name
/// (`a-z`, `0-9`, `-`) optionally followed by `?<item>` (the item a plain name
/// too), or more than one `/`.
pub(crate) fn parse_target(target: &str) -> Result<(&str, Option<&str>), String> {
    let mut parts = target.split('/');
    let area = parts.next().unwrap_or_default();
    if !AREAS.contains(&area) {
        return Err(format!(
            "unknown area {area:?}: use one of {}",
            AREAS.join(", ")
        ));
    }
    match (parts.next(), parts.next()) {
        (None, None) => Ok((area, None)),
        (Some(""), None) => Err(format!(
            "the sub-area of {target:?} is empty: use <area>/<sub>"
        )),
        (Some(sub), None) if is_sub_area(sub) => Ok((area, Some(sub))),
        (Some(sub), None) => Err(format!(
            "the sub-area {sub:?} may hold only a-z, 0-9 and \"-\", optionally followed by ?<item>"
        )),
        _ => Err(format!(
            "{target:?} names more than one sub-area: use <area> or <area>/<sub>"
        )),
    }
}

/// A sub-area is a name like `accounts` or `forecast`, optionally followed by
/// `?<item>` (`settings/accounts?<account id>`): lower-case letters, digits and
/// `-`. It travels over the socket and in the page's address unencoded, so
/// nothing else may be in it.
fn is_sub_area(sub: &str) -> bool {
    match sub.split_once('?') {
        Some((name, item)) => is_slug(name) && is_slug(item),
        None => is_slug(sub),
    }
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Create the window (1280 × 800, resizable, decorated, in the taskbar, not
/// always on top) or focus it, and send it `target`.
///
/// A newly created window loads `window.html#<target>`; an existing one is
/// shown, un-minimized, focused and sent `open-area`. The two windows each have
/// their own event name, because `emit` reaches every window: the pop-up keeps
/// `navigate` (decision 3 of the plan's lead).
#[cfg(feature = "gui")]
pub(crate) fn show(app: &AppHandle, target: &str) -> Result<(), String> {
    parse_target(target)?;
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        window.show().map_err(|error| error.to_string())?;
        window.unminimize().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        window
            .emit("open-area", target)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    create(app, target)
}

/// Build the large window on its first opening, with the target in its address.
#[cfg(feature = "gui")]
fn create(app: &AppHandle, target: &str) -> Result<(), String> {
    let initial = target.to_string();
    let window =
        WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("window.html".into()))
            .title("seatswap")
            .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
            .resizable(true)
            .decorations(true)
            .always_on_top(false)
            .skip_taskbar(false)
            .on_page_load(move |window, payload| {
                if payload.event() == PageLoadEvent::Finished {
                    let _ = window.eval(format!("window.location.hash = {initial:?};"));
                }
            })
            .build()
            .map_err(|error| error.to_string())?;
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

/// `open_window("settings")` from a page: the same as `show`.
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) fn open_window(app: AppHandle, target: String) -> Result<(), String> {
    show(&app, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_an_area_with_an_optional_sub() {
        assert_eq!(
            parse_target("settings/accounts"),
            Ok(("settings", Some("accounts")))
        );
        assert_eq!(
            parse_target("settings/accounts?8564d960fb9a498daede9211fc09c9f8"),
            Ok((
                "settings",
                Some("accounts?8564d960fb9a498daede9211fc09c9f8")
            ))
        );
        assert!(parse_target("settings/accounts?").is_err());
        assert!(parse_target("settings/?x").is_err());
        assert!(parse_target("settings/accounts?a b").is_err());
        assert!(parse_target("nonsense").is_err());
        assert_eq!(parse_target("settings"), Ok(("settings", None)));
        assert!(parse_target("settings/").is_err());
        assert!(parse_target("settings/a/b").is_err());
        assert!(parse_target("settings/a b").is_err());
        assert!(parse_target("settings/a\nb").is_err());
        assert!(parse_target("settings/Übersicht").is_err());
    }
}
