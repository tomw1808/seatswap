//! Account looks and settings shared between hosts (spec §5, "Shared between
//! hosts"; decision N5). The medium is the history folder: an edit writes
//! `accountLook`, `priceChanged` or `settingSet` into this host's
//! `events-YYYY-MM.jsonl`, and every host takes the newest value of each field
//! from all hosts' events. Logins are never shared, and neither is the account
//! list: an account exists on a host only after its login there, and then takes
//! the shared look and price of its [`AccountRef`].
//!
//! - [`edit_events`] and [`setting_events`]: the events an edit writes.
//! - [`latest`]: per account and field, and per shared setting, the newest
//!   value over all hosts (last edit wins).
//! - [`apply`]: those values into `accounts.json`, through the same validators
//!   as an edit; a value that fails is a warning, never applied.
//! - [`seed`]: this host's values for every field no host has an event for.
//! - [`sync`]: read, seed, apply: what the tray app runs at start and every
//!   few minutes, and what an account add runs.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::history::{AccountRef, Event, EventKind, HistoryError, Hosted, Hosts, Range, Recorder};
use crate::locks::{self, LockError};
use crate::store::{self, Account, Settings, Store, StoreError, SHARED_SETTINGS};
use crate::switcher::{Edited, APP_LOCK_TIMEOUT};

#[derive(Debug, thiserror::Error)]
pub enum SharedError {
    #[error("the shared looks and settings cannot be read or written: {0}")]
    History(#[from] HistoryError),
    #[error("the shared looks and settings cannot be applied: {0}")]
    Store(#[from] StoreError),
    #[error("the shared looks and settings cannot be applied: {0}")]
    Lock(#[from] LockError),
}

/// One shared value, when it was set and the host that set it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamped<T> {
    pub value: T,
    pub at: DateTime<Utc>,
    pub host: String,
}

impl<T> Stamped<T> {
    /// Whether `self` decides over `kept`: a later time, and on the same time
    /// the later host name in sort order, so every host decides the same. The
    /// same time and host is the later line of that host's file.
    fn wins_over(&self, kept: &Stamped<T>) -> bool {
        (self.at, &self.host) >= (kept.at, &kept.host)
    }
}

/// Keep `next` in `slot` when it is newer than what the slot holds.
fn keep<T>(slot: &mut Option<Stamped<T>>, next: Stamped<T>) {
    if slot.as_ref().is_none_or(|kept| next.wins_over(kept)) {
        *slot = Some(next);
    }
}

/// A monthly price as `priceChanged` carries it: `None` is a cleared price.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedPrice {
    pub amount: Option<f64>,
    pub currency: Option<String>,
}

/// The newest shared value of each field of one account; `None` where no
/// host wrote one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedLook {
    pub label: Option<Stamped<String>>,
    pub abbr: Option<Stamped<String>>,
    pub color: Option<Stamped<String>>,
    pub auto_burn: Option<Stamped<bool>>,
    pub price: Option<Stamped<SharedPrice>>,
}

/// What [`latest`] found: the newest value per account and field, per shared
/// setting, and a warning for every event it did not take.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shared {
    pub accounts: BTreeMap<AccountRef, SharedLook>,
    pub settings: BTreeMap<String, Stamped<serde_json::Value>>,
    pub warnings: Vec<String>,
}

/// What [`apply`] changed (local abbreviations of the accounts as they are
/// afterwards, and setting keys) and the values it refused, each named in a
/// warning. [`sync`] adds the read's, [`latest`]'s and [`seed`]'s warnings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    pub accounts: Vec<String>,
    pub settings: Vec<String>,
    pub warnings: Vec<String>,
}

impl Applied {
    /// Whether `accounts.json` was written.
    pub fn changed(&self) -> bool {
        !self.accounts.is_empty() || !self.settings.is_empty()
    }
}

/// What [`seed`] wrote, and the events it could not write (a status line too
/// long for one history line, an unwritable folder), each as a warning.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seeded {
    pub written: usize,
    pub warnings: Vec<String>,
}

/// The events an account edit writes: `accountLook` with the look fields that
/// differ from before (none when only the price changed), then `priceChanged`
/// when the price or its currency changed. The one source of an edit's events
/// for the tray app and the CLI.
pub fn edit_events(edited: &Edited) -> Vec<EventKind> {
    let (before, after) = (&edited.before, &edited.account);
    let changed = |old: &String, new: &String| (old != new).then(|| new.clone());
    let look = EventKind::AccountLook {
        account: AccountRef::of(after),
        label: changed(&before.label, &after.label),
        abbr: changed(&before.abbr, &after.abbr),
        color: changed(&before.color, &after.color),
        auto_burn: (before.auto_burn != after.auto_burn).then_some(after.auto_burn),
    };
    let mut events = Vec::new();
    if !look_is_empty(&look) {
        events.push(look);
    }
    if edited.price_changed() {
        events.push(EventKind::PriceChanged {
            account: AccountRef::of(after),
            amount: after.price,
            currency: after.price_currency.clone(),
        });
    }
    events
}

/// The `settingSet` events a settings save writes: one per shared setting
/// whose value differs between `previous` and `next`. A setting outside
/// [`SHARED_SETTINGS`] writes none.
pub fn setting_events(previous: &Settings, next: &Settings) -> Result<Vec<EventKind>, StoreError> {
    let mut events = Vec::new();
    for key in SHARED_SETTINGS {
        let value = next.value_of(key)?;
        if previous.value_of(key)? != value {
            events.push(EventKind::SettingSet {
                key: key.to_string(),
                value,
            });
        }
    }
    Ok(events)
}

fn look_is_empty(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::AccountLook {
            label: None,
            abbr: None,
            color: None,
            auto_burn: None,
            ..
        }
    )
}

fn is_shared(key: &str) -> bool {
    SHARED_SETTINGS.contains(&key)
}

/// Per account and field, and per shared setting key, the newest value by
/// event time over all hosts; on the same time the later host name in sort
/// order wins, so every host decides the same whatever order it read in. A
/// `settingSet` for a key outside [`SHARED_SETTINGS`] (a newer version's, or a
/// hand-made line) is left out with a warning naming the key and the host.
pub fn latest(events: &[Hosted<Event>]) -> Shared {
    let mut shared = Shared::default();
    for hosted in events {
        let at = hosted.record.t;
        let stamp = |value| Stamped {
            value,
            at,
            host: hosted.host.clone(),
        };
        match &hosted.record.kind {
            EventKind::AccountLook {
                account,
                label,
                abbr,
                color,
                auto_burn,
            } => {
                let look = shared.accounts.entry(account.clone()).or_default();
                if let Some(label) = label {
                    keep(&mut look.label, stamp(label.clone()));
                }
                if let Some(abbr) = abbr {
                    keep(&mut look.abbr, stamp(abbr.clone()));
                }
                if let Some(color) = color {
                    keep(&mut look.color, stamp(color.clone()));
                }
                if let Some(auto_burn) = auto_burn {
                    keep(
                        &mut look.auto_burn,
                        Stamped {
                            value: *auto_burn,
                            at,
                            host: hosted.host.clone(),
                        },
                    );
                }
            }
            EventKind::PriceChanged {
                account,
                amount,
                currency,
            } => {
                let look = shared.accounts.entry(account.clone()).or_default();
                keep(
                    &mut look.price,
                    Stamped {
                        value: SharedPrice {
                            amount: *amount,
                            currency: currency.clone(),
                        },
                        at,
                        host: hosted.host.clone(),
                    },
                );
            }
            EventKind::SettingSet { key, value } => {
                if !is_shared(key) {
                    shared.warnings.push(format!(
                        "the setting {key} from host {} is not one this version shares; ignored",
                        hosted.host
                    ));
                    continue;
                }
                let mut slot = shared.settings.remove(key);
                keep(
                    &mut slot,
                    Stamped {
                        value: value.clone(),
                        at,
                        host: hosted.host.clone(),
                    },
                );
                if let Some(kept) = slot {
                    shared.settings.insert(key.clone(), kept);
                }
            }
            _ => {}
        }
    }
    shared
}

/// How a warning names a local account: `AC (Acme)`.
fn name(account: &Account) -> String {
    format!("{} ({})", account.abbr, account.label)
}

/// Write the newest shared values into `accounts.json`, under the app lock:
/// for the accounts this host has, their label, abbreviation, colour,
/// "Auto-burn may use it" and price; and the shared settings. Each value goes
/// through the validator an edit uses (`validate_label`, `validate_abbr` with
/// the other accounts' abbreviations as they will be, `validate_color`,
/// `validate_price` and the currency rule, [`Settings::with_setting`]). A
/// value that fails, including an abbreviation another local account holds,
/// is not applied and is a warning naming the account or key and the host
/// that wrote it. Applying writes no event, and the file only when a value
/// changed.
pub fn apply(store: &Store, shared: &Shared) -> Result<Applied, SharedError> {
    let _app_lock = locks::acquire_app_lock(store.app_paths(), APP_LOCK_TIMEOUT)?;
    let mut file = store.load()?;
    let mut applied = Applied::default();
    let mut changed_accounts = vec![false; file.accounts.len()];

    for (index, account) in file.accounts.iter_mut().enumerate() {
        let Some(look) = shared.accounts.get(&AccountRef::of(account)) else {
            continue;
        };
        let who = name(account);
        let mut refused = |field: &str, host: &str, error: &dyn std::fmt::Display| {
            applied.warnings.push(format!(
                "account {who}: the shared {field} from host {host} is not applied: {error}"
            ));
        };
        if let Some(label) = &look.label {
            match store::validate_label(&label.value) {
                Ok(value) if value != account.label => {
                    account.label = value;
                    changed_accounts[index] = true;
                }
                Ok(_) => {}
                Err(error) => refused("label", &label.host, &error),
            }
        }
        if let Some(color) = &look.color {
            match store::validate_color(&color.value) {
                Ok(value) if value != account.color => {
                    account.color = value;
                    changed_accounts[index] = true;
                }
                Ok(_) => {}
                Err(error) => refused("colour", &color.host, &error),
            }
        }
        if let Some(auto_burn) = &look.auto_burn {
            if auto_burn.value != account.auto_burn {
                account.auto_burn = auto_burn.value;
                changed_accounts[index] = true;
            }
        }
        if let Some(price) = &look.price {
            match checked_price(&price.value) {
                Ok((amount, currency)) => {
                    if account.price != amount || account.price_currency != currency {
                        account.price = amount;
                        account.price_currency = currency;
                        changed_accounts[index] = true;
                    }
                }
                Err(error) => refused("price", &price.host, &error),
            }
        }
    }

    for index in apply_abbrs(&mut file.accounts, shared, &mut applied.warnings) {
        changed_accounts[index] = true;
    }

    for (key, stamped) in &shared.settings {
        if !is_shared(key) {
            applied.warnings.push(format!(
                "the setting {key} from host {} is not one this version shares; ignored",
                stamped.host
            ));
            continue;
        }
        if file.settings.value_of(key)? == stamped.value {
            continue;
        }
        match file.settings.with_setting(key, stamped.value.clone()) {
            Ok(settings) => {
                file.settings = settings;
                applied.settings.push(key.clone());
            }
            Err(error) => applied.warnings.push(format!(
                "setting {key}: the shared value from host {} is not applied: {error}",
                stamped.host
            )),
        }
    }

    applied.accounts = file
        .accounts
        .iter()
        .zip(&changed_accounts)
        .filter(|(_, changed)| **changed)
        .map(|(account, _)| account.abbr.clone())
        .collect();
    if applied.changed() {
        store.save(&file)?;
    }
    Ok(applied)
}

/// A shared price as the account stores it: the amount checked by
/// `validate_price`, its currency by the currency rule; a cleared price has no
/// currency, as an edit leaves it.
fn checked_price(price: &SharedPrice) -> Result<(Option<f64>, Option<String>), StoreError> {
    let amount = store::validate_price(price.amount)?;
    let currency = match (amount, &price.currency) {
        (Some(_), Some(currency)) => {
            store::validate_currency(currency)?;
            Some(currency.clone())
        }
        (Some(_), None) | (None, _) => None,
    };
    Ok((amount, currency))
}

/// The shared abbreviations, decided together: each must pass
/// `validate_abbr` against the abbreviations the other accounts will have,
/// so two accounts that swap theirs both apply, and one that would take
/// another's keeps its own with a warning. Returns the indexes it changed.
fn apply_abbrs(
    accounts: &mut [Account],
    shared: &Shared,
    warnings: &mut Vec<String>,
) -> Vec<usize> {
    // The wanted abbreviation per account, already in the stored form.
    let mut wanted: Vec<Option<(String, &str)>> = vec![None; accounts.len()];
    for (index, account) in accounts.iter().enumerate() {
        let Some(abbr) = shared
            .accounts
            .get(&AccountRef::of(account))
            .and_then(|look| look.abbr.as_ref())
        else {
            continue;
        };
        match store::validate_abbr(&abbr.value, &[]) {
            Ok(value) if value != account.abbr => wanted[index] = Some((value, &abbr.host)),
            Ok(_) => {}
            Err(error) => warnings.push(format!(
                "account {}: the shared abbreviation from host {} is not applied: {error}",
                name(account),
                abbr.host
            )),
        }
    }
    // Refuse one clash at a time: a refused account keeps its abbreviation,
    // which can clash with another wanted one, so check again until none does.
    loop {
        let finals: Vec<&str> = accounts
            .iter()
            .zip(&wanted)
            .map(|(account, wanted)| match wanted {
                Some((value, _)) => value.as_str(),
                None => account.abbr.as_str(),
            })
            .collect();
        let clash = wanted.iter().enumerate().find_map(|(index, wanted)| {
            let (value, host) = wanted.as_ref()?;
            let others: Vec<&str> = finals
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, abbr)| *abbr)
                .collect();
            store::validate_abbr(value, &others)
                .err()
                .map(|error| (index, host.to_string(), error))
        });
        let Some((index, host, error)) = clash else {
            break;
        };
        warnings.push(format!(
            "account {}: the shared abbreviation from host {host} is not applied: {error}",
            name(&accounts[index])
        ));
        wanted[index] = None;
    }
    let mut changed = Vec::new();
    for (index, wanted) in wanted.into_iter().enumerate() {
        if let Some((value, _)) = wanted {
            accounts[index].abbr = value;
            changed.push(index);
        }
    }
    changed
}

/// For each local account field and each shared setting that no host has an
/// event for yet, one event with this host's value, so the other hosts learn
/// it: one `accountLook` per account with its unshared fields, a
/// `priceChanged` for a stored price no host shared, and a `settingSet` per
/// unshared setting that differs from its default (a fresh host never
/// overrides a set-up one). An event that cannot be written is a warning.
pub fn seed(
    store: &Store,
    shared: &Shared,
    recorder: &Recorder,
    now: DateTime<Utc>,
) -> Result<Seeded, SharedError> {
    let file = store.load()?;
    let mut events = Vec::new();
    let none = SharedLook::default();
    for account in &file.accounts {
        let reference = AccountRef::of(account);
        let look = shared.accounts.get(&reference).unwrap_or(&none);
        let kind = EventKind::AccountLook {
            account: reference.clone(),
            label: look.label.is_none().then(|| account.label.clone()),
            abbr: look.abbr.is_none().then(|| account.abbr.clone()),
            color: look.color.is_none().then(|| account.color.clone()),
            auto_burn: look.auto_burn.is_none().then_some(account.auto_burn),
        };
        if !look_is_empty(&kind) {
            events.push(kind);
        }
        if look.price.is_none() && account.price.is_some() {
            events.push(EventKind::PriceChanged {
                account: reference,
                amount: account.price,
                currency: account.price_currency.clone(),
            });
        }
    }
    let defaults = Settings::default();
    for key in SHARED_SETTINGS {
        if shared.settings.contains_key(key) {
            continue;
        }
        let value = file.settings.value_of(key)?;
        if value != defaults.value_of(key)? {
            events.push(EventKind::SettingSet {
                key: key.to_string(),
                value,
            });
        }
    }
    let mut seeded = Seeded::default();
    for kind in events {
        let what = match &kind {
            EventKind::SettingSet { key, .. } => format!("the setting {key}"),
            _ => "an account's look".to_string(),
        };
        match recorder.event(now, kind) {
            Ok(()) => seeded.written += 1,
            Err(error) => seeded.warnings.push(format!(
                "{what} is not shared with the other hosts: {error}"
            )),
        }
    }
    Ok(seeded)
}

/// Read every host's events, [`seed`] what no host shared yet and [`apply`]
/// the newest values. The tray app runs it at start and every few minutes,
/// and every account add runs it so a login of a known account takes its
/// shared look and price at once. The result's warnings are the read's,
/// [`latest`]'s, [`seed`]'s and [`apply`]'s, in that order.
pub fn sync(
    store: &Store,
    recorder: &Recorder,
    now: DateTime<Utc>,
) -> Result<Applied, SharedError> {
    let read = recorder.dir().read::<Event>(&Range::ALL, &Hosts::All)?;
    let shared = latest(&read.records);
    let seeded = seed(store, &shared, recorder, now)?;
    let applied = apply(store, &shared)?;
    let mut warnings = read.warnings;
    warnings.extend(shared.warnings.iter().cloned());
    warnings.extend(seeded.warnings);
    warnings.extend(applied.warnings);
    Ok(Applied {
        warnings,
        ..applied
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;

    use crate::history::HistoryDir;
    use crate::paths::AppPaths;
    use crate::store::{AccountsFile, FileSecrets, LoginState};
    use crate::types::{Login, OAuthAccount};

    const ORG: &str = "e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62";
    const LB_UUID: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const AC_UUID: &str = "9c3d5e7f-1a2b-4c6d-8e0f-2b4d6f8a0c1e";
    const PR_UUID: &str = "7a1b3c5d-9e0f-4a2b-8c4d-6e8f0a2b4c6d";

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a test time")
            .with_timezone(&Utc)
    }

    fn reference(acc: &str) -> AccountRef {
        AccountRef {
            acc: acc.to_string(),
            org: ORG.to_string(),
        }
    }

    fn account(id: &str, acc: &str, label: &str, abbr: &str, color: &str) -> Account {
        Account {
            id: id.to_string(),
            account_uuid: acc.to_string(),
            organization_uuid: ORG.to_string(),
            label: label.to_string(),
            abbr: abbr.to_string(),
            color: color.to_string(),
            email: Some(format!("{}@example.com", abbr.to_ascii_lowercase())),
            org_name: Some("Lumen Bay".to_string()),
            plan: Some("Team 5x".to_string()),
            price: None,
            price_currency: None,
            oauth_account: OAuthAccount(serde_json::Map::new()),
            added_at: utc("2026-11-01T08:00:00Z"),
            last_used_at: None,
            login_state: LoginState::Ok,
            login_ends_at_ms: Some(1_796_772_840_000),
            auto_burn: true,
        }
    }

    fn lb() -> Account {
        account(
            "6f1c2a9be0d34c118f2d4e5a6b7c8d9e",
            LB_UUID,
            "Lumen Bay",
            "LB",
            "#3b82f6",
        )
    }

    fn ac() -> Account {
        account(
            "a92d4c1e8b3f4a5d9e6c7b8a1d2e3f40",
            AC_UUID,
            "Acme",
            "AC",
            "#f59e0b",
        )
    }

    /// A data folder with these accounts and settings, and a history folder
    /// beside it where this host is `atlas`.
    struct Fixture {
        _tmp: tempfile::TempDir,
        store: Store,
        root: PathBuf,
    }

    impl Fixture {
        fn new(accounts: Vec<Account>, settings: Settings) -> Fixture {
            let tmp = tempfile::tempdir().expect("tempdir");
            let app = AppPaths::at(&tmp.path().join("data"));
            let store = Store::new(app.clone(), Box::new(FileSecrets::new(app.secrets_dir())));
            store
                .save(&AccountsFile {
                    accounts,
                    settings,
                    ..AccountsFile::default()
                })
                .expect("accounts.json");
            let root = tmp.path().join("history");
            Fixture {
                _tmp: tmp,
                store,
                root,
            }
        }

        /// A host's history folder, with its marker so `Hosts::All` reads it.
        fn host(&self, host: &str) -> HistoryDir {
            let folder = self.root.join(host);
            fs::create_dir_all(&folder).expect("host folder");
            fs::write(folder.join("host.json"), r#"{"machineId":"made-up"}"#).expect("marker");
            HistoryDir::at(&self.root, host)
        }

        fn write(&self, host: &str, at: &str, kind: EventKind) {
            self.host(host)
                .append(&Event { t: utc(at), kind })
                .expect("the event is written");
        }

        fn events(&self) -> Vec<Hosted<Event>> {
            HistoryDir::at(&self.root, "atlas")
                .read::<Event>(&Range::ALL, &Hosts::All)
                .expect("the events are read")
                .records
        }

        fn atlas(&self) -> Recorder {
            Recorder::new(self.host("atlas"))
        }

        fn accounts(&self) -> AccountsFile {
            self.store.load().expect("accounts.json")
        }
    }

    fn look(acc: &str) -> EventKind {
        EventKind::AccountLook {
            account: reference(acc),
            label: None,
            abbr: None,
            color: None,
            auto_burn: None,
        }
    }

    fn with_color(acc: &str, color: &str) -> EventKind {
        match look(acc) {
            EventKind::AccountLook {
                account,
                label,
                abbr,
                auto_burn,
                ..
            } => EventKind::AccountLook {
                account,
                label,
                abbr,
                color: Some(color.to_string()),
                auto_burn,
            },
            other => other,
        }
    }

    fn with_label(acc: &str, label: &str) -> EventKind {
        EventKind::AccountLook {
            account: reference(acc),
            label: Some(label.to_string()),
            abbr: None,
            color: None,
            auto_burn: None,
        }
    }

    fn with_abbr(acc: &str, abbr: &str) -> EventKind {
        EventKind::AccountLook {
            account: reference(acc),
            label: None,
            abbr: Some(abbr.to_string()),
            color: None,
            auto_burn: None,
        }
    }

    fn setting(key: &str, value: serde_json::Value) -> EventKind {
        EventKind::SettingSet {
            key: key.to_string(),
            value,
        }
    }

    #[test]
    fn the_two_event_kinds_have_their_json_shape() {
        let kinds = [
            EventKind::AccountLook {
                account: reference(LB_UUID),
                label: Some("Lab".to_string()),
                abbr: None,
                color: None,
                auto_burn: Some(false),
            },
            setting("currency", json!("EUR")),
        ];
        let lines: Vec<serde_json::Value> = kinds
            .iter()
            .map(|kind| serde_json::to_value(kind).expect("JSON"))
            .collect();
        assert_eq!(
            lines[0],
            json!({"type": "accountLook", "account": {"acc": LB_UUID, "org": ORG},
                   "label": "Lab", "autoBurn": false})
        );
        assert_eq!(
            lines[1],
            json!({"type": "settingSet", "key": "currency", "value": "EUR"})
        );
        for (kind, line) in kinds.iter().zip(lines) {
            let back: EventKind = serde_json::from_value(line).expect("reads back");
            assert_eq!(&back, kind);
        }
        assert_eq!(kinds[0].accounts(), vec![&reference(LB_UUID)]);
        assert!(kinds[1].accounts().is_empty());
    }

    #[test]
    fn the_newest_account_look_wins_over_hosts() {
        let fixture = Fixture::new(vec![lb()], Settings::default());
        fixture.write(
            "vega",
            "2026-11-16T10:00:00Z",
            with_color(LB_UUID, "#10b981"),
        );
        fixture.write(
            "atlas",
            "2026-11-16T09:00:00Z",
            with_color(LB_UUID, "#ef4444"),
        );
        let shared = latest(&fixture.events());
        let color = shared.accounts[&reference(LB_UUID)]
            .color
            .clone()
            .expect("a shared colour");
        assert_eq!(
            (color.value.as_str(), color.host.as_str()),
            ("#10b981", "vega")
        );

        let applied = apply(&fixture.store, &shared).expect("applied");
        assert_eq!(applied.accounts, vec!["LB".to_string()]);
        assert_eq!(fixture.accounts().accounts[0].color, "#10b981");

        // The same time on two hosts: the later host name decides, whatever
        // order the events come in.
        let tie = [
            Hosted {
                host: "vega".to_string(),
                record: Event {
                    t: utc("2026-11-16T11:00:00Z"),
                    kind: with_color(LB_UUID, "#8b5cf6"),
                },
            },
            Hosted {
                host: "atlas".to_string(),
                record: Event {
                    t: utc("2026-11-16T11:00:00Z"),
                    kind: with_color(LB_UUID, "#ec4899"),
                },
            },
        ];
        for events in [tie.to_vec(), tie.iter().rev().cloned().collect()] {
            let color = latest(&events).accounts[&reference(LB_UUID)]
                .color
                .clone()
                .expect("a shared colour");
            assert_eq!(color.value, "#8b5cf6", "vega sorts after atlas");
        }
    }

    #[test]
    fn fields_of_one_account_are_decided_one_by_one() {
        let fixture = Fixture::new(vec![lb()], Settings::default());
        fixture.write(
            "atlas",
            "2026-11-16T09:00:00Z",
            with_color(LB_UUID, "#14b8a6"),
        );
        fixture.write("vega", "2026-11-16T10:00:00Z", with_label(LB_UUID, "Lab"));

        apply(&fixture.store, &latest(&fixture.events())).expect("applied");

        let account = &fixture.accounts().accounts[0];
        assert_eq!(
            account.color, "#14b8a6",
            "the later label does not undo the colour"
        );
        assert_eq!(account.label, "Lab");
        assert_eq!(account.abbr, "LB", "a field no host changed stays");
    }

    #[test]
    fn a_setting_set_on_another_host_is_applied() {
        let fixture = Fixture::new(vec![lb()], Settings::default());
        fixture.write(
            "vega",
            "2026-11-16T10:00:00Z",
            setting("currency", json!("EUR")),
        );
        fixture.write(
            "vega",
            "2026-11-16T10:01:00Z",
            setting("notifyWaitingHosts", json!("allHosts")),
        );

        let applied = apply(&fixture.store, &latest(&fixture.events())).expect("applied");

        assert_eq!(
            applied.settings,
            vec!["currency".to_string(), "notifyWaitingHosts".to_string()]
        );
        let settings = fixture.accounts().settings;
        assert_eq!(settings.currency.as_deref(), Some("EUR"));
        assert_eq!(
            settings.notify_waiting_hosts,
            crate::store::WaitingHosts::AllHosts
        );
        assert!(applied.warnings.is_empty(), "{:?}", applied.warnings);
    }

    #[test]
    fn an_invalid_shared_value_is_a_warning_not_applied() {
        let fixture = Fixture::new(vec![lb()], Settings::default());
        fixture.write("vega", "2026-11-16T10:00:00Z", with_color(LB_UUID, "blue"));
        fixture.write("vega", "2026-11-16T10:00:00Z", with_label(LB_UUID, "   "));
        fixture.write(
            "vega",
            "2026-11-16T10:01:00Z",
            setting("currency", json!("eur")),
        );
        fixture.write(
            "vega",
            "2026-11-16T10:02:00Z",
            EventKind::PriceChanged {
                account: reference(LB_UUID),
                amount: Some(-5.0),
                currency: Some("EUR".to_string()),
            },
        );
        let before = fs::read(fixture.store.app_paths().accounts_file()).expect("the file");

        let applied = apply(&fixture.store, &latest(&fixture.events())).expect("applied");

        assert!(!applied.changed());
        assert_eq!(
            fs::read(fixture.store.app_paths().accounts_file()).expect("the file"),
            before,
            "nothing is written"
        );
        assert_eq!(applied.warnings.len(), 4, "{:?}", applied.warnings);
        for (warning, field) in applied
            .warnings
            .iter()
            .zip(["label", "colour", "price", "currency"])
        {
            assert!(warning.contains(field), "{warning}");
            assert!(warning.contains("host vega"), "{warning}");
        }
        assert!(applied.warnings[0].contains("account LB (Lumen Bay)"));
        assert!(applied.warnings[3].starts_with("setting currency:"));
    }

    #[test]
    fn an_abbreviation_taken_locally_is_a_warning() {
        let fixture = Fixture::new(vec![lb(), ac()], Settings::default());
        fixture.write("vega", "2026-11-16T10:00:00Z", with_abbr(LB_UUID, "ac"));

        let applied = apply(&fixture.store, &latest(&fixture.events())).expect("applied");

        assert!(!applied.changed());
        assert_eq!(applied.warnings.len(), 1, "{:?}", applied.warnings);
        assert!(
            applied.warnings[0].contains("account LB (Lumen Bay)")
                && applied.warnings[0].contains("host vega")
                && applied.warnings[0].contains("already used"),
            "{}",
            applied.warnings[0]
        );
        assert_eq!(fixture.accounts().accounts[0].abbr, "LB");

        // Two accounts that swap theirs on another host both apply.
        fixture.write("vega", "2026-11-16T10:05:00Z", with_abbr(AC_UUID, "LB"));
        let applied = apply(&fixture.store, &latest(&fixture.events())).expect("applied");
        assert!(applied.warnings.is_empty(), "{:?}", applied.warnings);
        let abbrs: Vec<String> = fixture
            .accounts()
            .accounts
            .iter()
            .map(|account| account.abbr.clone())
            .collect();
        assert_eq!(abbrs, vec!["AC".to_string(), "LB".to_string()]);
    }

    #[test]
    fn applying_writes_no_event() {
        let fixture = Fixture::new(vec![lb()], Settings::default());
        fixture.write("vega", "2026-11-16T10:00:00Z", with_label(LB_UUID, "Lab"));
        fixture.write(
            "vega",
            "2026-11-16T10:01:00Z",
            setting("currency", json!("EUR")),
        );
        let before = fixture.events().len();

        let applied = apply(&fixture.store, &latest(&fixture.events())).expect("applied");

        assert!(applied.changed());
        assert_eq!(fixture.events().len(), before);
        assert!(
            !fixture.root.join("atlas").exists(),
            "this host wrote nothing"
        );
    }

    #[test]
    fn seeding_skips_defaults_and_keys_already_shared() {
        let mut priced = lb();
        priced.price = Some(180.0);
        priced.price_currency = Some("EUR".to_string());
        let settings = Settings {
            currency: Some("EUR".to_string()),
            statusline_feed: false,
            // Not shared at all, whatever its value.
            hotkey: "Ctrl+Alt+S".to_string(),
            ..Settings::default()
        };
        let fixture = Fixture::new(vec![priced], settings);
        fixture.write("vega", "2026-11-16T10:00:00Z", with_label(LB_UUID, "Lab"));
        fixture.write(
            "vega",
            "2026-11-16T10:00:00Z",
            setting("statuslineFeed", json!(true)),
        );
        let shared = latest(&fixture.events());

        let seeded = seed(
            &fixture.store,
            &shared,
            &fixture.atlas(),
            utc("2026-11-16T12:00:00Z"),
        )
        .expect("seeded");

        assert_eq!(seeded.written, 3, "{seeded:?}");
        let written: Vec<EventKind> = HistoryDir::at(&fixture.root, "atlas")
            .read::<Event>(&Range::ALL, &Hosts::One("atlas".to_string()))
            .expect("read")
            .records
            .into_iter()
            .map(|hosted| hosted.record.kind)
            .collect();
        assert_eq!(
            written,
            vec![
                EventKind::AccountLook {
                    account: reference(LB_UUID),
                    label: None,
                    abbr: Some("LB".to_string()),
                    color: Some("#3b82f6".to_string()),
                    auto_burn: Some(true),
                },
                EventKind::PriceChanged {
                    account: reference(LB_UUID),
                    amount: Some(180.0),
                    currency: Some("EUR".to_string()),
                },
                setting("currency", json!("EUR")),
            ]
        );

        // Seeding again finds everything shared and writes nothing.
        let again = seed(
            &fixture.store,
            &latest(&fixture.events()),
            &fixture.atlas(),
            utc("2026-11-16T12:05:00Z"),
        )
        .expect("seeded");
        assert_eq!(again.written, 0);
    }

    #[test]
    fn a_new_account_takes_its_shared_look_and_price() {
        let fixture = Fixture::new(vec![lb()], Settings::default());
        fixture.write(
            "vega",
            "2026-11-15T10:00:00Z",
            EventKind::AccountLook {
                account: reference(PR_UUID),
                label: Some("Private".to_string()),
                abbr: Some("PR".to_string()),
                color: Some("#ec4899".to_string()),
                auto_burn: Some(false),
            },
        );
        fixture.write(
            "vega",
            "2026-11-15T10:01:00Z",
            EventKind::PriceChanged {
                account: reference(PR_UUID),
                amount: Some(20.0),
                currency: Some("EUR".to_string()),
            },
        );
        // The login of that account on this host: a new account, with the
        // label, abbreviation and colour `new_account` picks.
        let login: Login = serde_json::from_value(json!({
            "claudeAiOauth": {
                "accessToken": format!("sk-ant-oat01-{}", "p".repeat(95)),
                "refreshToken": format!("sk-ant-ort01-{}", "q".repeat(95)),
                "expiresAt": 1_794_356_040_000_i64,
                "refreshTokenExpiresAt": 1_796_772_840_000_i64,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "pro"
            },
            "organizationUuid": ORG
        }))
        .expect("a login");
        let identity: OAuthAccount = serde_json::from_value(json!({
            "accountUuid": PR_UUID,
            "emailAddress": "pr@example.com",
            "organizationUuid": ORG,
            "organizationName": "Prism Works"
        }))
        .expect("an identity");
        let mut file = fixture.accounts();
        let added = store::new_account(&login, &identity, None, &file, utc("2026-11-16T12:00:00Z"))
            .expect("a new account");
        assert_eq!(added.label, "Prism Works");
        file.accounts.push(added);
        fixture.store.save(&file).expect("saved");

        let applied = sync(
            &fixture.store,
            &fixture.atlas(),
            utc("2026-11-16T12:00:01Z"),
        )
        .expect("synced");

        assert!(applied.warnings.is_empty(), "{:?}", applied.warnings);
        let file = fixture.accounts();
        let account = file
            .find_by_identity(PR_UUID, ORG)
            .expect("the new account");
        assert_eq!(account.label, "Private");
        assert_eq!(account.abbr, "PR");
        assert_eq!(account.color, "#ec4899");
        assert!(!account.auto_burn);
        assert_eq!(account.price, Some(20.0));
        assert_eq!(account.price_currency.as_deref(), Some("EUR"));
        assert_eq!(applied.accounts, vec!["PR".to_string()]);
    }

    #[test]
    fn a_setting_outside_the_shared_list_is_ignored_with_a_warning() {
        let fixture = Fixture::new(vec![lb()], Settings::default());
        fixture.write(
            "vega",
            "2026-11-16T10:00:00Z",
            setting("hotkey", json!("Ctrl+Alt+V")),
        );

        let shared = latest(&fixture.events());

        assert!(shared.settings.is_empty());
        assert_eq!(shared.warnings.len(), 1);
        assert!(
            shared.warnings[0].contains("hotkey") && shared.warnings[0].contains("host vega"),
            "{}",
            shared.warnings[0]
        );
        let applied = apply(&fixture.store, &shared).expect("applied");
        assert!(!applied.changed());
        assert_eq!(
            fixture.accounts().settings.hotkey,
            Settings::default().hotkey
        );
    }

    #[test]
    fn an_old_history_without_these_events_changes_nothing() {
        let fixture = Fixture::new(vec![lb(), ac()], Settings::default());
        fixture.write(
            "vega",
            "2026-11-10T10:00:00Z",
            EventKind::Switch {
                from: Some(reference(LB_UUID)),
                to: reference(AC_UUID),
                by: crate::history::By::User,
                reason: None,
                cwd: None,
                sessions: None,
                waited_secs: None,
            },
        );
        fixture.write(
            "atlas",
            "2026-11-10T11:00:00Z",
            EventKind::PlanChanged {
                account: reference(LB_UUID),
                from: Some("Team".to_string()),
                to: Some("Team 5x".to_string()),
            },
        );
        let before = fs::read(fixture.store.app_paths().accounts_file()).expect("the file");

        let shared = latest(&fixture.events());
        assert_eq!(shared, Shared::default());
        let applied = apply(&fixture.store, &shared).expect("applied");

        assert_eq!(applied, Applied::default());
        assert_eq!(
            fs::read(fixture.store.app_paths().accounts_file()).expect("the file"),
            before
        );
    }

    #[test]
    fn edit_events_name_only_what_changed() {
        let before = lb();
        let mut after = before.clone();
        after.label = "Lab".to_string();
        let edited = Edited {
            before: before.clone(),
            account: after.clone(),
        };
        assert_eq!(edit_events(&edited), vec![with_label(LB_UUID, "Lab")]);

        let mut priced = before.clone();
        priced.price = Some(180.0);
        priced.price_currency = Some("EUR".to_string());
        let edited = Edited {
            before,
            account: priced,
        };
        assert_eq!(
            edit_events(&edited),
            vec![EventKind::PriceChanged {
                account: reference(LB_UUID),
                amount: Some(180.0),
                currency: Some("EUR".to_string()),
            }],
            "a price edit alone writes no accountLook"
        );
    }

    #[test]
    fn setting_events_are_written_for_shared_keys_only() {
        let previous = Settings::default();
        let next = Settings {
            currency: Some("EUR".to_string()),
            hotkey: "Ctrl+Alt+S".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            setting_events(&previous, &next).expect("events"),
            vec![setting("currency", json!("EUR"))]
        );
        assert!(setting_events(&next, &next).expect("events").is_empty());
    }

    /// The status line is shared as one history line; the built-in layouts
    /// fit, so only a very long hand-made one can be refused as too long.
    #[test]
    fn the_built_in_status_lines_fit_one_history_line() {
        let presets = [
            crate::store::StatuslineConfig::current_line(),
            crate::store::StatuslineConfig::limits_only(),
            crate::store::StatuslineConfig::everything(),
        ];
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = HistoryDir::at(tmp.path(), "atlas");
        for preset in presets {
            let value = serde_json::to_value(&preset).expect("JSON");
            dir.append(&Event {
                t: utc("2026-11-16T10:00:00Z"),
                kind: setting("statusline", value),
            })
            .expect("one line");
        }
    }
}
