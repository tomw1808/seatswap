//! Actions shared by the CLI and the GUI (docs/implementation.md "app: CLI",
//! "app: GUI"). One place for the decisions both front ends must take the same
//! way.

use chrono::{DateTime, Utc};

use seatswap_core::history::{
    self, AccountRef, By, EventKind, HistoryDir, HistoryError, Recorder, SessionCounts,
};
use seatswap_core::paths::{AppPaths, ClaudePaths, CodexPaths};
use seatswap_core::providers::{self, CODEX_ORG};
use seatswap_core::shared;
#[cfg(feature = "gui")]
use seatswap_core::store::AccountsFile;
use seatswap_core::store::{Account, Settings, Store};
use seatswap_core::switcher::{self, LiveState, Synced};

use crate::gui::ipc::{self, IpcError};

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

/// The one place a Codex login switch is written to the history, for the GUI's
/// switch action and for the poll that sees Codex's login changed outside
/// seatswap (R8, spec §5). The caller decides whether the live login changed;
/// this writes the one event.
pub(crate) fn record_provider_switch(
    recorder: &Recorder,
    from: Option<AccountRef>,
    to: AccountRef,
    at: DateTime<Utc>,
) -> Result<(), HistoryError> {
    recorder.event(
        at,
        EventKind::ProviderSwitch {
            from,
            to,
            by: By::User,
        },
    )
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

/// The one place a provider reading's account gets its subscription, for the
/// provider thread and for a Codex switch (R8): `switcher` creates it (and
/// keeps its plan current under the app lock), then the history gets
/// `accountAdded` when it was created and `planChanged` when the provider named
/// another plan. Every history failure is one sentence in the returned
/// warnings; the caller reports them the one way it reports provider errors.
pub(crate) fn ensure_provider_subscription(
    app: &AppPaths,
    recorder: Option<&Recorder>,
    account: &AccountRef,
    plan: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<String> {
    let ensured = match switcher::ensure_provider_subscription(app, account, plan) {
        Ok(ensured) => ensured,
        Err(error) => {
            let label = seatswap_core::money::Company::of_account(account).name();
            return vec![format!("the {label} subscription is not saved: {error}")];
        }
    };
    let mut warnings = Vec::new();
    if let Some(recorder) = recorder {
        if ensured.created {
            if let Err(error) = recorder.event(
                now,
                EventKind::AccountAdded {
                    account: account.clone(),
                    plan: plan.map(str::to_string),
                },
            ) {
                warnings.push(history_unwritable(&error));
            }
        }
        if let Some(change) = &ensured.plan_change {
            if let Err(error) = record_plan_change(recorder, change, now) {
                warnings.push(history_unwritable(&error));
            }
        }
    }
    warnings
}

/// The one place a Codex login switch is decided, for the CLI and the GUI
/// (R8): the switch itself through `codex::switch_to`, then one
/// [`EventKind::ProviderSwitch`] when the live login changed. A history write
/// or a subscription write that fails is one sentence in `warnings`, reported
/// beside the note and never failing the switch. An account that is not an
/// OpenAI subscription is an error naming it.
///
/// The note is the sentence the person reads, and it is empty when the target
/// already is the live login: the switch then does nothing and says nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSwitch {
    pub note: String,
    pub warnings: Vec<String>,
}

pub(crate) fn switch_codex(
    app: &AppPaths,
    codex_paths: &CodexPaths,
    store: &Store,
    recorder: Option<&Recorder>,
    to: &AccountRef,
    now: DateTime<Utc>,
) -> Result<CodexSwitch, String> {
    let file = store.load().map_err(|error| error.to_string())?;
    let to_subscription = file
        .provider_subscription(to)
        .filter(|subscription| subscription.account.org == CODEX_ORG)
        .ok_or_else(|| format!("{} is not a saved Codex login", to.acc))?;
    let to_abbr = to_subscription.abbr.clone();

    let switched = providers::codex::switch_to(app, codex_paths, store.secrets(), to)
        .map_err(|error| error.to_string())?;

    // The switch stored the live login; make sure its subscription is there
    // too (R8: a new account id gets one), so the note can name it. The one
    // function writes its `accountAdded`, beside the switch event.
    let mut warnings = match &switched.from {
        Some(from) => ensure_provider_subscription(app, recorder, from, None, now),
        None => Vec::new(),
    };
    let file = store.load().map_err(|error| error.to_string())?;

    let changed = switched.from.as_ref() != Some(to);
    if changed {
        if let Some(recorder) = recorder {
            if let Err(error) =
                record_provider_switch(recorder, switched.from.clone(), switched.to.clone(), now)
            {
                warnings.push(history_unwritable(&error));
            }
        }
    }

    let note = if !changed {
        String::new()
    } else {
        let mut note = format!("Codex now uses {to_abbr}.");
        match switched
            .from
            .as_ref()
            .and_then(|from| file.provider_subscription(from))
        {
            Some(from) => note.push_str(&format!(
                " Codex sessions already running keep {} until they restart.",
                from.abbr
            )),
            None if switched.from.is_some() => note.push_str(
                " Codex sessions already running keep their previous login until they restart.",
            ),
            None => {}
        }
        note
    };

    Ok(CodexSwitch { note, warnings })
}

/// The one place an account edit is written to the history, for the CLI and
/// the GUI: the events [`shared::edit_events`] makes of it (`accountLook` with
/// the changed look fields, `priceChanged` when the price changed), so the
/// other hosts take the edit (spec §5). The edit happened first; every event
/// is tried and the first failure is returned as the one sentence for an
/// unwritable history, never failing the edit. Without a recorder nothing is
/// written; the caller already says why.
pub(crate) fn record_edit(
    recorder: Option<&Recorder>,
    edited: impl Into<shared::Edit>,
    at: DateTime<Utc>,
) -> Option<String> {
    let recorder = recorder?;
    let mut first_error = None;
    for kind in shared::edit_events(edited) {
        if let Err(error) = recorder.event(at, kind) {
            first_error.get_or_insert_with(|| history_unwritable(&error));
        }
    }
    first_error
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

/// Who brings this host's history along to a changed history folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bringer {
    /// The tray app at its start: it holds `gui.lock` for its whole run, so
    /// no other tray app writes the history, and none of its own threads
    /// writes it yet.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    TrayApp,
    /// A command: only while no tray app runs. A running tray app may still be
    /// writing the old folder; it brings the history along at its next start.
    Command,
}

/// Bring this host's history folder to the history folder the settings name
/// now (`history::bring_along`), when the folder changed since the last run.
/// The warnings are `history::bring_along`'s; the error is the one every
/// front end shows when the move failed. A failed move does not keep the
/// history from opening: the move is tried again at the next start, and lines
/// written to the new folder meanwhile are kept.
pub(crate) fn bring_history_along(
    app: &AppPaths,
    settings: &Settings,
    by: Bringer,
) -> Result<Vec<String>, String> {
    // `feed` asks a running tray app to take the status line's numbers, which
    // the status line does every few seconds anyway; nothing else changes.
    bring_unless_a_tray_app_runs(app, settings, by, || ipc::send_line(app, "feed"))
}

/// [`bring_history_along`] with the question to the tray app passed in, so a
/// test never reaches a real tray app's socket.
fn bring_unless_a_tray_app_runs(
    app: &AppPaths,
    settings: &Settings,
    by: Bringer,
    ask_tray_app: impl FnOnce() -> Result<(), IpcError>,
) -> Result<Vec<String>, String> {
    let due = history::bring_along_due(app, settings).map_err(|error| not_brought(&error))?;
    if !due {
        return Ok(Vec::new());
    }
    // A command asks the tray app's socket, the way `seatswap panel` reaches
    // it, and never takes `gui.lock`: a tray app starting in that instant
    // would find the lock taken, believe another one runs, and exit. A tray
    // app that runs binds its socket before it opens the history, so one that
    // may be writing the old folder answers, and the command leaves the move
    // to that tray app's next start. One that starts after this question reads
    // the settings that name the new folder, never writes the old one, and
    // does its own move first, on the app lock, after this one.
    if by == Bringer::Command && tray_app_may_run(ask_tray_app()) {
        return Ok(Vec::new());
    }
    history::bring_along(app, settings).map_err(|error| not_brought(&error))
}

/// Whether a tray app may be running, from what its socket answered. Only a
/// socket nobody listens on says none runs; any answer (an older tray app's
/// "unknown command" too) and any other failure (a socket that does not
/// answer in time) may be a running tray app.
fn tray_app_may_run(answer: Result<(), IpcError>) -> bool {
    !matches!(answer, Err(IpcError::NotRunning))
}

/// The one sentence for a move of the history that failed.
fn not_brought(error: &dyn std::fmt::Display) -> String {
    format!("this host's history was not brought to the new history folder: {error}")
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
    use seatswap_core::store::{FileSecrets, ProviderSubscription, Store};

    const ACCOUNT_UUID: &str = "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30";
    const ORG_UUID: &str = "e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62";
    const EMAIL: &str = "someone@example.com";
    /// Two made-up Codex logins (R8), both real-shaped.
    const CODEX_LIVE_ID: &str = "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13";
    const CODEX_OTHER_ID: &str = "7d2b9e4c-1a3f-4c5e-8b6d-9f0a1c2e3d4b";
    const CODEX_LIVE_AUTH: &str = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","last_refresh":"2026-11-14T16:53:03.750022420Z","tokens":{"id_token":"eyJhbGciOi.id.sig","access_token":"eyJhbGciOi.access.sig","refresh_token":"rt_9f2c","account_id":"0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13","login_owner_id":"user-sam","oauth_provenance":{"issuer":"https://auth.openai.com","client_id":"app_4c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f"}}}"#;
    const CODEX_OTHER_AUTH: &str = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","last_refresh":"2026-11-15T09:10:00.000000000Z","tokens":{"id_token":"eyJhbGciOi.other.id","access_token":"eyJhbGciOi.other.sig","refresh_token":"rt_51ab","account_id":"7d2b9e4c-1a3f-4c5e-8b6d-9f0a1c2e3d4b","login_owner_id":"user-sam","oauth_provenance":{"issuer":"https://auth.openai.com","client_id":"app_4c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f"}}}"#;

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
        assert_eq!(events[0]["plan"], "Team Premium");

        // Saving the same login again updates the stored copy, not a new account.
        save_live_login(&fixture.claude, &store, Some(&recorder), None, Utc::now())
            .expect("the stored copy is updated");
        assert_eq!(event_values(history.path()).len(), 1);
        assert_eq!(store.load().expect("accounts").accounts[0].id, account.id);
    }

    /// The live login is stored again and the plan of the account changed
    /// (the login says "Team Premium", the stored plan says "Team"): the change
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
        // The stored plan is stale; the live login's tier says "Team Premium".
        let mut file = store.load().expect("accounts");
        file.accounts[0].plan = Some("Team".to_string());
        store.save(&file).expect("the stale plan is stored");

        let history = tempfile::tempdir().expect("history folder");
        let recorder = Recorder::new(HistoryDir::at(history.path(), "atlas"));
        save_live_login(&fixture.claude, &store, Some(&recorder), None, Utc::now())
            .expect("the stored copy is updated");
        assert_eq!(
            store.load().expect("accounts").accounts[0].plan.as_deref(),
            Some("Team Premium"),
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
        assert_eq!(changed[0]["to"], "Team Premium");
    }

    /// R8: a switch whose live login had no subscription creates it through
    /// the one `ensure_provider_subscription`, and the history records it as
    /// `accountAdded` beside the switch.
    #[test]
    fn a_subscription_made_by_a_switch_is_recorded_as_added() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let codex_home = fixture._tmp.path().join("codex");
        fs::create_dir_all(&codex_home).expect("the Codex folder");
        fs::write(codex_home.join("auth.json"), CODEX_LIVE_AUTH).expect("auth.json");
        let paths = CodexPaths::at(&codex_home);
        let live = AccountRef {
            acc: CODEX_LIVE_ID.to_string(),
            org: CODEX_ORG.to_string(),
        };
        let other = AccountRef {
            acc: CODEX_OTHER_ID.to_string(),
            org: CODEX_ORG.to_string(),
        };
        let mut file = store.load().expect("accounts.json");
        file.provider_subscriptions.push(ProviderSubscription {
            account: other.clone(),
            label: "OpenAI".to_string(),
            abbr: "OA2".to_string(),
            color: "#db2777".to_string(),
            plan: Some("Pro".to_string()),
            price: None,
            price_currency: None,
        });
        store.save(&file).expect("accounts.json");
        let login = serde_json::from_str(CODEX_OTHER_AUTH).expect("a Codex login");
        store
            .secrets()
            .set_codex(&providers::codex::secret_id(&other), &login)
            .expect("the saved copy");

        let history = tempfile::tempdir().expect("history folder");
        let recorder = Recorder::new(HistoryDir::at(history.path(), "atlas"));
        let switched = switch_codex(
            &fixture.app,
            &paths,
            &store,
            Some(&recorder),
            &other,
            Utc::now(),
        )
        .expect("the switch runs");
        assert!(switched.warnings.is_empty(), "{:?}", switched.warnings);
        assert_eq!(
            switched.note,
            "Codex now uses OA2. Codex sessions already running keep OA until they restart."
        );

        let file = store.load().expect("accounts.json");
        let made = file
            .provider_subscription(&live)
            .expect("the live login's subscription was created");
        assert_eq!(made.abbr, "OA");
        assert_eq!(made.plan, None, "a switch knows no plan for the old login");

        let events = event_values(history.path());
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0]["type"], "accountAdded");
        assert_eq!(events[0]["account"]["acc"], CODEX_LIVE_ID);
        assert_eq!(events[0]["account"]["org"], CODEX_ORG);
        assert_eq!(events[1]["type"], "providerSwitch");
        assert_eq!(events[1]["to"]["acc"], CODEX_OTHER_ID);
    }

    /// The provider error names the company through `Company::of_account`:
    /// OpenAI and Ollama by their names, a Claude organization as Anthropic,
    /// never a raw organization id and never "OpenAI (Codex)".
    #[test]
    fn a_failed_subscription_names_the_company_from_its_owner() {
        let fixture = Fixture::new();
        fs::write(fixture.app.accounts_file(), b"{ not json").expect("corrupt accounts");
        let codex = AccountRef {
            acc: CODEX_LIVE_ID.to_string(),
            org: CODEX_ORG.to_string(),
        };
        let claude = AccountRef {
            acc: ACCOUNT_UUID.to_string(),
            org: ORG_UUID.to_string(),
        };

        let openai = ensure_provider_subscription(&fixture.app, None, &codex, None, Utc::now());
        assert_eq!(openai.len(), 1, "{openai:?}");
        assert!(
            openai[0].starts_with("the OpenAI subscription is not saved: "),
            "{openai:?}"
        );
        assert!(!openai[0].contains("OpenAI (Codex)"), "{openai:?}");

        let anthropic = ensure_provider_subscription(&fixture.app, None, &claude, None, Utc::now());
        assert_eq!(anthropic.len(), 1, "{anthropic:?}");
        assert!(
            anthropic[0].starts_with("the Anthropic subscription is not saved: "),
            "{anthropic:?}"
        );
        assert!(!anthropic[0].contains(ORG_UUID), "{anthropic:?}");
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
        assert_eq!(events[0]["plan"], "Team Premium");
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

    /// A command brings the history along only while no tray app answers on
    /// its socket: a running tray app may still write the old folder.
    #[test]
    fn a_command_brings_the_history_along_only_while_no_tray_app_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let app = AppPaths::at(&tmp.path().join("data"));
        fs::create_dir_all(&app.data_dir).unwrap();
        let machine_id = "0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60";
        fs::write(app.machine_id_file(), machine_id).unwrap();
        // This host's history in the data folder, as every earlier version kept it.
        let old = app.history_dir().join("atlas");
        fs::create_dir_all(&old).unwrap();
        fs::write(
            old.join("host.json"),
            format!("{{\"machineId\":\"{machine_id}\"}}"),
        )
        .unwrap();
        fs::write(
            old.join("up-2026-11.jsonl"),
            "{\"t\":\"2026-11-14T09:00:00Z\"}\n",
        )
        .unwrap();
        let new = tmp.path().join("Dropbox").join("seatswap");
        let settings = Settings {
            host_name: Some("atlas".to_string()),
            history_dir: Some(new.clone()),
            ..Settings::default()
        };

        // A tray app answers: it may be writing the old folder, so nothing
        // moves, and gui.lock is never taken.
        let answers = || Ok(());
        assert_eq!(
            bring_unless_a_tray_app_runs(&app, &settings, Bringer::Command, answers),
            Ok(Vec::new())
        );
        assert!(!new.exists(), "nothing moves while the tray app runs");
        assert!(!app.history_root_file().exists());
        assert!(!app.gui_lock().exists(), "gui.lock is never taken");

        // The tray app itself moves at its start without asking anyone.
        let asked = std::cell::Cell::new(false);
        let tray_start = Settings {
            history_dir: Some(tmp.path().join("elsewhere")),
            ..settings.clone()
        };
        let ask = || {
            asked.set(true);
            Ok(())
        };
        assert_eq!(
            bring_unless_a_tray_app_runs(&app, &tray_start, Bringer::TrayApp, ask),
            Ok(Vec::new())
        );
        assert!(!asked.get());
        assert!(tmp
            .path()
            .join("elsewhere/atlas/up-2026-11.jsonl")
            .is_file());
        fs::remove_file(app.history_root_file()).unwrap();

        // Nobody listens on the socket: no tray app runs, the command moves.
        let nobody = || Err(IpcError::NotRunning);
        assert_eq!(
            bring_unless_a_tray_app_runs(&app, &settings, Bringer::Command, nobody),
            Ok(Vec::new())
        );
        assert!(new.join("atlas/up-2026-11.jsonl").is_file());
        assert!(app.history_root_file().is_file());
        assert!(
            old.join("up-2026-11.jsonl").is_file(),
            "the old folder stays"
        );
    }

    /// Only a socket nobody listens on means no tray app runs.
    #[test]
    fn only_a_silent_socket_means_no_tray_app_runs() {
        assert!(!tray_app_may_run(Err(IpcError::NotRunning)));
        assert!(tray_app_may_run(Ok(())));
        assert!(tray_app_may_run(Err(IpcError::Reply(
            "error unknown command".to_string()
        ))));
        assert!(tray_app_may_run(Err(IpcError::Io {
            path: std::path::PathBuf::from("/run/user/1000/seatswap-0f3a9c21d4e6b781.sock"),
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"),
        })));
    }
}
