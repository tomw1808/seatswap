//! The tray icon, its menu and its tooltip (spec §8, docs/implementation.md
//! "app: GUI").
//!
//! The icon is drawn by `icon.rs`; this module only decides *what* to draw and
//! which texts the menu and tooltip carry.

use chrono::{DateTime, Local, Utc};
use seatswap_core::api::Usage;
use seatswap_core::engine::AccountView;
use seatswap_core::paths::AppPaths;
use seatswap_core::store::{IconLimit, IconText, LoginState, Settings, TrayText};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use super::format;
use super::worker::{LiveView, Snapshot};
use super::GuiState;
use crate::icon::{self, IconSpec};

/// The grey of the unknown ring (`#555b66`).
const GREY: [u8; 3] = [0x55, 0x5b, 0x66];
/// Pixel size of the image handed to the platform.
const ICON_SIZE: u32 = 44;
/// Tray id used by `update`.
const TRAY_ID: &str = "seatswap-tray";
/// The longest tooltip the platform shows in full, in UTF-16 units. Windows
/// cuts a tray tooltip without notice at 128 units including the closing NUL.
#[cfg(windows)]
const TOOLTIP_LIMIT: Option<usize> = Some(127);
#[cfg(not(windows))]
const TOOLTIP_LIMIT: Option<usize> = None;

fn active_view(views: &[AccountView]) -> Option<&AccountView> {
    views.iter().find(|view| view.is_active)
}

/// The icon for the live account and the `iconLimit` and `iconText` settings.
pub(crate) fn icon_spec(views: &[AccountView], settings: Option<&Settings>) -> IconSpec {
    let Some(view) = active_view(views).filter(|_| settings.is_some()) else {
        return IconSpec {
            text: "?".to_string(),
            account_color: GREY,
            ring: None,
            locked: false,
            monochrome: false,
        };
    };
    let Some(settings) = settings else {
        return IconSpec {
            text: "?".to_string(),
            account_color: GREY,
            ring: None,
            locked: false,
            monochrome: false,
        };
    };
    let color = format::parse_color(&view.color).unwrap_or(GREY);
    let limit = view.usage.as_ref().map(|usage| match settings.icon_limit {
        IconLimit::Binding => usage.binding(),
        IconLimit::Session => usage.session(),
        IconLimit::Week => usage.weekly_all(),
    });
    let limit = limit.flatten();
    let ring = limit.map(|limit| {
        let level_color = format::parse_color(limit.level().color()).unwrap_or(GREY);
        (limit.percent, level_color)
    });
    let text = match settings.icon_text {
        IconText::Letters => view.abbr.clone(),
        // No reading yet: the letters, beside the grey ring that says so.
        IconText::Percent => {
            limit.map_or_else(|| view.abbr.clone(), |limit| icon_percent(limit.percent))
        }
    };
    IconSpec {
        text,
        account_color: color,
        ring,
        locked: limit.is_some_and(|limit| limit.percent >= 100.0),
        monochrome: false,
    }
}

/// The ring's percent as the disc's text (`iconText` `percent`): the whole
/// percent, and 100 (or more) as `99`, so the disc never holds three digits
/// (owner, 18 Nov 2026). A full limit still shows: the ring is closed and the
/// lock badge is drawn.
fn icon_percent(percent: f64) -> String {
    debug_assert!(percent >= 0.0, "a usage percent is never negative");
    let whole = percent.round();
    if whole >= 100.0 {
        "99".to_string()
    } else {
        format!("{whole:.0}")
    }
}

/// The label next to the tray icon (`trayText`).
pub(crate) fn tray_text(
    views: &[AccountView],
    settings: Option<&Settings>,
    now: DateTime<Utc>,
) -> String {
    let Some(settings) = settings else {
        return String::new();
    };
    if settings.tray_text == TrayText::Off {
        return String::new();
    }
    let Some(view) = active_view(views) else {
        return String::new();
    };
    let Some(limit) = view.usage.as_ref().and_then(Usage::binding) else {
        return String::new();
    };
    let percent = format::percent(limit.percent);
    if settings.tray_text == TrayText::PercentReset {
        if let Some(resets_at) = limit.resets_at {
            return format!(
                "{percent} · {}",
                format::format_reset_at(resets_at, now, &Local)
            );
        }
    }
    percent
}

/// One account's menu item text:
/// `AC  Acme   97 % · 1h12m` and `✓` for the live account;
/// `AC  Acme   logged out` when the stored login ended.
pub(crate) fn menu_text(view: &AccountView, now: DateTime<Utc>) -> String {
    let mut text = format!("{}  {}", view.abbr, view.label);
    if view.login_state == LoginState::LoggedOut {
        text.push_str("   logged out");
    } else if let Some(limit) = view.usage.as_ref().and_then(Usage::binding) {
        text.push_str(&format!("   {} %", format::integer_percent(limit.percent)));
        if let Some(resets_at) = limit.resets_at {
            text.push_str(" · ");
            text.push_str(&format::format_reset_at(resets_at, now, &Local));
        }
    } else {
        text.push_str("   no usage");
    }
    if view.is_active {
        text.push_str(" ✓");
    }
    text
}

/// `text` if it has at most `max_units` UTF-16 units; otherwise its longest
/// prefix that, with "…" appended, has at most `max_units` units. Never splits
/// a character.
fn fit_tooltip(text: &str, max_units: usize) -> String {
    if text.encode_utf16().count() <= max_units {
        return text.to_string();
    }
    let budget = max_units.saturating_sub('…'.len_utf16());
    let mut used = 0;
    let mut end = 0;
    for (index, character) in text.char_indices() {
        used += character.len_utf16();
        if used > budget {
            break;
        }
        end = index + character.len_utf8();
    }
    format!("{}…", &text[..end])
}

/// The tooltip as handed to the tray: [`tooltip`], cut to [`TOOLTIP_LIMIT`]
/// where the platform has one, so the cut is visible ("…") and never splits a
/// character.
fn tray_tooltip(snapshot: &Snapshot, now: DateTime<Utc>) -> String {
    let text = tooltip(snapshot, now);
    match TOOLTIP_LIMIT {
        Some(max_units) => fit_tooltip(&text, max_units),
        None => text,
    }
}

/// The tooltip of the live account (spec §8.1), or the startup error so it can
/// be read without opening the panel.
pub(crate) fn tooltip(snapshot: &Snapshot, now: DateTime<Utc>) -> String {
    if let Some(error) = &snapshot.error {
        return format!("seatswap — {error}");
    }
    match active_view(&snapshot.views) {
        Some(view) => match &view.usage {
            Some(usage) => format::tooltip(view, usage, now),
            None => format!("{} · {} — no usage yet", view.abbr, view.label),
        },
        None => match &snapshot.live {
            LiveView::Unknown { email, org_name } => format!(
                "seatswap — new login: {}",
                email
                    .as_deref()
                    .or(org_name.as_deref())
                    .unwrap_or("unknown")
            ),
            _ => "seatswap — no live account".to_string(),
        },
    }
}

/// Create the tray icon from the current snapshot. Without a tray to show it
/// in (on Linux: no StatusNotifierWatcher, because the desktop has no tray
/// support or its panel starts after seatswap at login), seatswap keeps running
/// and says so; the pop-up stays reachable with `seatswap panel`, and the icon
/// is created as soon as a tray appears.
pub(crate) fn start(app: &AppHandle) {
    if let Err(error) = try_build(app) {
        let message = no_tray_message(&error);
        eprintln!("seatswap: {message}");
        let paths = app.state::<GuiState>().app.clone();
        wait_for_a_tray(app, &paths, message);
    }
}

fn no_tray_message(error: &str) -> String {
    format!(
        "no system tray to show the icon in ({error}). The icon appears as soon as the \
         desktop's tray runs (on GNOME it needs the AppIndicator extension); until then \
         `seatswap panel` opens the pop-up"
    )
}

fn try_build(app: &AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<GuiState>()
        .ok_or_else(|| "the app state is missing".to_string())?;
    let snapshot = state.snapshot()?;
    let tray = build(app, &snapshot).map_err(|error| error.to_string())?;
    *super::lock(&state.tray) = Some(tray);
    Ok(())
}

/// Watch the session bus for the StatusNotifierWatcher and create the icon when
/// it appears. Once the icon exists, ksni registers it again by itself whenever
/// the watcher restarts.
///
/// At login the desktop starts its tray and seatswap at the same moment
/// (Cinnamon: `xapp-sn-watcher` and the autostart entry, same second), so the
/// first try often comes too early. The notification is therefore shown only
/// when no tray has appeared after [`TRAY_GRACE`].
#[cfg(target_os = "linux")]
fn wait_for_a_tray(app: &AppHandle, paths: &AppPaths, message: String) {
    use gtk::{gio, glib};
    use std::sync::{Arc, Mutex};

    // Holds the watch id (`gio::bus_watch_name` returns it after the closure exists).
    let watch = Arc::new(Mutex::new(None));
    let appeared_watch = Arc::clone(&watch);
    let appeared_app = app.clone();
    let id = gio::bus_watch_name(
        gio::BusType::Session,
        STATUS_NOTIFIER_WATCHER,
        gio::BusNameWatcherFlags::NONE,
        move |_, _, _| match try_build(&appeared_app) {
            Ok(()) => {
                eprintln!("seatswap: a system tray appeared; the icon is shown");
                if let Some(id) = super::lock(&appeared_watch).take() {
                    gio::bus_unwatch_name(id);
                }
            }
            Err(error) => eprintln!("seatswap: tray icon: {error}"),
        },
        |_, _| {},
    );
    *super::lock(&watch) = Some(id);

    let app = app.clone();
    let paths = paths.clone();
    glib::timeout_add_local_once(TRAY_GRACE, move || {
        let shown = app
            .try_state::<GuiState>()
            .is_some_and(|state| super::lock(&state.tray).is_some());
        if !shown {
            super::notify::send_error(&paths, &message);
        }
    });
}

/// Elsewhere the tray is part of the system; there is nothing to wait for, so
/// the user is told at once.
#[cfg(not(target_os = "linux"))]
fn wait_for_a_tray(_app: &AppHandle, paths: &AppPaths, message: String) {
    super::notify::send_error(paths, &message);
}

/// The D-Bus name of the tray host registry on Linux (StatusNotifierItem spec).
#[cfg(target_os = "linux")]
const STATUS_NOTIFIER_WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// How long seatswap waits for a tray at startup before it notifies that there
/// is none. At login on Linux Mint the tray came up within the same second as
/// seatswap (2026-11-11); 30 s leaves room for a slow, busy login.
#[cfg(target_os = "linux")]
const TRAY_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

fn build(app: &AppHandle, snapshot: &Snapshot) -> tauri::Result<TrayIcon> {
    let entries = menu_entries(snapshot, Utc::now());
    let menu = build_menu(app, &entries)?;
    let spec = icon_spec(&snapshot.views, snapshot.settings.as_ref());
    let image = Image::new_owned(icon::render(&spec, ICON_SIZE), ICON_SIZE, ICON_SIZE);
    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .title(tray_text(
            &snapshot.views,
            snapshot.settings.as_ref(),
            Utc::now(),
        ))
        .tooltip(tray_tooltip(snapshot, Utc::now()))
        .icon(image)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Err(error) = super::panel::toggle(tray.app_handle()) {
                    eprintln!("seatswap: panel: {error}");
                }
            }
        })
        .build(app)
        .inspect(|_| *applied_menu() = Some(AppliedMenu { entries, menu }))
}

/// Update icon, tooltip, title and menu from a new snapshot.
pub(crate) fn update(tray: &TrayIcon, snapshot: &Snapshot) {
    let now = Utc::now();
    let spec = icon_spec(&snapshot.views, snapshot.settings.as_ref());
    let image = Image::new_owned(icon::render(&spec, ICON_SIZE), ICON_SIZE, ICON_SIZE);
    if let Err(error) = tray.set_icon(Some(image)) {
        eprintln!("seatswap: tray icon: {error}");
    }
    if let Err(error) = tray.set_tooltip(Some(tray_tooltip(snapshot, now))) {
        eprintln!("seatswap: tray tooltip: {error}");
    }
    let title = tray_text(&snapshot.views, snapshot.settings.as_ref(), now);
    let empty = title.is_empty();
    if let Err(error) = tray.set_title(if empty { None::<String> } else { Some(title) }) {
        eprintln!("seatswap: tray title: {error}");
    }
    if let Err(error) = apply_menu(tray, menu_entries(snapshot, now)) {
        eprintln!("seatswap: tray menu: {error}");
    }
}

/// One line of the tray menu: an item with its id and text, or a separator.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MenuEntry {
    Item { id: String, text: String },
    Separator,
}

impl MenuEntry {
    fn item(id: impl Into<String>, text: impl Into<String>) -> MenuEntry {
        MenuEntry::Item {
            id: id.into(),
            text: text.into(),
        }
    }

    /// The entry without its text: two menus with the same layout differ only
    /// in texts, which an open menu can take in place.
    fn layout(&self) -> Option<&str> {
        match self {
            MenuEntry::Item { id, .. } => Some(id),
            MenuEntry::Separator => None,
        }
    }
}

/// The menu a snapshot asks for: one item per account, *Save current login*
/// for an unknown login, the windows, and *Quit*.
fn menu_entries(snapshot: &Snapshot, now: DateTime<Utc>) -> Vec<MenuEntry> {
    let mut entries: Vec<MenuEntry> = snapshot
        .views
        .iter()
        .map(|view| MenuEntry::item(format!("account:{}", view.id), menu_text(view, now)))
        .collect();
    if let LiveView::Unknown { email, org_name } = &snapshot.live {
        let who = email
            .as_deref()
            .or(org_name.as_deref())
            .unwrap_or("new login");
        entries.push(MenuEntry::item(
            "save-current",
            format!("Save current login ({who})…"),
        ));
    }
    entries.push(MenuEntry::Separator);
    for (id, text) in [
        ("open-panel", "Open panel…"),
        ("add-account", "Add account…"),
        ("sessions", "Sessions…"),
        ("statistics", "Statistics…"),
        ("settings", "Settings…"),
    ] {
        entries.push(MenuEntry::item(id, text));
    }
    entries.push(MenuEntry::Separator);
    entries.push(MenuEntry::item("quit", "Quit seatswap"));
    entries
}

fn build_menu(app: &AppHandle, entries: &[MenuEntry]) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for entry in entries {
        match entry {
            MenuEntry::Item { id, text } => {
                let item = MenuItem::with_id(app, id.as_str(), text, true, None::<&str>)?;
                menu.append(&item)?;
            }
            MenuEntry::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
        }
    }
    Ok(menu)
}

/// The menu the tray shows now, with the entries it was built from.
struct AppliedMenu {
    entries: Vec<MenuEntry>,
    menu: Menu<tauri::Wry>,
}

/// The tray has one menu; this module alone keeps it.
fn applied_menu() -> std::sync::MutexGuard<'static, Option<AppliedMenu>> {
    static APPLIED: std::sync::Mutex<Option<AppliedMenu>> = std::sync::Mutex::new(None);
    APPLIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a new list of entries needs from the menu the tray shows.
#[derive(Debug, PartialEq, Eq)]
enum MenuChange {
    /// The same entries: nothing to do.
    None,
    /// The same items in the same order; these items' texts changed.
    Texts(Vec<(String, String)>),
    /// Items came, went or moved: a new menu.
    Rebuild,
}

fn menu_change(applied: Option<&[MenuEntry]>, entries: &[MenuEntry]) -> MenuChange {
    let Some(applied) = applied else {
        return MenuChange::Rebuild;
    };
    if applied == entries {
        return MenuChange::None;
    }
    let same_layout = applied.len() == entries.len()
        && applied
            .iter()
            .zip(entries)
            .all(|(old, new)| old.layout() == new.layout());
    if !same_layout {
        return MenuChange::Rebuild;
    }
    MenuChange::Texts(
        applied
            .iter()
            .zip(entries)
            .filter_map(|(old, new)| match (old, new) {
                (MenuEntry::Item { text: old, .. }, MenuEntry::Item { id, text })
                    if old != text =>
                {
                    Some((id.clone(), text.clone()))
                }
                _ => None,
            })
            .collect(),
    )
}

/// Show `entries` in the tray's menu. Replacing the menu closes it while it is
/// open on Windows, and a snapshot comes with every sessions pass (about once a
/// second while a session works), so the menu is replaced only when its items
/// change; a changed text is set on the item in place, which an open menu
/// keeps.
fn apply_menu(tray: &TrayIcon, entries: Vec<MenuEntry>) -> Result<(), String> {
    let mut applied = applied_menu();
    match menu_change(
        applied.as_ref().map(|menu| menu.entries.as_slice()),
        &entries,
    ) {
        MenuChange::None => Ok(()),
        MenuChange::Texts(texts) => {
            let Some(current) = applied.as_mut() else {
                return Err("the tray menu is not built".to_string());
            };
            for (id, text) in &texts {
                let Some(item) = current.menu.get(id.as_str()) else {
                    return Err(format!("the tray menu has no item {id}"));
                };
                let Some(item) = item.as_menuitem() else {
                    return Err(format!("the tray menu's {id} is not a plain item"));
                };
                item.set_text(text).map_err(|error| error.to_string())?;
            }
            current.entries = entries;
            Ok(())
        }
        MenuChange::Rebuild => {
            let menu =
                build_menu(tray.app_handle(), &entries).map_err(|error| error.to_string())?;
            tray.set_menu(Some(menu.clone()))
                .map_err(|error| error.to_string())?;
            *applied = Some(AppliedMenu { entries, menu });
            Ok(())
        }
    }
}

fn menu_event(app: &AppHandle, id: &str) {
    match id {
        "open-panel" => super::show_panel(app, "main"),
        "add-account" | "save-current" => super::show_panel(app, "add"),
        "sessions" => super::show_window(app, "sessions"),
        "statistics" => super::show_window(app, "statistics"),
        "settings" => super::show_window(app, "settings"),
        "quit" => app.exit(0),
        other => {
            if let Some(key) = other.strip_prefix("account:") {
                super::request_switch(
                    app,
                    key.to_string(),
                    seatswap_core::switcher::UnsavedPolicy::Refuse,
                    None,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use seatswap_core::api::{Limit, LimitKind, Severity};
    use seatswap_core::store::Settings;

    use chrono::Duration;

    const ACCOUNT: &str = "account:5f0c2a7e-3b1d-4c8e-9a6f-2d7b8e1c4f30";

    fn entries(account_text: &str, save_current: bool) -> Vec<MenuEntry> {
        let mut entries = vec![MenuEntry::item(ACCOUNT, account_text)];
        if save_current {
            entries.push(MenuEntry::item("save-current", "Save current login (sam)…"));
        }
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::item("quit", "Quit seatswap"));
        entries
    }

    #[test]
    fn the_same_menu_is_left_alone() {
        let menu = entries("LB · 41 % · resets in 2h", false);
        assert_eq!(menu_change(Some(&menu), &menu), MenuChange::None);
    }

    #[test]
    fn a_changed_text_is_set_in_place() {
        let old = entries("LB · 41 % · resets in 2h", false);
        let new = entries("LB · 42 % · resets in 2h", false);
        assert_eq!(
            menu_change(Some(&old), &new),
            MenuChange::Texts(vec![(
                ACCOUNT.to_string(),
                "LB · 42 % · resets in 2h".to_string()
            )])
        );
    }

    #[test]
    fn a_new_item_or_no_menu_yet_builds_the_menu_again() {
        let old = entries("LB · 41 %", false);
        let new = entries("LB · 41 %", true);
        assert_eq!(menu_change(Some(&old), &new), MenuChange::Rebuild);
        assert_eq!(menu_change(Some(&new), &old), MenuChange::Rebuild);
        assert_eq!(menu_change(None, &new), MenuChange::Rebuild);
    }

    fn usage(limits: Vec<Limit>) -> Usage {
        Usage {
            limits,
            extra_usage_enabled: false,
            warnings: Vec::new(),
        }
    }

    fn limit(
        kind: LimitKind,
        percent: f64,
        active: bool,
        resets_at: Option<DateTime<Utc>>,
    ) -> Limit {
        Limit {
            kind,
            percent,
            severity: Severity::Normal,
            resets_at,
            model: None,
            is_active: active,
        }
    }

    fn view(
        abbr: &str,
        label: &str,
        is_active: bool,
        usage: Option<Usage>,
        login_state: LoginState,
    ) -> AccountView {
        AccountView {
            id: format!("id-{abbr}"),
            label: label.to_string(),
            abbr: abbr.to_string(),
            color: "#1e3a8a".to_string(),
            email: Some("someone@example.com".to_string()),
            plan: None,
            auto_burn: true,
            is_active,
            login_state,
            login_ends_at_ms: None,
            login_days_left: None,
            usage,
            usable: true,
            blocked_until: None,
            blocker: None,
            fetched_at: None,
            last_error: None,
        }
    }

    fn both_limits() -> Usage {
        let now = Utc::now();
        usage(vec![
            limit(
                LimitKind::Session,
                97.0,
                true,
                Some(now + Duration::minutes(72)),
            ),
            limit(
                LimitKind::WeeklyAll,
                48.0,
                false,
                Some(now + Duration::hours(72)),
            ),
        ])
    }

    fn weekly_only() -> Usage {
        usage(vec![limit(
            LimitKind::WeeklyAll,
            31.0,
            true,
            Some(Utc::now() + Duration::hours(72)),
        )])
    }

    #[test]
    fn icon_limit_chooses_binding_session_or_week() {
        let mut settings = Settings::default();
        let views = vec![view(
            "AC",
            "Acme",
            true,
            Some(both_limits()),
            LoginState::Ok,
        )];

        settings.icon_limit = IconLimit::Binding;
        let spec = icon_spec(&views, Some(&settings));
        // Both limits exist; `binding()` takes the active session limit.
        assert_eq!(spec.ring.map(|(percent, _)| percent), Some(97.0));

        settings.icon_limit = IconLimit::Session;
        let spec = icon_spec(&views, Some(&settings));
        assert_eq!(spec.ring.map(|(percent, _)| percent), Some(97.0));

        settings.icon_limit = IconLimit::Week;
        let spec = icon_spec(&views, Some(&settings));
        assert_eq!(spec.ring.map(|(percent, _)| percent), Some(48.0));
    }

    #[test]
    fn icon_text_percent_shows_the_rings_limit_with_100_as_99() {
        let mut settings = Settings::default();
        let views = vec![view(
            "AC",
            "Acme",
            true,
            Some(both_limits()),
            LoginState::Ok,
        )];
        assert_eq!(icon_spec(&views, Some(&settings)).text, "AC");

        settings.icon_text = IconText::Percent;
        assert_eq!(icon_spec(&views, Some(&settings)).text, "97");
        settings.icon_limit = IconLimit::Week;
        assert_eq!(icon_spec(&views, Some(&settings)).text, "48");

        settings.icon_limit = IconLimit::Session;
        for (percent, shown) in [
            (100.0, "99"),
            (99.6, "99"),
            (99.4, "99"),
            (7.2, "7"),
            (0.0, "0"),
        ] {
            let full = vec![view(
                "AC",
                "Acme",
                true,
                Some(usage(vec![limit(LimitKind::Session, percent, true, None)])),
                LoginState::Ok,
            )];
            assert_eq!(icon_spec(&full, Some(&settings)).text, shown, "{percent} %");
        }
    }

    #[test]
    fn icon_text_percent_without_a_reading_shows_the_letters() {
        let settings = Settings {
            icon_text: IconText::Percent,
            ..Settings::default()
        };
        let views = vec![view("AC", "Acme", true, None, LoginState::Ok)];
        let spec = icon_spec(&views, Some(&settings));
        assert_eq!(spec.text, "AC");
        assert_eq!(spec.ring, None);
    }

    #[test]
    fn a_full_limit_is_locked() {
        let settings = Settings {
            icon_limit: IconLimit::Session,
            ..Settings::default()
        };
        let views = vec![view(
            "AC",
            "Acme",
            true,
            Some(usage(vec![limit(LimitKind::Session, 100.0, true, None)])),
            LoginState::Ok,
        )];
        let spec = icon_spec(&views, Some(&settings));
        assert!(spec.locked);
        assert_eq!(spec.text, "AC");
        assert_eq!(spec.account_color, [0x1e, 0x3a, 0x8a]);
    }

    #[test]
    fn no_usage_gives_a_grey_ring_and_no_live_account_gets_the_unknown_icon() {
        let settings = Settings::default();
        let views = vec![view("AC", "Acme", true, None, LoginState::Ok)];
        assert_eq!(icon_spec(&views, Some(&settings)).ring, None);

        let spec = icon_spec(&[], Some(&settings));
        assert_eq!(spec.text, "?");
        assert_eq!(spec.account_color, GREY);
        assert_eq!(spec.ring, None);
        assert!(!spec.locked);
    }

    #[test]
    fn tray_text_follows_the_setting() {
        let now = Utc::now();
        let views = vec![view(
            "AC",
            "Acme",
            true,
            Some(both_limits()),
            LoginState::Ok,
        )];

        let settings = Settings::default();
        assert_eq!(tray_text(&views, Some(&settings), now), "97%");

        let settings = Settings {
            tray_text: TrayText::PercentReset,
            ..Settings::default()
        };
        assert!(tray_text(&views, Some(&settings), now).starts_with("97% · "));

        let settings = Settings {
            tray_text: TrayText::Off,
            ..Settings::default()
        };
        assert_eq!(tray_text(&views, Some(&settings), now), "");
        assert_eq!(tray_text(&[], Some(&settings), now), "");
    }

    #[test]
    fn menu_texts_show_the_binding_limit_and_the_live_mark() {
        let now = Utc::now();
        let with_both = view("AC", "Acme", true, Some(both_limits()), LoginState::Ok);
        let text = menu_text(&with_both, now);
        assert!(text.starts_with("AC  Acme   97 % · "), "{text}");
        assert!(text.ends_with("✓"), "{text}");

        let weekly = view("P", "Private", false, Some(weekly_only()), LoginState::Ok);
        let text = menu_text(&weekly, now);
        assert!(text.starts_with("P  Private   31 % · "), "{text}");
        assert!(!text.ends_with("✓"), "{text}");

        let logged_out = view("AC", "Acme", false, None, LoginState::LoggedOut);
        let text = menu_text(&logged_out, now);
        assert!(text.contains("logged out"), "{text}");
    }

    #[test]
    fn fit_tooltip_keeps_a_text_that_fits() {
        assert_eq!(fit_tooltip("AS · Acme — 42 %", 127), "AS · Acme — 42 %");
        let exact = "a".repeat(127);
        assert_eq!(fit_tooltip(&exact, 127), exact);
    }

    #[test]
    fn fit_tooltip_cuts_one_unit_over_and_marks_it() {
        let text = format!("{}b", "a".repeat(127));
        let fitted = fit_tooltip(&text, 127);
        assert_eq!(fitted, format!("{}…", "a".repeat(126)));
        assert_eq!(fitted.encode_utf16().count(), 127);
    }

    #[test]
    fn fit_tooltip_never_splits_a_two_unit_character() {
        // 125 units, then an emoji of two units across the cut point at 126.
        let text = format!("{}🔥 and more", "a".repeat(125));
        let fitted = fit_tooltip(&text, 127);
        assert_eq!(fitted, format!("{}…", "a".repeat(125)));
        assert!(fitted.encode_utf16().count() <= 127);
    }

    #[test]
    fn tooltips_follow_the_live_account_shape() {
        let now = Utc::now();
        let views = vec![view(
            "AC",
            "Acme",
            true,
            Some(both_limits()),
            LoginState::Ok,
        )];
        let snapshot = snapshot_with(views, None);
        let text = tooltip(&snapshot, now);
        assert!(text.starts_with("AC · Acme — session 97 %"), "{text}");
        assert!(text.contains("week 48 %"), "{text}");

        let views = vec![view(
            "P",
            "Private",
            true,
            Some(weekly_only()),
            LoginState::Ok,
        )];
        let text = tooltip(&snapshot_with(views, None), now);
        assert!(text.starts_with("P · Private — week 31 %"), "{text}");

        let text = tooltip(&snapshot_with(Vec::new(), None), now);
        assert_eq!(text, "seatswap — no live account");

        let text = tooltip(&snapshot_with(Vec::new(), Some("accounts.json: nope")), now);
        assert!(text.contains("accounts.json: nope"), "{text}");
    }

    fn snapshot_with(views: Vec<AccountView>, error: Option<&str>) -> Snapshot {
        Snapshot {
            accounts: Vec::new(),
            provider_subscriptions: Vec::new(),
            codex_logins: Vec::new(),
            active_id: views
                .iter()
                .find(|view| view.is_active)
                .map(|view| view.id.clone()),
            views,
            live: LiveView::NoLogin,
            burn: None,
            warnings: Vec::new(),
            pending: None,
            secrets_warning: None,
            last_poll_at: None,
            error: error.map(str::to_string),
            sessions: None,
            providers: Vec::new(),
            settings: Some(Settings::default()),
            claude_config_dir: String::new(),
            claude_not_set_up: false,
        }
    }
}
