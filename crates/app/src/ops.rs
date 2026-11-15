//! Actions shared by the CLI and the GUI (docs/implementation.md "app: CLI",
//! "app: GUI"). One place for the decisions both front ends must take the same
//! way.

use chrono::{DateTime, Utc};

use seatswap_core::history::{
    AccountRef, By, EventKind, HistoryDir, HistoryError, Recorder, SessionCounts,
};
use seatswap_core::paths::{AppPaths, ClaudePaths};
#[cfg(feature = "gui")]
use seatswap_core::store::AccountsFile;
use seatswap_core::store::{Account, Settings, Store};
use seatswap_core::switcher::{self, LiveState, Synced};

/// The one place a switch is written to the history, for the CLI and the GUI.
/// A switch that saved the unsaved login writes that account's `accountAdded`
/// first, so a reader never sees a switch to an account it does not know yet.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_switch(
    recorder: &Recorder,
    from: Option<AccountRef>,
    to: AccountRef,
    saved_new: Option<&Account>,
    by: By,
    reason: Option<String>,
    cwd: Option<String>,
    sessions: Option<SessionCounts>,
    at: DateTime<Utc>,
) -> Result<(), HistoryError> {
    // Try both lines even when the first fails, and return the first error: a
    // failed `accountAdded` must not lose the switch.
    let mut first_error = None;
    if let Some(account) = saved_new {
        if let Err(error) = recorder.event(
            at,
            EventKind::AccountAdded {
                account: AccountRef::of(account),
                plan: account.plan.clone(),
            },
        ) {
            first_error = Some(error);
        }
    }
    if let Err(error) = recorder.event(
        at,
        EventKind::Switch {
            from,
            to,
            by,
            reason,
            cwd,
            sessions,
            waited_secs: None,
        },
    ) {
        if first_error.is_none() {
            return Err(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The one place an account is removed, for the CLI and the GUI. The remove
/// happens first; a history that cannot be written is returned beside the
/// removed account and never fails the remove.
pub(crate) fn remove_account(
    store: &Store,
    key: &str,
    recorder: Option<&Recorder>,
    now: DateTime<Utc>,
) -> Result<(Account, Option<String>), String> {
    let removed = switcher::remove_account(store, key).map_err(|error| error.to_string())?;
    let history_error = recorder.and_then(|recorder| {
        recorder
            .event(
                now,
                EventKind::AccountRemoved {
                    account: AccountRef::of(&removed),
                },
            )
            .err()
            .map(|error| history_unwritable(&error))
    });
    Ok((removed, history_error))
}

/// The one place a plan change is written to the history, for the switch, the
/// live sync and the mirror. The switcher reports the change; every path that
/// records its events writes it here.
pub(crate) fn record_plan_change(
    recorder: &Recorder,
    change: &switcher::PlanChange,
    at: DateTime<Utc>,
) -> Result<(), HistoryError> {
    recorder.event(at, change.event())
}

/// The one sentence the app uses when the history cannot be written.
pub(crate) fn history_unwritable(error: &dyn std::fmt::Display) -> String {
    format!("the history cannot be written: {error}")
}

/// Open the history folder with the same rule everywhere. The text is the one
/// every front end shows when the history cannot be written.
pub(crate) fn open_history(app: &AppPaths, settings: &Settings) -> Result<Recorder, String> {
    HistoryDir::open(app, settings)
        .map(Recorder::new)
        .map_err(|error| history_unwritable(&error))
}

/// The account the live state names, when it is a stored account.
#[cfg(feature = "gui")]
pub(crate) fn live_account_ref(live: &LiveState, file: &AccountsFile) -> Option<AccountRef> {
    let account_id = match live {
        LiveState::Known { account_id } => account_id,
        LiveState::Ended {
            account_id: Some(account_id),
        } => account_id,
        LiveState::Ended { account_id: None } | LiveState::NoLogin | LiveState::Unknown { .. } => {
            return None
        }
    };
    file.find(account_id).map(AccountRef::of)
}

/// Save the live login as an account (or update the stored copy). A login that
/// is missing, ended or still unknown saved nothing and is an error here, so
/// neither front end can report a success that did not happen.
pub(crate) fn save_live_login(
    claude: &ClaudePaths,
    store: &Store,
    recorder: Option<&Recorder>,
    label: Option<String>,
    now: DateTime<Utc>,
) -> Result<(Account, Option<String>), String> {
    let (state, synced) =
        switcher::sync_live(claude, store, Some(label), now).map_err(|error| error.to_string())?;
    match state {
        LiveState::Known { account_id } => {
            let file = store.load().map_err(|error| error.to_string())?;
            let account = file
                .find(&account_id)
                .cloned()
                .ok_or_else(|| format!("the store has no account {account_id}"))?;
            // Only a newly created account gets `accountAdded`; saving the
            // stored copy again is an update, and a plan change is written as
            // `PlanChanged` after it.
            let history_error = match &synced {
                Synced::Added => recorder.and_then(|recorder| {
                    recorder
                        .event(
                            now,
                            EventKind::AccountAdded {
                                account: AccountRef::of(&account),
                                plan: account.plan.clone(),
                            },
                        )
                        .err()
                        .map(|error| history_unwritable(&error))
                }),
                Synced::Updated {
                    plan_change: Some(change),
                } => recorder.and_then(|recorder| {
                    record_plan_change(recorder, change, now)
                        .err()
                        .map(|error| history_unwritable(&error))
                }),
                Synced::Unchanged | Synced::Updated { plan_change: None } => None,
            };
            Ok((account, history_error))
        }
        LiveState::Ended { .. } => {
            Err("the live login has ended; log in again in Claude Code".to_string())
        }
        LiveState::NoLogin => {
            Err("no live login to save; run Claude Code once and log in".to_string())
        }
        LiveState::Unknown { .. } => {
            Err("the live login could not be saved as an account".to_string())
        }
    }
}

/// The one sentence the CLI and the GUI print after a switch, e.g.
/// "Switched to AC — Acme". A store that cannot be read, or that has no
/// such account, is an error: the raw id is never shown as if it were a name.
pub(crate) fn switch_message(store: &Store, account_id: &str) -> Result<String, String> {
    let file = store.load().map_err(|error| error.to_string())?;
    let account = file
        .find(account_id)
        .ok_or_else(|| format!("the store has no account {account_id}"))?;
    Ok(format!("Switched to {} — {}", account.abbr, account.label))
}

/// The sentence for a switch that happened but whose account list could not
/// name it. Used by the CLI and the GUI so both say the same thing.
pub(crate) fn switched_without_list(error: &str) -> String {
    format!("Switched, but the account list could not be read: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use chrono::Utc;
    use seatswap_core::paths::{AppPaths, ClaudePaths};
    use seatswap_core::store::{FileSecrets, Store};

    const ACCOUNT_UUID: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const ORG_UUID: &str = "e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62";
    const EMAIL: &str = "someone@example.com";

    struct Fixture {
        _tmp: tempfile::TempDir,
        claude: ClaudePaths,
        app: AppPaths,
    }

    fn credentials_json(refresh_token: &str) -> serde_json::Value {
        serde_json::json!({
            "mcpOAuth": {
                "plugin:engineering:linear|5f1c2a9be0d34c11": {
                    "serverName": "plugin:engineering:linear",
                    "accessToken": format!("lin_oauth_{}", "e".repeat(64)),
                    "expiresAt": 1_794_400_000_000_i64
                }
            },
            "claudeAiOauth": {
                "accessToken": format!("sk-ant-oat01-{}", "a".repeat(95)),
                "refreshToken": refresh_token,
                "expiresAt": 1_794_356_040_000_i64,
                "refreshTokenExpiresAt": 1_796_772_840_000_i64,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "team",
                "subscriptionTypeSource": "profile",
                "rateLimitTier": "default_claude_max_5x"
            },
            "organizationUuid": ORG_UUID
        })
    }

    fn identity_json() -> serde_json::Value {
        serde_json::json!({
            "numStartups": 41,
            "oauthAccount": {
                "accountUuid": ACCOUNT_UUID,
                "emailAddress": EMAIL,
                "organizationUuid": ORG_UUID,
                "organizationName": "Acme",
                "organizationRole": "admin",
                "billingType": "stripe_subscription",
                "hasExtraUsageEnabled": false
            },
            "projects": {}
        })
    }

    impl Fixture {
        fn new() -> Fixture {
            let tmp = tempfile::tempdir().expect("tempdir");
            let config = tmp.path().join("claude");
            fs::create_dir_all(&config).expect("config folder");
            let claude = ClaudePaths::for_config_dir(&config, tmp.path());
            let app = AppPaths::at(&tmp.path().join("data"));
            fs::create_dir_all(&app.data_dir).expect("data folder");
            fs::write(
                &claude.credentials_file,
                credentials_json(&format!("sk-ant-ort01-{}", "b".repeat(95))).to_string(),
            )
            .expect("credentials");
            fs::write(&claude.identity_file, identity_json().to_string()).expect("identity");
            Fixture {
                _tmp: tmp,
                claude,
                app,
            }
        }

        fn store(&self) -> Store {
            Store::new(
                self.app.clone(),
                Box::new(FileSecrets::new(self.app.secrets_dir())),
            )
        }
    }

    #[test]
    fn save_live_login_returns_the_stored_account() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (account, history_error) = save_live_login(
            &fixture.claude,
            &store,
            None,
            Some("Acme".to_string()),
            Utc::now(),
        )
        .expect("the live login is saved");
        assert_eq!(history_error, None);
        assert_eq!(account.abbr, "AC");
        assert_eq!(account.label, "Acme");
        assert_eq!(account.email.as_deref(), Some(EMAIL));
        let file = store.load().expect("accounts");
        assert_eq!(file.accounts.len(), 1);
    }

    #[test]
    fn save_live_login_without_a_live_login_is_an_error() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.claude.credentials_file).expect("remove credentials");
        let store = fixture.store();
        let error = save_live_login(&fixture.claude, &store, None, None, Utc::now())
            .expect_err("nothing to save");
        assert!(error.contains("no live login"), "{error}");
    }

    #[test]
    fn save_live_login_with_an_ended_login_is_an_error() {
        let fixture = Fixture::new();
        fs::write(
            &fixture.claude.credentials_file,
            credentials_json("").to_string(),
        )
        .expect("ended credentials");
        let store = fixture.store();
        let error = save_live_login(&fixture.claude, &store, None, None, Utc::now())
            .expect_err("the login ended");
        assert!(error.contains("ended"), "{error}");
    }

    #[test]
    fn a_switch_message_never_uses_a_raw_id() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (account, _) = save_live_login(&fixture.claude, &store, None, None, Utc::now())
            .expect("the live login is saved");
        assert_eq!(
            switch_message(&store, &account.id),
            Ok("Switched to AC — Acme".to_string())
        );

        let error = switch_message(&store, "not-an-account").expect_err("no such account");
        assert_eq!(
            switched_without_list(&error),
            "Switched, but the account list could not be read: the store has no account not-an-account"
        );

        // A store that cannot be read is the same honest sentence.
        fs::write(fixture.app.accounts_file(), b"{ not json").expect("corrupt accounts");
        let error = switch_message(&store, &account.id).expect_err("accounts.json is corrupt");
        let message = switched_without_list(&error);
        assert!(
            message.starts_with("Switched, but the account list could not be read:"),
            "{message}"
        );
        assert!(
            !message.contains(&account.id),
            "the raw id must not be a name: {message}"
        );
    }

    #[test]
    fn the_switched_sentence_has_one_spelling() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (account, _) = save_live_login(&fixture.claude, &store, None, None, Utc::now())
            .expect("the live login is saved");
        assert_eq!(
            switch_message(&store, &account.id),
            Ok("Switched to AC — Acme".to_string())
        );
    }

    fn event_values(root: &std::path::Path) -> Vec<serde_json::Value> {
        let folder = root.join("atlas");
        let mut names: Vec<std::path::PathBuf> = fs::read_dir(&folder)
            .expect("the history folder")
            .map(|entry| entry.expect("a history entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("events-") && name.ends_with(".jsonl"))
            })
            .collect();
        names.sort();
        names
            .into_iter()
            .flat_map(|path| {
                fs::read_to_string(path)
                    .expect("the events file")
                    .lines()
                    .map(|line| serde_json::from_str(line).expect("an event line"))
                    .collect::<Vec<serde_json::Value>>()
            })
            .collect()
    }

    #[test]
    fn save_live_login_records_a_new_account_with_its_plan() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let history = tempfile::tempdir().expect("history folder");
        let recorder = Recorder::new(HistoryDir::at(history.path(), "atlas"));

        let (account, history_error) = save_live_login(
            &fixture.claude,
            &store,
            Some(&recorder),
            Some("Acme".to_string()),
            Utc::now(),
        )
        .expect("the live login is saved");
        assert_eq!(history_error, None);

        let events = event_values(history.path());
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["type"], "accountAdded");
        assert_eq!(events[0]["account"]["acc"], ACCOUNT_UUID);
        assert_eq!(events[0]["account"]["org"], ORG_UUID);
        assert_eq!(events[0]["plan"], "Team 5x");

        // Saving the same login again updates the stored copy, not a new account.
        save_live_login(&fixture.claude, &store, Some(&recorder), None, Utc::now())
            .expect("the stored copy is updated");
        assert_eq!(event_values(history.path()).len(), 1);
        assert_eq!(store.load().expect("accounts").accounts[0].id, account.id);
    }

    /// The live login is stored again and the plan of the account changed
    /// (the login says "Team 5x", the stored plan says "Team"): the change
    /// belongs in the history, written by the code that records the events of
    /// this path.
    #[test]
    fn a_changed_plan_is_written_to_the_history_when_the_live_login_is_saved_again() {
        let fixture = Fixture::new();
        let store = fixture.store();
        save_live_login(
            &fixture.claude,
            &store,
            None,
            Some("Acme".to_string()),
            Utc::now(),
        )
        .expect("the live login is saved");
        // The stored plan is stale; the live login's tier says "Team 5x".
        let mut file = store.load().expect("accounts");
        file.accounts[0].plan = Some("Team".to_string());
        store.save(&file).expect("the stale plan is stored");

        let history = tempfile::tempdir().expect("history folder");
        let recorder = Recorder::new(HistoryDir::at(history.path(), "atlas"));
        save_live_login(&fixture.claude, &store, Some(&recorder), None, Utc::now())
            .expect("the stored copy is updated");
        assert_eq!(
            store.load().expect("accounts").accounts[0].plan.as_deref(),
            Some("Team 5x"),
            "the plan of the account is the login's"
        );

        let events = if history.path().join("atlas").exists() {
            event_values(history.path())
        } else {
            Vec::new()
        };
        let changed: Vec<&serde_json::Value> = events
            .iter()
            .filter(|event| event["type"] == "planChanged")
            .collect();
        assert_eq!(
            changed.len(),
            1,
            "one event for one plan change: {events:?}"
        );
        assert_eq!(changed[0]["account"]["acc"], ACCOUNT_UUID);
        assert_eq!(changed[0]["account"]["org"], ORG_UUID);
        assert_eq!(changed[0]["from"], "Team");
        assert_eq!(changed[0]["to"], "Team 5x");
    }

    #[test]
    fn remove_account_records_the_removed_account() {
        let fixture = Fixture::new();
        let store = fixture.store();
        save_live_login(
            &fixture.claude,
            &store,
            None,
            Some("Acme".to_string()),
            Utc::now(),
        )
        .expect("the live login is saved");
        let history = tempfile::tempdir().expect("history folder");
        let recorder = Recorder::new(HistoryDir::at(history.path(), "atlas"));

        let (removed, history_error) = remove_account(&store, "AC", Some(&recorder), Utc::now())
            .expect("the account is removed");

        assert_eq!(removed.label, "Acme");
        assert_eq!(history_error, None);
        let events = event_values(history.path());
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["type"], "accountRemoved");
        assert_eq!(events[0]["account"]["acc"], ACCOUNT_UUID);
        assert_eq!(events[0]["account"]["org"], ORG_UUID);
        assert!(store.load().expect("accounts").accounts.is_empty());
    }

    #[test]
    fn a_saved_new_account_is_written_before_its_switch() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (account, _) = save_live_login(
            &fixture.claude,
            &store,
            None,
            Some("Acme".to_string()),
            Utc::now(),
        )
        .expect("the live login is saved");
        let history = tempfile::tempdir().expect("history folder");
        let recorder = Recorder::new(HistoryDir::at(history.path(), "atlas"));
        let at = Utc::now();

        record_switch(
            &recorder,
            None,
            AccountRef::of(&account),
            Some(&account),
            By::User,
            None,
            None,
            None,
            at,
        )
        .expect("the switch is written");

        let events = event_values(history.path());
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0]["type"], "accountAdded");
        assert_eq!(events[0]["account"]["acc"], ACCOUNT_UUID);
        assert_eq!(events[0]["plan"], "Team 5x");
        assert_eq!(events[1]["type"], "switch");
    }

    #[test]
    fn record_switch_writes_one_event() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let history = HistoryDir::at(tmp.path(), "atlas");
        let recorder = Recorder::new(seatswap_core::history::HistoryDir::at(
            history.root(),
            "atlas",
        ));
        let to = AccountRef {
            acc: ACCOUNT_UUID.to_string(),
            org: ORG_UUID.to_string(),
        };
        let at = Utc::now();
        record_switch(
            &recorder,
            None,
            to,
            None,
            By::User,
            None,
            None,
            Some(SessionCounts {
                running: 2,
                busy: 1,
                tied: None,
            }),
            at,
        )
        .expect("the switch is written");

        let file = tmp
            .path()
            .join("atlas")
            .join(format!("events-{}.jsonl", at.format("%Y-%m")));
        let text = fs::read_to_string(&file).expect("the events file");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let value: serde_json::Value = serde_json::from_str(lines[0]).expect("JSON");
        assert_eq!(value["type"], "switch");
        assert_eq!(value["by"], "user");
        assert_eq!(value["to"]["acc"], ACCOUNT_UUID);
    }

    /// A failed `accountAdded` write must not lose the switch line: both lines
    /// are tried, and the first error is returned.
    #[test]
    fn a_failed_account_added_write_still_writes_the_switch() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (mut account, _) = save_live_login(
            &fixture.claude,
            &store,
            None,
            Some("Acme".to_string()),
            Utc::now(),
        )
        .expect("the live login is saved");
        // A plan long enough to make the accountAdded line too long for the
        // history, while the switch line stays short.
        account.plan = Some("p".repeat(5000));
        let history = tempfile::tempdir().expect("history folder");
        let recorder = Recorder::new(HistoryDir::at(history.path(), "atlas"));

        let error = record_switch(
            &recorder,
            None,
            AccountRef::of(&account),
            Some(&account),
            By::User,
            None,
            None,
            None,
            Utc::now(),
        )
        .expect_err("the accountAdded line is too long");
        assert!(
            matches!(error, HistoryError::LineTooLong { .. }),
            "{error:?}"
        );

        let events = event_values(history.path());
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["type"], "switch");
    }
}
