//! The Codex reader and its saved logins (spec §7.2, R8): `auth.json` and
//! Codex's usage answer into a [`ProviderReading`], the live login kept in the
//! secrets store, and the switch between two saved logins. A usage poll reads
//! `auth.json` without a lock (Codex reads it that way too). Every write of a
//! stored copy, and every switch, first takes seatswap's own app lock
//! ([`APP_LOCK_TIMEOUT`]); a switch then takes Codex's own two locks, in the
//! order app lock, `auth.lock`, `auth.json.lock`, and reads the live file only
//! after all of them. `auth.json` is written only when switching, with the
//! [`crate::atomic`] write. No token is printed or put in an error text.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::api::{mark_highest_active, Api, ApiError, Limit, LimitKind, Severity, Usage};
use crate::atomic::{self, Access};
use crate::history::AccountRef;
use crate::locks::{acquire_app_lock, acquire_file_lock, FileLock};
use crate::paths::{AppPaths, CodexPaths};
use crate::store::SecretStore;
use crate::switcher::APP_LOCK_TIMEOUT;
use crate::types::CodexLogin;

use super::{ProviderError, ProviderReading, CODEX_ORG};

/// How long a switch waits for Codex's two locks together (R8): one deadline for
/// both, so the second gets what the first left.
pub const CODEX_LOCK_TIMEOUT_SECS: u64 = 10;

/// The one issuer a stored login may name for seatswap to renew it: a token is
/// never sent to another issuer (R8).
const CODEX_ISSUER: &str = "https://auth.openai.com";

/// What a renewal failure tells the user to do.
const SIGN_IN_AGAIN: &str = "Sign in again with codex login.";

/// The refusal a Remove of the live Codex login gets (R8). The worker uses it as
/// the action's error and the provider snapshot sends it to Settings, so the
/// sentence has one home.
pub const LIVE_LOGIN_REMOVE: &str =
    "the live login: switch Codex to another login first to remove it";

/// The stored copy's id: the first 32 hex digits of SHA-256 of `codex:<acc>`
/// (R8). 32 lower-case hex characters, which is the id shape
/// [`crate::store::FileSecrets`] accepts.
pub fn secret_id(account: &AccountRef) -> String {
    Sha256::digest(format!("codex:{}", account.acc).as_bytes())
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The live login as it was saved: which account it belongs to, and whether the
/// stored copy was created or replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveCodex {
    pub account: AccountRef,
    pub saved: bool,
}

/// Read Codex's live login and keep it in the secrets store (R8). The stored
/// copy is replaced when there is none or its `tokens` differ, so a login Codex
/// renewed, or one the user just signed in with, is saved at the next poll, and
/// a poll that changed nothing writes nothing.
///
/// Takes seatswap's own app lock ([`APP_LOCK_TIMEOUT`]) before the live file is
/// read and the stored copy written. A live ChatGPT login is saved only when
/// its `tokens` hold a non-empty `account_id`, `access_token` and
/// `refresh_token`; otherwise [`ProviderError::Shape`] names the first missing
/// field and the stored copy stays. A login kept in the keyring or an API-key
/// login is not saved and is its own error; a missing file is
/// [`ProviderError::NoLogin`].
pub fn sync_live(
    app: &AppPaths,
    paths: &CodexPaths,
    secrets: &dyn SecretStore,
) -> Result<LiveCodex, ProviderError> {
    let _app_lock = acquire_app_lock(app, APP_LOCK_TIMEOUT)?;
    let login = live_login(paths)?.ok_or_else(|| ProviderError::NoLogin {
        path: paths.auth_file.clone(),
    })?;
    let (_, account) = savable_login(&login, &paths.auth_file)?;
    let saved = save_copy(secrets, &account, &login)?;
    Ok(LiveCodex { account, saved })
}

/// Read Codex's login and ask its usage endpoint for the account's limits: the
/// live login, whose tokens Codex renews itself. The reading's plan comes from
/// the answer's `plan_type`. A `tokens` object is read as a ChatGPT login by
/// the one rule [`chatgpt_tokens`] documents; usage needs only `access_token`
/// and `account_id` there.
pub fn read(paths: &CodexPaths, api: &Api) -> Result<ProviderReading, ProviderError> {
    let login = live_login(paths)?.ok_or_else(|| ProviderError::NoLogin {
        path: paths.auth_file.clone(),
    })?;
    let tokens = chatgpt_tokens(&login, &paths.auth_file)?;
    let access_token = string_field(tokens, "access_token")?;
    let account_id = string_field(tokens, "account_id")?;
    let answer = api
        .codex_usage(access_token, account_id)
        .map_err(api_error)?;
    reading_from(&answer, account_id)
}

/// The account Codex's live `auth.json` belongs to, read by the one
/// [`chatgpt_tokens`] rule and without saving anything or taking a lock (R8).
/// `None` when there is no login file. A login kept in the keyring, an API-key
/// login, a file that does not read and a file without an `account_id` are
/// their own errors, exactly as [`sync_live`] and [`read`] report them.
///
/// The one read-only way to know which saved login is live, for `codex list`
/// and for the worker's guard against removing the live login's copy. A poll
/// uses [`sync_live`], which also stores what it sees.
pub fn live_account(paths: &CodexPaths) -> Result<Option<AccountRef>, ProviderError> {
    let login = match live_login(paths)? {
        Some(login) => login,
        None => return Ok(None),
    };
    let tokens = chatgpt_tokens(&login, &paths.auth_file)?;
    let account_id = string_field(tokens, "account_id")?;
    Ok(Some(AccountRef {
        acc: account_id.to_string(),
        org: CODEX_ORG.to_string(),
    }))
}

/// Whether a saved login may still be renewed: the live login is not, and the
/// live file is read again here, under the caller's app lock, so a login that
/// became live after the round began is refused ([`ProviderError::BecameLive`]). No file, a
/// keyring login and an API-key login mean no saved login is live; a file that
/// does not read is its own error, never a guess.
fn ensure_not_live(paths: &CodexPaths, account: &AccountRef) -> Result<(), ProviderError> {
    match live_account(paths) {
        Ok(Some(live)) if &live == account => Err(ProviderError::BecameLive),
        Ok(_) => Ok(()),
        Err(ProviderError::KeyringLogin) | Err(ProviderError::ApiKeyLogin) => Ok(()),
        Err(error) => Err(error),
    }
}

/// A saved Codex login that is not live: its usage, asked with the stored access
/// token (R8). The first usage ask is a network call without a lock. A 401
/// renews the login once at Codex's own issuer and asks again; a renewal that
/// fails, or a second 401, is [`ProviderError::RenewFailed`] and the copy stays,
/// so the user can sign in again with `codex login`.
///
/// The renewal — the token POST and storing its answer — runs under seatswap's
/// own app lock, after the live file has been read under that lock: a login that
/// became live during the poll is not renewed ([`ProviderError::BecameLive`]), because Codex
/// renews the live login itself. Under the same lock the stored copy is re-read:
/// the renewed copy replaces it only when it still holds the tokens the renewal
/// started from ([`CodexLogin::same_tokens`]); a newer copy someone saved
/// meanwhile stays, and the second usage ask uses that one. There is no second
/// renewal.
///
/// Never used for the live login: Codex renews that one itself, and the caller
/// reads it with [`read`].
pub fn read_saved(
    app: &AppPaths,
    paths: &CodexPaths,
    secrets: &dyn SecretStore,
    account: &AccountRef,
    api: &Api,
) -> Result<ProviderReading, ProviderError> {
    let id = secret_id(account);
    let login = secrets
        .get_codex(&id)
        .map_err(ProviderError::Store)?
        .ok_or_else(|| ProviderError::NotSaved {
            acc: account.acc.clone(),
        })?;
    let tokens = saved_tokens(&login)?;
    let login_account = string_field(tokens, "account_id")?;
    let access_token = string_field(tokens, "access_token")?;
    match api.codex_usage(access_token, login_account) {
        Ok(answer) => reading_from(&answer, login_account),
        Err(ApiError::Unauthorized) => {
            // The lock covers the live check, the token POST and the store, and
            // nothing else: the second usage ask runs without it.
            let login = {
                let _app_lock = acquire_app_lock(app, APP_LOCK_TIMEOUT)?;
                ensure_not_live(paths, account)?;
                // The copy is read again under the lock: one someone renewed
                // while this round waited is asked with as it is, so a spent
                // refresh token is never sent.
                let current = secrets
                    .get_codex(&id)
                    .map_err(ProviderError::Store)?
                    .ok_or_else(|| ProviderError::NotSaved {
                        acc: account.acc.clone(),
                    })?;
                if current.same_tokens(&login) {
                    let renewed = renew(&current, api)?;
                    store_renewed(secrets, &id, &current, renewed, account)?
                } else {
                    current
                }
            };
            let tokens = saved_tokens(&login)?;
            let access_token = string_field(tokens, "access_token")?;
            match api.codex_usage(access_token, login_account) {
                Ok(answer) => reading_from(&answer, login_account),
                Err(ApiError::Unauthorized) => {
                    Err(ProviderError::RenewFailed(SIGN_IN_AGAIN.to_string()))
                }
                Err(other) => Err(ProviderError::Api(other)),
            }
        }
        Err(other) => Err(ProviderError::Api(other)),
    }
}

/// Forget seatswap's saved copy of a Codex login (R8). Codex's own `auth.json`
/// is not touched, so a login that is live keeps working; a copy that is not
/// there is not an error. The delete runs under seatswap's app lock, as every
/// other write of a stored copy does.
pub fn forget(
    app: &AppPaths,
    secrets: &dyn SecretStore,
    account: &AccountRef,
) -> Result<(), ProviderError> {
    let _app_lock = acquire_app_lock(app, APP_LOCK_TIMEOUT)?;
    secrets.delete(&secret_id(account))?;
    Ok(())
}

/// A finished switch: which login was live before (none when `auth.json` was
/// missing) and which one is live now.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexSwitched {
    pub from: Option<AccountRef>,
    pub to: AccountRef,
}

/// Switch Codex's live login to the stored copy of `to` (R8). The locks are taken
/// in the order seatswap's own app lock ([`APP_LOCK_TIMEOUT`]), then Codex's own
/// `auth.lock`, then `auth.json.lock`, within one
/// [`CODEX_LOCK_TIMEOUT_SECS`] deadline for Codex's two; a lock that stays busy
/// is [`ProviderError::Lock`] and names the file.
///
/// Only after all locks are held is the target's stored copy read, so a copy
/// saved while the switch waited is the one written. Then the live login is
/// read and saved first (as [`sync_live`] does). A login seatswap cannot save —
/// kept in the keyring, an API key, incomplete, or not JSON — stops the switch
/// with that error and nothing is written, so seatswap never overwrites a login
/// it has not stored; a missing `auth.json` is no `from`. Then the target's
/// stored object is written whole, key order included and mode 0600.
///
/// When the live login already is `to`, nothing is written and `from` is
/// `Some(to)`: the caller records no event then. This writes no history; the
/// caller records [`crate::history::EventKind::ProviderSwitch`].
pub fn switch_to(
    app: &AppPaths,
    paths: &CodexPaths,
    secrets: &dyn SecretStore,
    to: &AccountRef,
) -> Result<CodexSwitched, ProviderError> {
    // Declaration order is the lock order; Rust drops them in reverse.
    let _app_lock = acquire_app_lock(app, APP_LOCK_TIMEOUT)?;
    let deadline = Instant::now() + Duration::from_secs(CODEX_LOCK_TIMEOUT_SECS);
    let _auth_lock = lock_until(&paths.auth_lock, deadline)?;
    let _auth_json_lock = lock_until(&paths.auth_json_lock, deadline)?;

    // Never trust anything read before taking the locks.
    let target = secrets
        .get_codex(&secret_id(to))
        .map_err(ProviderError::Store)?
        .ok_or_else(|| ProviderError::NotSaved {
            acc: to.acc.clone(),
        })?;

    let from = match live_login(paths)? {
        None => None,
        Some(login) => {
            let (_, account) = savable_login(&login, &paths.auth_file)?;
            save_copy(secrets, &account, &login)?;
            Some(account)
        }
    };
    if from.as_ref() == Some(to) {
        return Ok(CodexSwitched {
            from,
            to: to.clone(),
        });
    }

    let bytes = Value::Object(target.0).to_string();
    atomic::write_atomic(&paths.auth_file, bytes.as_bytes(), Access::Private).map_err(
        |source| ProviderError::Io {
            path: paths.auth_file.clone(),
            source,
        },
    )?;
    Ok(CodexSwitched {
        from,
        to: to.clone(),
    })
}

/// Codex's live `auth.json`, or `None` when there is no file. A file that does
/// not read is [`ProviderError::Io`]; one that is not a JSON object is
/// [`ProviderError::Shape`].
fn live_login(paths: &CodexPaths) -> Result<Option<CodexLogin>, ProviderError> {
    let bytes = match std::fs::read(&paths.auth_file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ProviderError::Io {
                path: paths.auth_file.clone(),
                source,
            })
        }
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| ProviderError::Shape(format!("auth.json is not JSON: {error}")))?;
    match value {
        Value::Object(map) => Ok(Some(CodexLogin(map))),
        _ => Err(ProviderError::Shape(
            "auth.json is not a JSON object".to_string(),
        )),
    }
}

/// The `tokens` of a ChatGPT login, read by one rule for [`read`], [`sync_live`]
/// and [`switch_to`], in this order:
///
/// 1. `auth_mode` `"apikey"` is [`ProviderError::ApiKeyLogin`];
/// 2. `tokens` absent or `null` is [`ProviderError::KeyringLogin`] when
///    `auth_mode` is `"chatgpt"`, else [`ProviderError::NoLogin`] (as 1.2.2);
/// 3. `tokens` that is not an object is [`ProviderError::Shape`] with
///    `"auth.json's tokens is not an object"` (as 1.2.2);
/// 4. a `tokens` object is a ChatGPT login, whatever `auth_mode` says or whether
///    it is there at all (as 1.2.2 read it).
fn chatgpt_tokens<'a>(
    login: &'a CodexLogin,
    auth_file: &Path,
) -> Result<&'a Map<String, Value>, ProviderError> {
    if login.auth_mode() == Some("apikey") {
        return Err(ProviderError::ApiKeyLogin);
    }
    match login.0.get("tokens") {
        None | Some(Value::Null) => Err(if login.auth_mode() == Some("chatgpt") {
            ProviderError::KeyringLogin
        } else {
            ProviderError::NoLogin {
                path: auth_file.to_path_buf(),
            }
        }),
        Some(Value::Object(tokens)) => Ok(tokens),
        Some(_) => Err(ProviderError::Shape(
            "auth.json's tokens is not an object".to_string(),
        )),
    }
}

/// The `tokens` and account of a live ChatGPT login seatswap may save or switch:
/// [`chatgpt_tokens`]' rule, and its tokens hold a non-empty `account_id`,
/// `access_token` and `refresh_token`. Otherwise [`ProviderError::Shape`] names
/// the first missing field, and the stored copy is not touched. Its `AccountRef`
/// is the login's own `tokens.account_id` under [`CODEX_ORG`] (R8).
fn savable_login<'a>(
    login: &'a CodexLogin,
    auth_file: &Path,
) -> Result<(&'a Map<String, Value>, AccountRef), ProviderError> {
    let tokens = chatgpt_tokens(login, auth_file)?;
    let account_id = complete_field(tokens, "account_id")?;
    complete_field(tokens, "access_token")?;
    complete_field(tokens, "refresh_token")?;
    Ok((
        tokens,
        AccountRef {
            acc: account_id.to_string(),
            org: CODEX_ORG.to_string(),
        },
    ))
}

/// A token field a live login must have before seatswap saves it; the error
/// names the key only, never a value.
fn complete_field<'a>(
    tokens: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a str, ProviderError> {
    tokens
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::Shape(format!("the Codex login lacks {name}")))
}

/// A stored login's `tokens`; a stored copy always has them.
fn saved_tokens(login: &CodexLogin) -> Result<&Map<String, Value>, ProviderError> {
    login
        .tokens()
        .ok_or_else(|| ProviderError::Shape("the saved Codex login has no tokens".to_string()))
}

/// Keep `login` in the secrets store under its account's id when the stored copy
/// holds other tokens. True when the copy was created or replaced.
fn save_copy(
    secrets: &dyn SecretStore,
    account: &AccountRef,
    login: &CodexLogin,
) -> Result<bool, ProviderError> {
    let id = secret_id(account);
    let stored = secrets.get_codex(&id).map_err(ProviderError::Store)?;
    if stored
        .as_ref()
        .is_some_and(|stored| stored.same_tokens(login))
    {
        return Ok(false);
    }
    secrets
        .set_codex(&id, login)
        .map_err(ProviderError::Store)?;
    Ok(true)
}

/// Store a renewed copy after the renewal's network calls; [`read_saved`] holds
/// seatswap's own app lock around this call and the token POST. The stored copy
/// is re-read here: when it still holds the tokens the renewal started from
/// ([`CodexLogin::same_tokens`]) the renewed copy replaces it; otherwise a newer
/// copy someone saved meanwhile stays untouched. The login to ask usage with
/// next is returned: the renewed one, or the newer stored one. A copy removed
/// meanwhile is [`ProviderError::NotSaved`].
fn store_renewed(
    secrets: &dyn SecretStore,
    id: &str,
    started_from: &CodexLogin,
    renewed: CodexLogin,
    account: &AccountRef,
) -> Result<CodexLogin, ProviderError> {
    let stored = secrets
        .get_codex(id)
        .map_err(ProviderError::Store)?
        .ok_or_else(|| ProviderError::NotSaved {
            acc: account.acc.clone(),
        })?;
    if !stored.same_tokens(started_from) {
        return Ok(stored);
    }
    secrets
        .set_codex(id, &renewed)
        .map_err(ProviderError::Store)?;
    Ok(renewed)
}

/// Take one of Codex's advisory locks with what is left of the switch's one
/// deadline (R8).
fn lock_until(path: &Path, deadline: Instant) -> Result<FileLock, ProviderError> {
    let left = deadline.saturating_duration_since(Instant::now());
    acquire_file_lock(path, left).map_err(ProviderError::Lock)
}

/// Renew a stored login once (R8). Only a login that names [`CODEX_ISSUER`] is
/// renewed, so its refresh token never goes to another issuer. The answer must
/// carry a non-empty `access_token`; its `id_token`, `access_token` and
/// `refresh_token` replace the stored ones in their places, a key the answer
/// does not carry keeps its value, `last_refresh` becomes now in Codex's own
/// form, and every other key stays as it was. An answer that renews nothing is
/// [`ProviderError::RenewFailed`] and nothing is stored.
fn renew(login: &CodexLogin, api: &Api) -> Result<CodexLogin, ProviderError> {
    let tokens = saved_tokens(login)?;
    let provenance = tokens
        .get("oauth_provenance")
        .and_then(Value::as_object)
        .filter(|provenance| provenance.get("issuer").and_then(Value::as_str) == Some(CODEX_ISSUER))
        .ok_or_else(|| ProviderError::RenewFailed("the login names another issuer".to_string()))?;
    let client_id = provenance
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ProviderError::RenewFailed(SIGN_IN_AGAIN.to_string()))?;
    let refresh_token = string_field(tokens, "refresh_token")
        .map_err(|_| ProviderError::RenewFailed(SIGN_IN_AGAIN.to_string()))?;
    let answer = api
        .codex_refresh(client_id, refresh_token)
        .map_err(renewal_error)?;
    let access_token = answer
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::RenewFailed(SIGN_IN_AGAIN.to_string()))?;

    let mut renewed = login.clone();
    let tokens = renewed
        .0
        .get_mut("tokens")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| ProviderError::Shape("the saved Codex login has no tokens".to_string()))?;
    tokens.insert(
        "access_token".to_string(),
        Value::String(access_token.to_string()),
    );
    for key in ["id_token", "refresh_token"] {
        if let Some(value) = answer
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            tokens.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
    renewed.0.insert(
        "last_refresh".to_string(),
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)),
    );
    Ok(renewed)
}

/// A failed refresh call: a 400 or 401 answer means the refresh token is dead,
/// so the user signs in again; any other failure (5xx, transport, a bad body)
/// is an [`ProviderError::Api`] error — the login may still be good, and the
/// user is not told to sign in again.
fn renewal_error(error: ApiError) -> ProviderError {
    match error {
        ApiError::Unauthorized | ApiError::InvalidGrant(_) => {
            ProviderError::RenewFailed(SIGN_IN_AGAIN.to_string())
        }
        ApiError::Http { status: 400, .. } => ProviderError::RenewFailed(SIGN_IN_AGAIN.to_string()),
        other => ProviderError::Api(other),
    }
}

/// A usage answer as a reading, after checking that it names the login's own
/// account. Two ids that differ are [`ProviderError::Shape`]; neither value is
/// printed. The one rule for [`read`] and [`read_saved`].
fn reading_from(answer: &Value, login_account_id: &str) -> Result<ProviderReading, ProviderError> {
    let (usage, plan, account) = usage_from(answer)?;
    if account.acc != login_account_id {
        return Err(ProviderError::Shape(
            "the usage answer and the login name different account ids".to_string(),
        ));
    }
    Ok(ProviderReading {
        account,
        plan,
        usage: Some(usage),
        credits: None,
        spend_24h: None,
        hours: None,
    })
}

/// An API failure of a usage request: a dead login is its own variant.
fn api_error(error: ApiError) -> ProviderError {
    match error {
        ApiError::Unauthorized => ProviderError::LoginExpired,
        other => ProviderError::Api(other),
    }
}

/// A string token field, named by its key only; its value never enters an error.
fn string_field<'a>(tokens: &'a Map<String, Value>, name: &str) -> Result<&'a str, ProviderError> {
    match tokens.get(name).and_then(Value::as_str) {
        None => Err(ProviderError::Shape(format!(
            "auth.json's tokens.{name} is missing or not a string"
        ))),
        Some("") => Err(ProviderError::Shape(format!(
            "auth.json's tokens.{name} is empty"
        ))),
        Some(value) => Ok(value),
    }
}

/// The usage answer's windows as [`Limit`]s, with the plan and the account.
///
/// `rate_limit`'s own windows give [`LimitKind::Session`] (18000 s) and
/// [`LimitKind::WeeklyAll`] (604800 s). An entry of `additional_rate_limits`
/// never gives the account's session: 604800 s is
/// [`LimitKind::WeeklyScoped`], every other length keeps its own label
/// (`Other("5h limit")` and so on), and the entry's `limit_name` is the
/// limit's model.
pub fn usage_from(answer: &Value) -> Result<(Usage, Option<String>, AccountRef), ProviderError> {
    let object = answer
        .as_object()
        .ok_or_else(|| ProviderError::Shape("the Codex answer is not a JSON object".to_string()))?;

    let mut limits = Vec::new();
    match object.get("rate_limit") {
        Some(Value::Null) => {}
        Some(Value::Object(rate_limit)) => {
            push_windows(rate_limit, "rate_limit", false, None, &mut limits)?;
        }
        None => {
            return Err(ProviderError::Shape(
                "the Codex answer has no rate_limit".to_string(),
            ))
        }
        Some(_) => {
            return Err(ProviderError::Shape(
                "the Codex answer's rate_limit is not an object".to_string(),
            ))
        }
    }
    match object.get("additional_rate_limits") {
        None | Some(Value::Null) => {}
        Some(Value::Array(entries)) => {
            for (index, entry) in entries.iter().enumerate() {
                let path = format!("additional_rate_limits[{index}]");
                let entry = entry
                    .as_object()
                    .ok_or_else(|| ProviderError::Shape(format!("{path} is not a JSON object")))?;
                match entry.get("rate_limit") {
                    Some(Value::Null) => continue,
                    Some(Value::Object(rate_limit)) => {
                        let limit_name = entry
                            .get("limit_name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ProviderError::Shape(format!(
                                    "{path}.limit_name is missing or not a string"
                                ))
                            })?
                            .to_string();
                        push_windows(
                            rate_limit,
                            &format!("{path}.rate_limit"),
                            true,
                            Some(limit_name),
                            &mut limits,
                        )?;
                    }
                    _ => {
                        return Err(ProviderError::Shape(format!(
                            "{path}.rate_limit is missing or not an object"
                        )))
                    }
                }
            }
        }
        Some(_) => {
            return Err(ProviderError::Shape(
                "the Codex answer's additional_rate_limits is not an array".to_string(),
            ))
        }
    }

    let account_id = object
        .get("account_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProviderError::Shape(
                "the Codex answer's account_id is missing or not a string".to_string(),
            )
        })?;
    if account_id.is_empty() {
        return Err(ProviderError::Shape(
            "the Codex answer's account_id is empty".to_string(),
        ));
    }
    let plan = plan_type(object.get("plan_type"))?;

    mark_highest_active(&mut limits);

    Ok((
        Usage {
            limits,
            extra_usage_enabled: false,
            warnings: Vec::new(),
        },
        plan,
        AccountRef {
            acc: account_id.to_string(),
            org: CODEX_ORG.to_string(),
        },
    ))
}

fn push_windows(
    rate_limit: &Map<String, Value>,
    path: &str,
    additional: bool,
    model: Option<String>,
    limits: &mut Vec<Limit>,
) -> Result<(), ProviderError> {
    for name in ["primary_window", "secondary_window"] {
        let window_path = format!("{path}.{name}");
        match rate_limit.get(name) {
            None | Some(Value::Null) => {}
            Some(window) => limits.push(limit_from_window(
                window,
                &window_path,
                additional,
                model.clone(),
            )?),
        }
    }
    Ok(())
}

fn limit_from_window(
    window: &Value,
    path: &str,
    additional: bool,
    model: Option<String>,
) -> Result<Limit, ProviderError> {
    let object = window
        .as_object()
        .ok_or_else(|| ProviderError::Shape(format!("{path} is not a JSON object")))?;
    // Codex sends no severity; as for Claude's fallback windows, the load level
    // comes from the percent alone.
    let percent = object
        .get("used_percent")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            ProviderError::Shape(format!("{path}.used_percent is missing or not a number"))
        })?;
    if percent < 0.0 {
        return Err(ProviderError::Shape(format!(
            "{path}.used_percent is negative"
        )));
    }
    let seconds = match object.get("limit_window_seconds") {
        Some(value @ Value::Number(_)) => value.as_i64().ok_or_else(|| {
            ProviderError::Shape(format!("{path}.limit_window_seconds is not a whole number"))
        })?,
        _ => {
            return Err(ProviderError::Shape(format!(
                "{path}.limit_window_seconds is missing or not a number"
            )))
        }
    };
    if seconds <= 0 {
        return Err(ProviderError::Shape(format!(
            "{path}.limit_window_seconds is zero or negative"
        )));
    }
    let resets_at =
        match object.get("reset_at") {
            None | Some(Value::Null) => None,
            Some(Value::Number(_)) => {
                let seconds = object
                    .get("reset_at")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| {
                        ProviderError::Shape(format!("{path}.reset_at is not a whole number"))
                    })?;
                Some(DateTime::<Utc>::from_timestamp(seconds, 0).ok_or_else(|| {
                    ProviderError::Shape(format!("{path}.reset_at is out of range"))
                })?)
            }
            Some(_) => {
                return Err(ProviderError::Shape(format!(
                    "{path}.reset_at is not a number"
                )))
            }
        };
    Ok(Limit {
        kind: window_kind(seconds as u64, additional),
        percent,
        severity: Severity::Normal,
        resets_at,
        model,
        is_active: false,
    })
}

/// Codex's window lengths: `rate_limit`'s own 5 h and week have names; an
/// additional limit's week is scoped to its model, and every other length keeps
/// its own duration (review item 42): `24h limit`, `1h 30min limit`.
fn window_kind(seconds: u64, additional: bool) -> LimitKind {
    match (seconds, additional) {
        (18_000, false) => LimitKind::Session,
        (604_800, false) => LimitKind::WeeklyAll,
        (604_800, true) => LimitKind::WeeklyScoped,
        _ => LimitKind::Other(format!("{} limit", crate::duration::duration_text(seconds))),
    }
}

fn plan_type(value: Option<&Value>) -> Result<Option<String>, ProviderError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let mut chars = text.chars();
            let Some(first) = chars.next() else {
                return Ok(None);
            };
            Ok(Some(first.to_uppercase().chain(chars).collect()))
        }
        Some(_) => Err(ProviderError::Shape(
            "the Codex answer's plan_type is not a string".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use serde_json::json;
    use tiny_http::{Response, Server, StatusCode};

    use crate::locks::LockError;
    use crate::store::FileSecrets;

    const ACCOUNT_ID: &str = "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13";
    const ACCESS_TOKEN: &str = "eyJhbGciOi.eyJzdWIiOiJ1c2VyLXNhbSIsImVtYWlsIjoic2FtQG1haWwuZXhhbXBsZSIsImV4cCI6MTc5NDY0MDAwMH0.7f3a91c2e4b6d8f0a2c4e6b8d0f2a4c6e8b0d2f4a6c8e0b2d4f6a8c0e2b4d6f8";
    const REFRESH_TOKEN: &str = "rt_9f2c5d7e1b3a4c6d8e0f2a4b6c8d0e2f";
    const ID_TOKEN: &str =
        "eyJhbGciOi.eyJzdWIiOiJ1c2VyLXNhbSJ9.1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";

    const USAGE_ANSWER: &str = r#"{
      "user_id": "user-sam",
      "account_id": "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13",
      "email": "sam@mail.example",
      "plan_type": "plus",
      "rate_limit": {
        "allowed": true,
        "limit_reached": false,
        "primary_window": {
          "used_percent": 12,
          "limit_window_seconds": 18000,
          "reset_after_seconds": 17100,
          "reset_at": 1794693195
        },
        "secondary_window": {
          "used_percent": 48,
          "limit_window_seconds": 604800,
          "reset_after_seconds": 316800,
          "reset_at": 1795279995
        }
      },
      "code_review_rate_limit": null,
      "additional_rate_limits": null,
      "credits": {
        "has_credits": false,
        "unlimited": false,
        "overage_limit_reached": false,
        "balance": "0"
      },
      "spend_control": {"reached": false, "individual_limit": null},
      "rate_limit_reached_type": null
    }"#;

    fn answer() -> Value {
        serde_json::from_str(USAGE_ANSWER).expect("the fixture answer is JSON")
    }

    fn auth_json() -> Value {
        json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "last_refresh": "2026-11-14T16:53:03.750022420Z",
            "tokens": {
                "id_token": ID_TOKEN,
                "access_token": ACCESS_TOKEN,
                "refresh_token": REFRESH_TOKEN,
                "account_id": ACCOUNT_ID,
                "login_owner_id": "user-sam",
                "oauth_provenance": "cli"
            }
        })
    }

    fn write_auth(paths: &CodexPaths) {
        std::fs::write(&paths.auth_file, auth_json().to_string()).expect("write auth.json");
    }

    /// The fixture `auth.json` after one edit, in a tempdir of its own.
    fn with_auth(edit: impl FnOnce(&mut Value)) -> (tempfile::TempDir, CodexPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        let mut auth = auth_json();
        edit(&mut auth);
        std::fs::write(&paths.auth_file, auth.to_string()).unwrap();
        (tmp, paths)
    }

    struct Reply {
        status: u16,
        body: String,
    }

    impl Reply {
        fn ok(body: impl Into<String>) -> Reply {
            Reply {
                status: 200,
                body: body.into(),
            }
        }

        fn status(status: u16, body: impl Into<String>) -> Reply {
            Reply {
                status,
                body: body.into(),
            }
        }
    }

    #[derive(Debug)]
    struct Captured {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Captured {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    fn start_server(reply: Reply) -> (String, Receiver<Captured>, JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let captured = capture(&mut request);
            let response =
                Response::from_string(reply.body).with_status_code(StatusCode(reply.status));
            request.respond(response).unwrap();
            sender.send(captured).unwrap();
        });
        (base, receiver, handle)
    }

    /// One request as the fake server saw it, its body included.
    fn capture(request: &mut tiny_http::Request) -> Captured {
        let mut bytes = Vec::new();
        let _ = request.as_reader().read_to_end(&mut bytes);
        Captured {
            method: request.method().as_str().to_string(),
            path: request.url().to_string(),
            headers: request
                .headers()
                .iter()
                .map(|header| {
                    (
                        header.field.as_str().as_str().to_string(),
                        header.value.as_str().to_string(),
                    )
                })
                .collect(),
            body: String::from_utf8_lossy(&bytes).to_string(),
        }
    }

    /// A server that answers `replies` in order, one request each.
    fn start_script(replies: Vec<Reply>) -> (String, Receiver<Captured>, JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            for reply in replies {
                let Ok(mut request) = server.recv() else {
                    return;
                };
                let captured = capture(&mut request);
                let response =
                    Response::from_string(reply.body).with_status_code(StatusCode(reply.status));
                if request.respond(response).is_err() {
                    return;
                }
                if sender.send(captured).is_err() {
                    return;
                }
            }
        });
        (base, receiver, handle)
    }

    fn received(receiver: &Receiver<Captured>) -> Captured {
        receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the fake server received no request")
    }

    fn api_for(base: &str) -> Api {
        Api::new().with_codex_url(&format!("{base}/backend-api/wham/usage"))
    }

    /// A server that reports whether anything reached it within a short window.
    fn no_request_server() -> (String, Receiver<bool>, JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let got = server
                .recv_timeout(std::time::Duration::from_millis(500))
                .unwrap()
                .is_some();
            sender.send(got).unwrap();
        });
        (base, receiver, handle)
    }

    fn saw_no_request(receiver: &Receiver<bool>, handle: JoinHandle<()>) {
        let got = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the counting server did not report");
        handle.join().unwrap();
        assert!(!got, "a request was sent");
    }

    fn closed_api() -> Api {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        Api::new().with_codex_url(&format!("http://{address}/backend-api/wham/usage"))
    }

    fn captured(receiver: &Receiver<Captured>, handle: JoinHandle<()>) -> Captured {
        let request = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the fake server received no request");
        handle.join().unwrap();
        request
    }

    #[test]
    fn the_plus_answer_becomes_two_limits() {
        let (usage, plan, account) = usage_from(&answer()).unwrap();

        assert_eq!(plan.as_deref(), Some("Plus"));
        assert_eq!(
            account,
            AccountRef {
                acc: ACCOUNT_ID.to_string(),
                org: "codex".to_string(),
            }
        );
        assert_eq!(usage.limits.len(), 2);

        let session = usage.session().expect("a session limit");
        assert_eq!(session.kind, LimitKind::Session);
        assert_eq!(session.percent, 12.0);
        assert_eq!(
            session.resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_794_693_195, 0).unwrap())
        );
        assert_eq!(session.severity, Severity::Normal);
        assert!(!session.is_active);

        let weekly = usage.weekly_all().expect("a weekly limit");
        assert_eq!(weekly.percent, 48.0);
        assert_eq!(
            weekly.resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_795_279_995, 0).unwrap())
        );
        assert!(weekly.is_active);
        assert!(!usage.extra_usage_enabled);
    }

    #[test]
    fn two_limits_at_the_same_percent_mark_the_first() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]["used_percent"] = json!(48);

        let (usage, _, _) = usage_from(&answer).unwrap();

        assert_eq!(usage.limits.len(), 2);
        assert!(
            usage.session().expect("a session limit").is_active,
            "the first of equals is the active one"
        );
        assert!(!usage.weekly_all().expect("a weekly limit").is_active);
        assert_eq!(
            usage.binding().map(|limit| &limit.kind),
            Some(&LimitKind::Session)
        );
    }

    #[test]
    fn a_window_of_another_length_keeps_its_length() {
        let mut answer = answer();
        answer["rate_limit"]["secondary_window"]["limit_window_seconds"] = json!(86_400);
        let (usage, _, _) = usage_from(&answer).unwrap();

        let weekly = usage
            .limits
            .iter()
            .find(|limit| limit.kind == LimitKind::Other("1d limit".to_string()))
            .expect("a 1d limit");
        assert_eq!(weekly.percent, 48.0);
        assert_eq!(
            weekly.resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_795_279_995, 0).unwrap())
        );
    }

    #[test]
    fn an_additional_limit_carries_its_name() {
        let mut answer = answer();
        answer["additional_rate_limits"] = json!([{
            "limit_name": "gpt-6-astra",
            "metered_feature": "codex",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 30,
                    "limit_window_seconds": 604800,
                    "reset_at": 1795279995
                }
            }
        }]);
        let (usage, _, _) = usage_from(&answer).unwrap();

        let scoped: Vec<_> = usage.weekly_scoped().collect();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].kind, LimitKind::WeeklyScoped);
        assert_eq!(scoped[0].model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(scoped[0].percent, 30.0);
        assert_eq!(
            scoped[0].resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_795_279_995, 0).unwrap())
        );
    }

    #[test]
    fn a_null_window_is_left_out() {
        let mut answer = answer();
        answer["rate_limit"]["secondary_window"] = Value::Null;
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert_eq!(usage.limits.len(), 1);
        assert!(usage.session().is_some());
        assert!(usage.weekly_all().is_none());
    }

    #[test]
    fn a_window_of_whole_minutes_says_minutes() {
        let mut answer = answer();
        answer["rate_limit"]["secondary_window"]["limit_window_seconds"] = json!(5_400);
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert!(usage
            .limits
            .iter()
            .any(|limit| limit.kind == LimitKind::Other("1h 30min limit".to_string())));
    }

    #[test]
    fn a_missing_used_percent_is_a_shape_error() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]
            .as_object_mut()
            .unwrap()
            .remove("used_percent");

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => {
                assert!(
                    text.contains("rate_limit.primary_window.used_percent"),
                    "{text}"
                );
            }
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn no_auth_file_is_no_login() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());

        match read(&paths, &closed_api()) {
            Err(ProviderError::NoLogin { path }) => assert_eq!(path, paths.auth_file),
            other => panic!("expected NoLogin, got {other:?}"),
        }
    }

    #[test]
    fn an_auth_file_without_tokens_is_a_keyring_login() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        std::fs::write(&paths.auth_file, r#"{"auth_mode":"chatgpt","tokens":null}"#).unwrap();

        match read(&paths, &closed_api()) {
            Err(ProviderError::KeyringLogin) => {}
            other => panic!("expected KeyringLogin, got {other:?}"),
        }
    }

    #[test]
    fn a_login_without_auth_mode_is_read_as_before() {
        let (_dir, paths) = with_auth(|auth| {
            auth.as_object_mut().unwrap().remove("auth_mode");
        });
        let (base, receiver, handle) = start_server(Reply::ok(USAGE_ANSWER));

        let reading = read(&paths, &api_for(&base)).unwrap();
        let request = captured(&receiver, handle);

        assert_eq!(reading.account.acc, ACCOUNT_ID);
        assert_eq!(reading.plan.as_deref(), Some("Plus"));
        assert_eq!(request.header("chatgpt-account-id"), Some(ACCOUNT_ID));
    }

    #[test]
    fn tokens_that_are_not_an_object_are_a_shape_error() {
        let (_dir, paths) = with_auth(|auth| auth["tokens"] = json!("not an object"));

        match read(&paths, &closed_api()) {
            Err(ProviderError::Shape(text)) => {
                assert_eq!(text, "auth.json's tokens is not an object")
            }
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn a_file_with_only_an_api_key_is_no_login() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        std::fs::write(
            &paths.auth_file,
            r#"{"OPENAI_API_KEY":"sk-proj-made-up-not-a-real-key"}"#,
        )
        .unwrap();

        match read(&paths, &closed_api()) {
            Err(ProviderError::NoLogin { path }) => assert_eq!(path, paths.auth_file),
            other => panic!("expected NoLogin, got {other:?}"),
        }
    }

    #[test]
    fn a_401_is_an_expired_login() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        write_auth(&paths);
        let (base, receiver, handle) = start_server(Reply::status(401, "unauthorized"));

        let error = read(&paths, &api_for(&base)).unwrap_err();
        let _ = captured(&receiver, handle);

        assert!(
            matches!(error, ProviderError::LoginExpired),
            "got {error:?}"
        );
    }

    #[test]
    fn the_request_carries_both_headers() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        write_auth(&paths);
        let (base, receiver, handle) = start_server(Reply::ok(USAGE_ANSWER));

        let reading = read(&paths, &api_for(&base)).unwrap();
        let request = captured(&receiver, handle);

        assert_eq!(reading.plan.as_deref(), Some("Plus"));
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/backend-api/wham/usage");
        let authorization = format!("Bearer {ACCESS_TOKEN}");
        assert_eq!(
            request.header("Authorization"),
            Some(authorization.as_str())
        );
        assert_eq!(request.header("chatgpt-account-id"), Some(ACCOUNT_ID));
        assert!(request
            .header("User-Agent")
            .is_some_and(|value| value.starts_with("seatswap/")));
    }

    #[test]
    fn the_token_never_appears_in_an_error() {
        let mut errors = Vec::new();

        let (_expired_dir, expired) = with_auth(|_| {});
        let (base, receiver, handle) = start_server(Reply::status(401, "unauthorized"));
        errors.push(read(&expired, &api_for(&base)).unwrap_err());
        let _ = captured(&receiver, handle);

        let (_server_dir, server) = with_auth(|_| {});
        let (base, receiver, handle) = start_server(Reply::status(500, "down"));
        errors.push(read(&server, &api_for(&base)).unwrap_err());
        let _ = captured(&receiver, handle);

        let (_shape_dir, shape) = with_auth(|_| {});
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]
            .as_object_mut()
            .unwrap()
            .remove("used_percent");
        let (base, receiver, handle) = start_server(Reply::ok(answer.to_string()));
        errors.push(read(&shape, &api_for(&base)).unwrap_err());
        let _ = captured(&receiver, handle);

        let (_bad_header_dir, bad_header) = with_auth(|auth| {
            auth["tokens"]["access_token"] = json!(format!("{ACCESS_TOKEN}\u{e9}"));
        });
        errors.push(read(&bad_header, &closed_api()).unwrap_err());

        assert_eq!(errors.len(), 4);
        for error in &errors {
            for text in [error.to_string(), format!("{error:?}")] {
                assert!(!text.contains(ACCESS_TOKEN), "{text}");
                assert!(!text.contains(REFRESH_TOKEN), "{text}");
                assert!(!text.contains(ID_TOKEN), "{text}");
            }
        }
    }

    #[test]
    fn an_unreadable_auth_file_is_an_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        std::fs::create_dir(&paths.auth_file).unwrap();

        match read(&paths, &closed_api()) {
            Err(ProviderError::Io { path, .. }) => assert_eq!(path, paths.auth_file),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn a_bad_header_character_does_not_leak_the_token() {
        let (_dir, paths) = with_auth(|auth| {
            auth["tokens"]["access_token"] = json!(format!("{ACCESS_TOKEN}\u{e9}"));
        });

        let error = read(&paths, &closed_api()).unwrap_err();
        assert!(
            matches!(error, ProviderError::Api(ApiError::Network(_))),
            "got {error:?}"
        );
        let text = error.to_string();
        let debug = format!("{error:?}");
        assert!(!text.contains(ACCESS_TOKEN), "{text}");
        assert!(!debug.contains(ACCESS_TOKEN), "{debug}");
    }

    #[test]
    fn an_empty_access_token_is_a_shape_error() {
        let (_dir, paths) = with_auth(|auth| auth["tokens"]["access_token"] = json!(""));
        let (base, receiver, handle) = no_request_server();

        match read(&paths, &api_for(&base)) {
            Err(ProviderError::Shape(text)) => assert!(text.contains("access_token"), "{text}"),
            other => panic!("expected Shape, got {other:?}"),
        }
        saw_no_request(&receiver, handle);
    }

    #[test]
    fn an_empty_account_id_is_a_shape_error() {
        let (_dir, paths) = with_auth(|auth| auth["tokens"]["account_id"] = json!(""));
        let (base, receiver, handle) = no_request_server();

        match read(&paths, &api_for(&base)) {
            Err(ProviderError::Shape(text)) => assert!(text.contains("account_id"), "{text}"),
            other => panic!("expected Shape, got {other:?}"),
        }
        saw_no_request(&receiver, handle);
    }

    #[test]
    fn an_empty_answer_account_id_is_a_shape_error() {
        let mut answer = answer();
        answer["account_id"] = json!("");

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => assert!(text.contains("account_id"), "{text}"),
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_or_negative_window_is_a_shape_error() {
        for seconds in [0, -3_600] {
            let mut answer = answer();
            answer["rate_limit"]["primary_window"]["limit_window_seconds"] = json!(seconds);

            match usage_from(&answer) {
                Err(ProviderError::Shape(text)) => assert!(
                    text.contains("rate_limit.primary_window.limit_window_seconds"),
                    "{text}"
                ),
                other => panic!("expected Shape for {seconds}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_negative_percent_is_a_shape_error() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]["used_percent"] = json!(-5);

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => {
                assert!(
                    text.contains("rate_limit.primary_window.used_percent"),
                    "{text}"
                )
            }
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn a_fractional_reset_at_says_whole_number() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]["reset_at"] = json!(1_794_693_195.5);

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => {
                assert!(text.contains("whole number"), "{text}");
            }
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn an_additional_5h_limit_is_not_the_account_session() {
        let mut answer = answer();
        answer["rate_limit"] = Value::Null;
        answer["additional_rate_limits"] = json!([{
            "limit_name": "gpt-6-astra",
            "metered_feature": "codex",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 30,
                    "limit_window_seconds": 18000,
                    "reset_at": 1794693195
                }
            }
        }]);
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert!(usage.session().is_none());
        assert!(usage.weekly_all().is_none());
        assert_eq!(usage.limits.len(), 1);
        assert_eq!(
            usage.limits[0].kind,
            LimitKind::Other("5h limit".to_string())
        );
        assert_eq!(usage.limits[0].model.as_deref(), Some("gpt-6-astra"));
    }

    #[test]
    fn an_additional_entry_with_a_null_rate_limit_is_left_out() {
        let mut answer = answer();
        answer["additional_rate_limits"] = json!([{
            "limit_name": "gpt-6-astra",
            "metered_feature": "codex",
            "rate_limit": null
        }]);
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert_eq!(usage.limits.len(), 2);
        assert!(usage.session().is_some());
        assert!(usage.weekly_all().is_some());
    }

    #[test]
    fn an_empty_plan_is_none() {
        let mut answer = answer();
        answer["plan_type"] = json!("");
        let (_, plan, _) = usage_from(&answer).unwrap();

        assert_eq!(plan, None);
    }

    #[test]
    fn the_error_texts_are_the_cards_words() {
        assert_eq!(
            ProviderError::NoLogin {
                path: std::path::PathBuf::from("/tmp/codex/auth.json"),
            }
            .to_string(),
            "No Codex login found in /tmp/codex/auth.json. If Codex keeps its login in the system keyring, seatswap cannot read it."
        );
        assert_eq!(
            ProviderError::KeyringLogin.to_string(),
            "Codex keeps its login in the system keyring. To switch Codex logins, set cli_auth_credentials_store = \"file\" in Codex's config.toml and sign in again."
        );
        assert_eq!(
            ProviderError::ApiKeyLogin.to_string(),
            "Codex is signed in with an API key. An API key is no subscription; seatswap neither saves nor switches it."
        );
        assert_eq!(
            ProviderError::NotSaved {
                acc: "6a1d4c8e-2f5b-4d3a-9e7c-1b0a5f2d8c64".to_string(),
            }
            .to_string(),
            "No saved Codex login for 6a1d4c8e-2f5b-4d3a-9e7c-1b0a5f2d8c64. Sign in with codex login once while seatswap runs."
        );
        assert_eq!(
            ProviderError::LoginExpired.to_string(),
            "Codex's login has expired. Run codex once to renew it."
        );
        assert_eq!(
            ProviderError::NotRunning {
                url: "http://127.0.0.1:11434".to_string(),
            }
            .to_string(),
            "Ollama is not running on http://127.0.0.1:11434."
        );
    }

    #[test]
    fn auth_json_is_not_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        write_auth(&paths);
        let before = std::fs::read(&paths.auth_file).unwrap();
        let before_mtime = std::fs::metadata(&paths.auth_file)
            .unwrap()
            .modified()
            .unwrap();

        let (base, receiver, handle) = start_server(Reply::ok(USAGE_ANSWER));
        read(&paths, &api_for(&base)).unwrap();
        let _ = captured(&receiver, handle);

        let after = std::fs::read(&paths.auth_file).unwrap();
        let after_mtime = std::fs::metadata(&paths.auth_file)
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(before_mtime, after_mtime);
    }

    // ---- Saved Codex logins (R8) ------------------------------------------
    //
    // The fixture `auth.json` is the real layout of `codex-research.md`: a
    // 36-character `account_id`, an id token of about 1,750 made-up characters,
    // an access token of about 1,740 and a 196-character refresh token, with
    // `oauth_provenance` naming Codex's own issuer.

    /// Account ids of the two ChatGPT logins the tests use (36 characters each).
    const OA: &str = "6a1d4c8e-2f5b-4d3a-9e7c-1b0a5f2d8c64";
    const OA2: &str = "c3e8f1a2-7b4d-4a69-8f2e-5d0c9b1a3e47";
    const SAM_CLIENT_ID: &str = "app_9f1c2d3e4b5a6c7d8e9f0a1b2c3d4e5f";

    /// A made-up token of a real length, in made-up characters only.
    fn made_up(prefix: &str, length: usize) -> String {
        let mut text = prefix.to_string();
        while text.len() < length {
            text.push_str("9f2c5d7e1b3a4c6d8e0f2a4b6c8d0e2f");
        }
        text.truncate(length);
        text
    }

    fn id_token() -> String {
        made_up(
            "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyLXNhbSJ9.",
            1750,
        )
    }

    fn access_token() -> String {
        made_up(
            "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJhY2NvdW50IjoiT0EifQ.",
            1740,
        )
    }

    fn refresh_token() -> String {
        made_up("rt_", 196)
    }

    /// The real `auth.json` shape for one account.
    fn login_value(account_id: &str) -> Value {
        json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": id_token(),
                "access_token": access_token(),
                "refresh_token": refresh_token(),
                "oauth_provenance": {
                    "issuer": "https://auth.openai.com",
                    "client_id": SAM_CLIENT_ID,
                },
                "login_owner_id": "user-sam",
                "account_id": account_id,
            },
            "last_refresh": "2026-11-17T18:44:10.152335833Z",
        })
    }

    /// The same login after a Codex renewal: three fresh tokens, a newer
    /// `last_refresh`, every other key as it was.
    fn renewed(mut login: Value, marker: char) -> Value {
        login["tokens"]["id_token"] = json!(made_up(&format!("id-{marker}-"), 1750));
        login["tokens"]["access_token"] = json!(made_up(&format!("access-{marker}-"), 1740));
        login["tokens"]["refresh_token"] = json!(made_up(&format!("refresh-{marker}-"), 196));
        login["last_refresh"] = json!("2026-11-18T09:10:11.123456789Z");
        login
    }

    fn codex(login: &Value) -> CodexLogin {
        CodexLogin(
            login
                .as_object()
                .expect("the fixture login is an object")
                .clone(),
        )
    }

    fn account(acc: &str) -> AccountRef {
        AccountRef {
            acc: acc.to_string(),
            org: CODEX_ORG.to_string(),
        }
    }

    /// A fake Codex folder, a `FileSecrets` and seatswap's data folder in
    /// tempdirs of their own, with the tempdirs kept alive.
    struct Fake {
        codex_folder: tempfile::TempDir,
        secret_folder: tempfile::TempDir,
        app_folder: tempfile::TempDir,
        secrets: FileSecrets,
    }

    impl Fake {
        fn new() -> Fake {
            let codex_folder = tempfile::tempdir().unwrap();
            let secret_folder = tempfile::tempdir().unwrap();
            let app_folder = tempfile::tempdir().unwrap();
            let secrets = FileSecrets::new(secret_folder.path().to_path_buf());
            Fake {
                codex_folder,
                secret_folder,
                app_folder,
                secrets,
            }
        }

        fn app(&self) -> AppPaths {
            AppPaths::at(self.app_folder.path())
        }

        fn paths(&self) -> CodexPaths {
            CodexPaths::at(self.codex_folder.path())
        }

        fn write_live(&self, login: &Value) {
            atomic::write_atomic(
                &self.paths().auth_file,
                login.to_string().as_bytes(),
                Access::Private,
            )
            .expect("write auth.json");
        }

        fn store(&self, account: &AccountRef, login: &Value) {
            self.secrets
                .set_codex(&secret_id(account), &codex(login))
                .expect("store a login");
        }

        fn secret_file(&self, id: &str) -> std::path::PathBuf {
            self.secret_folder.path().join(format!("{id}.json"))
        }
    }

    fn usage_answer(account_id: &str) -> String {
        json!({
            "user_id": "user-sam",
            "account_id": account_id,
            "email": "sam@mail.example",
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 12,
                    "limit_window_seconds": 18000,
                    "reset_at": 1794693195
                },
                "secondary_window": {
                    "used_percent": 48,
                    "limit_window_seconds": 604800,
                    "reset_at": 1795279995
                }
            },
            "additional_rate_limits": null,
        })
        .to_string()
    }

    fn token_answer(login: &Value) -> String {
        json!({
            "id_token": login["tokens"]["id_token"],
            "access_token": login["tokens"]["access_token"],
            "refresh_token": login["tokens"]["refresh_token"],
            "expires_in": 864000,
            "earliest_refresh_at": "2026-11-26T09:00:00Z",
        })
        .to_string()
    }

    fn api_at(base: &str) -> Api {
        Api::new()
            .with_codex_url(&format!("{base}/backend-api/wham/usage"))
            .with_codex_token_url(&format!("{base}/oauth/token"))
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_switch_saves_the_live_login_and_writes_the_target() {
        let fake = Fake::new();
        let oa = account(OA);
        let oa2 = account(OA2);
        // OA's stored copy is one renewal behind its live file.
        fake.store(&oa, &login_value(OA));
        let live = renewed(login_value(OA), 'L');
        fake.write_live(&live);
        // OA2's stored copy is what the switch must put in place.
        let target = login_value(OA2);
        fake.store(&oa2, &target);

        let switched = switch_to(&fake.app(), &fake.paths(), &fake.secrets, &oa2).unwrap();

        assert_eq!(switched.from, Some(oa.clone()));
        assert_eq!(switched.to, oa2.clone());
        // The live file is the target's stored object, key order included.
        let written = std::fs::read_to_string(&fake.paths().auth_file).unwrap();
        assert_eq!(written, target.to_string());
        // The login that was live is stored with its newest tokens.
        let stored = fake
            .secrets
            .get_codex(&secret_id(&oa))
            .unwrap()
            .expect("OA is saved");
        assert_eq!(Value::Object(stored.0).to_string(), live.to_string());
        // The id is the one FileSecrets accepts: 32 lower-case hex digits.
        let id = secret_id(&oa2);
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{id}"
        );
        #[cfg(unix)]
        assert_eq!(mode(&fake.paths().auth_file), 0o600);
    }

    /// R8: forgetting deletes seatswap's stored copy only; Codex's own
    /// `auth.json` stays byte for byte, another login's copy stays, and a copy
    /// that is already gone is not an error.
    #[test]
    fn forget_deletes_only_the_saved_copy() {
        let fake = Fake::new();
        let oa = account(OA);
        let oa2 = account(OA2);
        fake.write_live(&login_value(OA));
        fake.store(&oa, &login_value(OA));
        fake.store(&oa2, &login_value(OA2));
        let auth_before = std::fs::read(&fake.paths().auth_file).unwrap();

        forget(&fake.app(), &fake.secrets, &oa2).unwrap();

        assert!(
            fake.secrets.get_codex(&secret_id(&oa2)).unwrap().is_none(),
            "the forgotten copy is gone"
        );
        assert!(
            fake.secrets.get_codex(&secret_id(&oa)).unwrap().is_some(),
            "the other login's copy stays"
        );
        assert_eq!(
            std::fs::read(&fake.paths().auth_file).unwrap(),
            auth_before,
            "Codex's own auth.json is not touched"
        );

        // A copy that is not there is not an error.
        forget(&fake.app(), &fake.secrets, &oa2).unwrap();
    }

    #[test]
    fn a_switch_waits_for_codex_locks_and_gives_up_after_the_timeout() {
        let fake = Fake::new();
        let oa = account(OA);
        let oa2 = account(OA2);
        let live = login_value(OA);
        fake.write_live(&live);
        let target = login_value(OA2);
        fake.store(&oa2, &target);
        let before = std::fs::read(&fake.paths().auth_file).unwrap();

        // A held `auth.lock`: the switch waits the whole deadline and names it.
        let held = acquire_file_lock(&fake.paths().auth_lock, Duration::ZERO).unwrap();
        let started = Instant::now();
        let error = switch_to(&fake.app(), &fake.paths(), &fake.secrets, &oa2).unwrap_err();
        assert!(
            started.elapsed() >= Duration::from_secs(CODEX_LOCK_TIMEOUT_SECS),
            "the switch gave up after {:?}",
            started.elapsed()
        );
        match &error {
            ProviderError::Lock(LockError::Busy { path }) => {
                assert_eq!(path, &fake.paths().auth_lock)
            }
            other => panic!("expected a busy auth.lock, got {other:?}"),
        }
        let text = error.to_string();
        assert!(text.contains("auth.lock"), "{text}");
        assert_eq!(
            std::fs::read(&fake.paths().auth_file).unwrap(),
            before,
            "auth.json must not change when the lock stays busy"
        );
        assert!(
            fake.secrets.get_codex(&secret_id(&oa)).unwrap().is_none(),
            "nothing is saved before the locks are ours"
        );
        drop(held);

        // Released within the deadline: the switch waits for it and goes through.
        let held = acquire_file_lock(&fake.paths().auth_lock, Duration::ZERO).unwrap();
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let switched = switch_to(&fake.app(), &fake.paths(), &fake.secrets, &oa2).unwrap();
        releaser.join().unwrap();
        assert_eq!(switched.from, Some(oa));
        assert_eq!(
            std::fs::read_to_string(&fake.paths().auth_file).unwrap(),
            target.to_string()
        );
    }

    #[test]
    fn a_switch_writes_the_target_as_stored_after_the_locks() {
        let fake = Fake::new();
        let oa2 = account(OA2);
        fake.write_live(&login_value(OA));
        fake.store(&oa2, &login_value(OA2));
        let newer = renewed(login_value(OA2), 'T');

        // The switch waits on `auth.lock`; meanwhile the target's copy is
        // replaced. Reading the target only after the locks must write the newer one.
        let held = acquire_file_lock(&fake.paths().auth_lock, Duration::ZERO).unwrap();
        let app = fake.app();
        let paths = fake.paths();
        let secrets = FileSecrets::new(fake.secret_folder.path().to_path_buf());
        let target = oa2.clone();
        let handle = thread::spawn(move || switch_to(&app, &paths, &secrets, &target));
        thread::sleep(Duration::from_millis(300));
        fake.store(&oa2, &newer);
        drop(held);

        let switched = handle.join().unwrap().unwrap();
        assert_eq!(switched.from, Some(account(OA)));
        assert_eq!(switched.to, oa2);
        assert_eq!(
            std::fs::read_to_string(&fake.paths().auth_file).unwrap(),
            newer.to_string(),
            "the copy stored while the switch waited is the one written"
        );
    }

    #[test]
    fn a_held_app_lock_stops_a_switch_and_names_it() {
        let fake = Fake::new();
        let oa2 = account(OA2);
        fake.write_live(&login_value(OA));
        fake.store(&oa2, &login_value(OA2));
        let before = std::fs::read(&fake.paths().auth_file).unwrap();

        let held = acquire_app_lock(&fake.app(), Duration::ZERO).unwrap();
        let error = switch_to(&fake.app(), &fake.paths(), &fake.secrets, &oa2).unwrap_err();
        drop(held);

        match &error {
            ProviderError::Lock(LockError::Busy { path }) => {
                assert_eq!(path, &fake.app().switch_lock())
            }
            other => panic!("expected a busy app lock, got {other:?}"),
        }
        let text = error.to_string();
        assert!(text.contains("switch.lock"), "{text}");
        assert_eq!(
            std::fs::read(&fake.paths().auth_file).unwrap(),
            before,
            "auth.json must not change when the app lock stays busy"
        );
    }

    #[test]
    fn a_held_auth_json_lock_names_it_and_releases_auth_lock() {
        let fake = Fake::new();
        let oa2 = account(OA2);
        fake.write_live(&login_value(OA));
        fake.store(&oa2, &login_value(OA2));
        let before = std::fs::read(&fake.paths().auth_file).unwrap();

        let held = acquire_file_lock(&fake.paths().auth_json_lock, Duration::ZERO).unwrap();
        let started = Instant::now();
        let error = switch_to(&fake.app(), &fake.paths(), &fake.secrets, &oa2).unwrap_err();
        assert!(
            started.elapsed() >= Duration::from_secs(CODEX_LOCK_TIMEOUT_SECS),
            "the switch gave up after {:?}",
            started.elapsed()
        );
        drop(held);

        match &error {
            ProviderError::Lock(LockError::Busy { path }) => {
                assert_eq!(path, &fake.paths().auth_json_lock)
            }
            other => panic!("expected a busy auth.json.lock, got {other:?}"),
        }
        let text = error.to_string();
        assert!(text.contains("auth.json.lock"), "{text}");
        assert_eq!(
            std::fs::read(&fake.paths().auth_file).unwrap(),
            before,
            "auth.json must not change when a lock stays busy"
        );
        // The first Codex lock the failing switch took was released with it.
        let released = acquire_file_lock(&fake.paths().auth_lock, Duration::ZERO).unwrap();
        drop(released);
    }

    #[test]
    fn a_login_in_the_keyring_is_not_saved_and_says_how_to_change_it() {
        let fake = Fake::new();
        let oa2 = account(OA2);
        fake.store(&oa2, &login_value(OA2));
        let keyring = json!({"auth_mode": "chatgpt", "tokens": null});
        fake.write_live(&keyring);

        let error = sync_live(&fake.app(), &fake.paths(), &fake.secrets).unwrap_err();
        assert!(matches!(&error, ProviderError::KeyringLogin), "{error:?}");
        assert_eq!(
            error.to_string(),
            "Codex keeps its login in the system keyring. To switch Codex logins, set cli_auth_credentials_store = \"file\" in Codex's config.toml and sign in again."
        );

        // A switch stops with the same error and writes nothing at all.
        let error = switch_to(&fake.app(), &fake.paths(), &fake.secrets, &oa2).unwrap_err();
        assert!(matches!(&error, ProviderError::KeyringLogin), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(&fake.paths().auth_file).unwrap(),
            keyring.to_string()
        );
        assert!(fake
            .secrets
            .get_codex(&secret_id(&account(OA)))
            .unwrap()
            .is_none());
    }

    #[test]
    fn an_api_key_login_is_not_saved() {
        let fake = Fake::new();
        let key =
            json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-proj-made-up-not-a-real-key"});
        fake.write_live(&key);

        let error = sync_live(&fake.app(), &fake.paths(), &fake.secrets).unwrap_err();
        assert!(matches!(&error, ProviderError::ApiKeyLogin), "{error:?}");
        assert_eq!(
            error.to_string(),
            "Codex is signed in with an API key. An API key is no subscription; seatswap neither saves nor switches it."
        );
        let stored: Vec<_> = std::fs::read_dir(fake.secret_folder.path())
            .unwrap()
            .collect();
        assert!(stored.is_empty(), "an API key is never stored");
    }

    #[test]
    fn a_renewed_live_login_replaces_the_stored_copy() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let live = renewed(login_value(OA), 'R');
        fake.write_live(&live);

        let synced = sync_live(&fake.app(), &fake.paths(), &fake.secrets).unwrap();
        assert_eq!(synced.account, oa);
        assert!(synced.saved, "the renewed tokens replace the stored copy");
        let stored = fake
            .secrets
            .get_codex(&secret_id(&oa))
            .unwrap()
            .expect("OA is saved");
        let stored_text = Value::Object(stored.0).to_string();
        assert_eq!(stored_text, live.to_string());
        assert_ne!(stored_text, before.to_string());

        // A poll that sees the same tokens writes nothing a second time.
        let again = sync_live(&fake.app(), &fake.paths(), &fake.secrets).unwrap();
        assert!(!again.saved);
        assert_eq!(again.account, oa);
    }

    #[test]
    fn an_incomplete_live_login_is_not_saved() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let mut incomplete = login_value(OA);
        incomplete["tokens"]
            .as_object_mut()
            .unwrap()
            .remove("refresh_token");
        fake.write_live(&incomplete);

        match sync_live(&fake.app(), &fake.paths(), &fake.secrets) {
            Err(ProviderError::Shape(text)) => {
                assert_eq!(text, "the Codex login lacks refresh_token")
            }
            other => panic!("expected Shape, got {other:?}"),
        }
        let stored = fake
            .secrets
            .get_codex(&secret_id(&oa))
            .unwrap()
            .expect("OA is still saved");
        assert_eq!(
            Value::Object(stored.0).to_string(),
            before.to_string(),
            "the good stored copy stays"
        );
    }

    #[test]
    fn an_inactive_login_is_renewed_once_after_a_401_and_stored() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let old_access = before["tokens"]["access_token"]
            .as_str()
            .unwrap()
            .to_string();
        let old_refresh = before["tokens"]["refresh_token"]
            .as_str()
            .unwrap()
            .to_string();
        let fresh = renewed(login_value(OA), 'N');

        let (base, receiver, handle) = start_script(vec![
            Reply::status(401, "unauthorized"),
            Reply::ok(token_answer(&fresh)),
            Reply::ok(usage_answer(OA)),
        ]);
        let reading = read_saved(
            &fake.app(),
            &fake.paths(),
            &fake.secrets,
            &oa,
            &api_at(&base),
        )
        .unwrap();
        let first = received(&receiver);
        let second = received(&receiver);
        let third = received(&receiver);
        handle.join().unwrap();

        assert_eq!(reading.account, oa);
        assert_eq!(reading.plan.as_deref(), Some("Plus"));
        // The first ask used the stored token.
        let expected = format!("Bearer {old_access}");
        assert_eq!(first.method, "GET");
        assert_eq!(first.header("Authorization"), Some(expected.as_str()));
        // The renewal went to the token URL with Codex's own client id.
        assert_eq!(second.method, "POST");
        assert_eq!(second.path, "/oauth/token");
        let sent: Value = serde_json::from_str(&second.body).expect("the token request is JSON");
        assert_eq!(sent["client_id"], json!(SAM_CLIENT_ID));
        assert_eq!(sent["grant_type"], json!("refresh_token"));
        assert_eq!(sent["refresh_token"], json!(old_refresh));
        assert_eq!(sent["scope"], json!("openid profile email"));
        // The second ask used the renewed token.
        let new_access = fresh["tokens"]["access_token"].as_str().unwrap();
        let expected = format!("Bearer {new_access}");
        assert_eq!(third.header("Authorization"), Some(expected.as_str()));

        // The renewed tokens are stored in their places, every other key kept.
        let stored = fake
            .secrets
            .get_codex(&secret_id(&oa))
            .unwrap()
            .expect("OA is saved");
        let tokens = stored.tokens().expect("the stored copy has tokens");
        for key in ["id_token", "access_token", "refresh_token"] {
            assert_eq!(tokens[key], fresh["tokens"][key], "{key}");
        }
        assert_eq!(tokens["login_owner_id"], json!("user-sam"));
        assert_eq!(stored.0["auth_mode"], json!("chatgpt"));
        let last_refresh = stored.0["last_refresh"].as_str().unwrap();
        assert_ne!(last_refresh, before["last_refresh"].as_str().unwrap());
        assert!(last_refresh.ends_with('Z'), "{last_refresh}");
        assert_eq!(last_refresh.len(), "2026-11-17T18:44:10.152335833Z".len());
        assert!(DateTime::parse_from_rfc3339(last_refresh).is_ok());
    }

    #[test]
    fn a_renewal_does_not_overwrite_a_newer_copy() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let old_access = before["tokens"]["access_token"]
            .as_str()
            .unwrap()
            .to_string();
        let newer = renewed(login_value(OA), 'W');
        let fresh = renewed(login_value(OA), 'N');

        // The fake server saves a newer copy while the renewal answer is in
        // flight: the store write must keep it, and the second ask must use it.
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let secret_folder = fake.secret_folder.path().to_path_buf();
        let id = secret_id(&oa);
        let newer_copy = newer.clone();
        let fresh_answer = token_answer(&fresh);
        let handle = thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let first = capture(&mut request);
            request
                .respond(Response::from_string("unauthorized").with_status_code(StatusCode(401)))
                .unwrap();

            let mut request = server.recv().unwrap();
            let second = capture(&mut request);
            FileSecrets::new(secret_folder)
                .set_codex(&id, &codex(&newer_copy))
                .unwrap();
            request
                .respond(Response::from_string(fresh_answer).with_status_code(StatusCode(200)))
                .unwrap();

            let mut request = server.recv().unwrap();
            let third = capture(&mut request);
            request
                .respond(Response::from_string(usage_answer(OA)).with_status_code(StatusCode(200)))
                .unwrap();
            (first, second, third)
        });

        let reading = read_saved(
            &fake.app(),
            &fake.paths(),
            &fake.secrets,
            &oa,
            &api_at(&base),
        )
        .unwrap();
        let (first, second, third) = handle.join().unwrap();

        assert_eq!(reading.account, oa);
        assert_eq!(reading.plan.as_deref(), Some("Plus"));
        let expected = format!("Bearer {old_access}");
        assert_eq!(first.header("Authorization"), Some(expected.as_str()));
        assert_eq!(second.path, "/oauth/token");
        let newer_access = newer["tokens"]["access_token"].as_str().unwrap();
        let expected = format!("Bearer {newer_access}");
        assert_eq!(
            third.header("Authorization"),
            Some(expected.as_str()),
            "the second ask uses the newer stored copy's token"
        );
        let stored = fake
            .secrets
            .get_codex(&secret_id(&oa))
            .unwrap()
            .expect("OA is saved");
        let stored_text = Value::Object(stored.0).to_string();
        assert_eq!(
            stored_text,
            newer.to_string(),
            "the renewal does not overwrite the newer copy"
        );
        assert_ne!(
            stored_text,
            fresh.to_string(),
            "the renewed copy was not written"
        );
    }

    #[test]
    fn a_failed_renewal_keeps_the_copy_and_says_sign_in_again() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let id = secret_id(&oa);
        let stored_before = std::fs::read(fake.secret_file(&id)).unwrap();

        let (base, receiver, handle) = start_script(vec![
            Reply::status(401, "unauthorized"),
            Reply::status(
                400,
                json!({
                    "error": "invalid_grant",
                    "error_description": "the refresh token is no longer valid",
                })
                .to_string(),
            ),
        ]);
        let error = read_saved(
            &fake.app(),
            &fake.paths(),
            &fake.secrets,
            &oa,
            &api_at(&base),
        )
        .unwrap_err();
        let _ = received(&receiver);
        let _ = received(&receiver);
        handle.join().unwrap();

        assert_eq!(error.to_string(), "Sign in again with codex login.");
        assert!(
            matches!(&error, ProviderError::RenewFailed(text) if text == "Sign in again with codex login."),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read(fake.secret_file(&id)).unwrap(),
            stored_before,
            "the stored copy is kept unchanged"
        );
        let refresh = before["tokens"]["refresh_token"].as_str().unwrap();
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains(refresh), "a token in an error text: {text}");
        }
    }

    #[test]
    fn a_second_401_after_a_renewal_keeps_the_renewed_copy() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let fresh = renewed(login_value(OA), 'S');

        let (base, receiver, handle) = start_script(vec![
            Reply::status(401, "unauthorized"),
            Reply::ok(token_answer(&fresh)),
            Reply::status(401, "unauthorized"),
        ]);
        let error = read_saved(
            &fake.app(),
            &fake.paths(),
            &fake.secrets,
            &oa,
            &api_at(&base),
        )
        .unwrap_err();
        let _ = received(&receiver);
        let _ = received(&receiver);
        let _ = received(&receiver);
        handle.join().unwrap();

        assert!(
            matches!(&error, ProviderError::RenewFailed(text) if text == "Sign in again with codex login."),
            "{error:?}"
        );
        let stored = fake
            .secrets
            .get_codex(&secret_id(&oa))
            .unwrap()
            .expect("OA is saved");
        let tokens = stored.tokens().expect("the stored copy has tokens");
        for key in ["id_token", "access_token", "refresh_token"] {
            assert_eq!(tokens[key], fresh["tokens"][key], "{key}");
        }
        assert_ne!(
            stored.0["last_refresh"], before["last_refresh"],
            "the renewed copy, not the old one, is stored"
        );
    }

    /// R8: a poll reads which login is live at its start; a switch during the
    /// round must not let a 401 renew the login that is live by then. The live
    /// file is read again under the app lock, before the token POST.
    #[test]
    fn a_login_that_became_live_is_not_renewed() {
        let fake = Fake::new();
        let oa = account(OA);
        let login = login_value(OA);
        fake.store(&oa, &login);
        // The login became Codex's live login while the round ran.
        fake.write_live(&login);
        let id = secret_id(&oa);
        let stored_before = std::fs::read(fake.secret_file(&id)).unwrap();

        let (base, receiver, handle) = start_script(vec![Reply::status(401, "unauthorized")]);
        let (token_base, token_receiver, token_handle) = no_request_server();
        let api = Api::new()
            .with_codex_url(&format!("{base}/backend-api/wham/usage"))
            .with_codex_token_url(&format!("{token_base}/oauth/token"));

        let error = read_saved(&fake.app(), &fake.paths(), &fake.secrets, &oa, &api).unwrap_err();
        let _ = received(&receiver);
        handle.join().unwrap();
        saw_no_request(&token_receiver, token_handle);

        assert!(matches!(&error, ProviderError::BecameLive), "{error:?}");
        assert_eq!(
            std::fs::read(fake.secret_file(&id)).unwrap(),
            stored_before,
            "the copy that became live is not renewed and not stored again"
        );
    }

    #[test]
    fn an_empty_renewal_answer_is_a_failed_renewal() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let id = secret_id(&oa);
        let stored_before = std::fs::read(fake.secret_file(&id)).unwrap();
        let mut answer: Value = serde_json::from_str(&token_answer(&before)).unwrap();
        answer["access_token"] = json!("");

        let (base, receiver, handle) = start_script(vec![
            Reply::status(401, "unauthorized"),
            Reply::ok(answer.to_string()),
        ]);
        let error = read_saved(
            &fake.app(),
            &fake.paths(),
            &fake.secrets,
            &oa,
            &api_at(&base),
        )
        .unwrap_err();
        let _ = received(&receiver);
        let _ = received(&receiver);
        handle.join().unwrap();

        assert!(
            matches!(&error, ProviderError::RenewFailed(text) if text == "Sign in again with codex login."),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read(fake.secret_file(&id)).unwrap(),
            stored_before,
            "an answer that renews nothing stores nothing"
        );
    }

    #[test]
    fn a_server_error_at_renewal_is_not_sign_in_again() {
        let fake = Fake::new();
        let oa = account(OA);
        let before = login_value(OA);
        fake.store(&oa, &before);
        let id = secret_id(&oa);
        let stored_before = std::fs::read(fake.secret_file(&id)).unwrap();

        let (base, receiver, handle) = start_script(vec![
            Reply::status(401, "unauthorized"),
            Reply::status(500, "down"),
        ]);
        let error = read_saved(
            &fake.app(),
            &fake.paths(),
            &fake.secrets,
            &oa,
            &api_at(&base),
        )
        .unwrap_err();
        let _ = received(&receiver);
        let _ = received(&receiver);
        handle.join().unwrap();

        assert!(
            matches!(&error, ProviderError::Api(ApiError::Server(500))),
            "a 500 at renewal leaves the login possibly good, got {error:?}"
        );
        assert_eq!(
            std::fs::read(fake.secret_file(&id)).unwrap(),
            stored_before,
            "the stored copy is kept unchanged"
        );
    }

    #[test]
    fn a_token_is_never_sent_to_another_issuer() {
        let fake = Fake::new();
        let oa = account(OA);
        let mut other = login_value(OA);
        other["tokens"]["oauth_provenance"]["issuer"] =
            json!("https://auth.example-made-up.invalid");
        fake.store(&oa, &other);
        let refresh = other["tokens"]["refresh_token"].as_str().unwrap();

        let (usage_base, usage_receiver, usage_handle) =
            start_script(vec![Reply::status(401, "unauthorized")]);
        let (token_base, token_receiver, token_handle) = no_request_server();
        let api = Api::new()
            .with_codex_url(&format!("{usage_base}/backend-api/wham/usage"))
            .with_codex_token_url(&format!("{token_base}/oauth/token"));

        let error = read_saved(&fake.app(), &fake.paths(), &fake.secrets, &oa, &api).unwrap_err();
        let _ = received(&usage_receiver);
        usage_handle.join().unwrap();
        saw_no_request(&token_receiver, token_handle);

        assert!(
            matches!(&error, ProviderError::RenewFailed(text) if text == "the login names another issuer"),
            "{error:?}"
        );
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains(refresh), "a token in an error text: {text}");
        }
    }
}
