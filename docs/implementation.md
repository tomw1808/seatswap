# seatswap — implementation notes

The spec (`docs/spec.md`) says what the program does and why. This file fixes the module contracts, so slices built in parallel fit together. Signatures here are binding; bodies are free. Section numbers like §6.2 refer to the spec.

Done already (do not rewrite): `types.rs` (`Login`, `OAuthAccount`, `LOGIN_KEYS`), `paths.rs` (`ClaudePaths`, `AppPaths`), `atomic.rs` (`write_atomic`, `backup_once`).

Linux and macOS share the Unix code. The Windows port and its contracts are in `docs/windows.md`; the macOS Keychain is later work.

---

## Real-shaped test data

`.credentials.json` as Claude Code writes it: one line, no trailing newline, mode 0600.

```json
{"mcpOAuth":{"plugin:engineering:linear|5f1c2a9be0d34c11":{"serverName":"plugin:engineering:linear","accessToken":"lin_oauth_8d2f…(64 chars)","expiresAt":1794400000000}},"claudeAiOauth":{"accessToken":"sk-ant-oat01-<95 random chars>","refreshToken":"sk-ant-ort01-<95 random chars>","expiresAt":1794356040000,"refreshTokenExpiresAt":1796772840000,"scopes":["user:inference","user:profile","user:file_upload","user:mcp_servers","user:plugins","user:sessions:claude_code"],"subscriptionType":"team","subscriptionTypeSource":"profile","rateLimitTier":"default_claude_max_5x"},"organizationUuid":"e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62"}
```

`.claude.json`: pretty-printed with 2 spaces, no trailing newline, mode 0600, ~100 KB, many keys. The part we touch:

```json
{
  "numStartups": 41,
  "oauthAccount": {
    "accountUuid": "4b7e2f91-3c5a-4d8e-9f1b-6a2c8e4d7b30",
    "emailAddress": "someone@example.com",
    "organizationUuid": "e2a6c9d1-7f4b-4a3e-8c5d-1b9f0a7e3c62",
    "organizationName": "Acme",
    "organizationRole": "admin",
    "billingType": "stripe_subscription",
    "hasExtraUsageEnabled": false
  },
  "projects": { "/home/u/x": { "allowedTools": [] } }
}
```

Usage reply (`GET /api/oauth/usage`), trimmed:

```json
{
  "five_hour":  { "utilization": 97.0, "resets_at": "2026-11-10T13:40:00.102645+00:00" },
  "seven_day":  { "utilization": 48.0, "resets_at": "2026-11-16T01:00:00+00:00" },
  "limits": [
    { "kind": "session", "group": "session", "percent": 97, "severity": "critical",
      "resets_at": "2026-11-10T13:40:00+00:00", "scope": null, "is_active": true },
    { "kind": "weekly_all", "group": "weekly", "percent": 48, "severity": "normal",
      "resets_at": "2026-11-16T01:00:00+00:00", "scope": null, "is_active": false },
    { "kind": "weekly_scoped", "group": "weekly", "percent": 43, "severity": "normal",
      "resets_at": "2026-11-16T01:00:00+00:00",
      "scope": { "model": { "display_name": "Fable" } }, "is_active": false }
  ],
  "extra_usage": { "is_enabled": false },
  "some_code_named_field": null
}
```

A personal account's reply can have `"five_hour": null` and no `session` entry in `limits`.

Refresh reply (`POST https://platform.claude.com/v1/oauth/token`):

```json
{ "token_type": "Bearer", "access_token": "sk-ant-oat01-…", "expires_in": 28800,
  "refresh_token": "sk-ant-ort01-…", "refresh_token_expires_in": 2374560,
  "scope": "user:inference user:profile user:file_upload user:mcp_servers user:plugins user:sessions:claude_code",
  "token_uuid": "…", "account": { "uuid": "4b7e2f91-…", "email_address": "someone@example.com" },
  "organization": { "uuid": "e2a6c9d1-…" } }
```

Errors seen: `400 {"error":"invalid_grant","error_description":"Refresh token not found or invalid"}`; `401` on usage with a dead access token; `403` with body `error code: 1010` when the User-Agent is a library default.

---

## atomic.rs — files, modes and private folders

The one way seatswap writes a file, and the only module that knows about file modes. No other module imports `std::os::unix` outside tests.

```rust
/// Who may read a file seatswap creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access { Private /* Unix 0600 */, Shared /* Unix 0644 */ }

/// Temp file in the same folder, write, fsync, rename over `path`, fsync the folder (Unix).
/// Unix: an existing file keeps its mode, a new one gets the mode of `access_if_new`.
/// Windows: no modes (a file gets its folder's permissions), no folder fsync (Windows cannot
/// open a folder as a file), and the rename is retried while it fails with OS error 5 or 32
/// (another program holds the target open without delete sharing): 10 ms, doubling up to
/// 200 ms, for at most 2 s, then the last OS error. Any error removes the temp file.
pub fn write_atomic(path: &Path, bytes: &[u8], access_if_new: Access) -> io::Result<()>;

/// Append `line` plus '\n' as one write to `path` (created with `access_if_new` when
/// missing), then fsync. `line` must not contain '\n' and, with the newline, must fit
/// in MAX_LINE_BYTES: otherwise io::ErrorKind::InvalidInput.
pub const MAX_LINE_BYTES: usize = 4096;
pub fn append_line(path: &Path, line: &str, access_if_new: Access) -> io::Result<()>;
/// Append every line plus '\n' as one write, after checking every line; a rejected line leaves the
/// file unchanged. `append_line` is this with one line.
pub fn append_lines<S: AsRef<str>>(path: &Path, lines: &[S], access_if_new: Access) -> io::Result<()>;

/// Copy `path` to `<path>.seatswap-backup` (Access::Private) unless that backup exists.
pub fn backup_once(path: &Path) -> io::Result<bool>;

/// Missing: create with its parents (Unix: mode 0700, set explicitly). Existing folder: left
/// as it is. Something else in its place: io::ErrorKind::AlreadyExists, "exists but is not a folder".
pub fn ensure_private_dir(dir: &Path) -> io::Result<()>;

/// Unix: chmod 0700. Windows: nothing to set, Ok(()).
pub fn restrict_dir(dir: &Path) -> io::Result<()>;

/// Read + write, create when missing, never truncate; a new file is private (Unix: 0600).
pub fn open_private(path: &Path) -> io::Result<File>;
```

Every file seatswap writes passes `Access::Private`, except the XDG autostart entry (`Access::Shared`). The data folder (store, usage cache, lock files) is made with `ensure_private_dir`; the fallback secrets folder also gets `restrict_dir`.

## locks.rs (§2.6, §6.2)

Claude Code's locks are folders made with `mkdir`. The holder keeps the folder's modification time fresh; others treat a lock older than its limit as abandoned.

```rust
pub const HEARTBEAT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct LockSpec { pub path: PathBuf, pub stale_after: Duration }

/// A held lock folder. A background thread sets its mtime to now every HEARTBEAT.
/// Drop stops the thread and removes the folder (a folder already gone is fine).
pub struct DirLock { /* … */ }
impl DirLock { pub fn path(&self) -> &Path; }

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("{path} is held by another process")] Busy { path: PathBuf },
    #[error("lock {path}: {source}")] Io { path: PathBuf, #[source] source: std::io::Error },
}

/// mkdir the folder. If it exists: when its mtime is older than stale_after, remove it
/// (rmdir) and try again; otherwise sleep ~50 ms and try again until `deadline`, then Busy.
/// The parent folder must exist (no create_dir_all: a missing config folder is an Io error).
pub fn acquire_dir_lock(spec: &LockSpec, deadline: Instant) -> Result<DirLock, LockError>;

/// The six specs in the order Claude Code takes them: the renewal locks first, because
/// Claude Code saves a renewed login inside its renewal lock:
/// refresh_lock 60 s, legacy_refresh_lock 60 s, design_refresh_lock 60 s (designOauth),
/// cloud_sessions_refresh_lock 60 s (cloudSessionsOauth), storage_write_lock 15 s,
/// identity_lock 10 s.
pub fn claude_lock_specs(paths: &ClaudePaths) -> [LockSpec; 6];

/// All six, in that order, sharing one deadline (now + timeout). On failure the ones
/// already taken are released before returning the error. Dropping ClaudeLocks releases
/// them in reverse order.
pub struct ClaudeLocks { /* Vec<DirLock> */ }
pub fn acquire_claude_locks(paths: &ClaudePaths, timeout: Duration) -> Result<ClaudeLocks, LockError>;

/// True when any of the six exists and is not stale. The watcher reads only when false.
pub fn claude_locks_busy(paths: &ClaudePaths) -> bool;

/// seatswap's own exclusive lock on <data_dir>/switch.lock (fs4 file lock, the file
/// is created if missing with atomic::open_private, the data folder too with
/// atomic::ensure_private_dir). Poll try_lock until timeout, then Busy.
pub struct AppLock { /* File */ }
pub fn acquire_app_lock(app: &AppPaths, timeout: Duration) -> Result<AppLock, LockError>;
```

Never write `.oauth_refresh.lock.owner`.

## live.rs (§2.1, §2.3, §6.2)

Reads and writes the live login and identity. **The caller holds `ClaudeLocks` for every write**; live.rs does not lock.

```rust
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    #[error("{path} is not valid JSON; not touching it")] Corrupt { path: PathBuf },
    #[error("{path} has no JSON object at the top level")] NotObject { path: PathBuf },
    #[error("{path} does not exist; run Claude Code once first")] Missing { path: PathBuf },
    #[error("login holds a key that is not a login key: {key}")] ForeignKey { key: String },
    #[error("{path}: {source}")] Io { path: PathBuf, #[source] source: std::io::Error },
}

/// The live login (the LOGIN_KEYS of .credentials.json). Ok(None) when the file is missing
/// or has no claudeAiOauth.
pub fn read_login(paths: &ClaudePaths) -> Result<Option<Login>, LiveError>;

/// Replace the login in .credentials.json: read the whole file (missing → empty object),
/// remove every LOGIN_KEYS key, insert the login's keys (None = log out: only remove),
/// keep every other key in place and order, write compact JSON without a trailing newline,
/// via atomic::write_atomic (Access::Private if new). Call atomic::backup_once first.
pub fn write_login(paths: &ClaudePaths, login: Option<&Login>) -> Result<(), LiveError>;

/// oauthAccount of .claude.json. Ok(None) when the file or the key is missing.
pub fn read_identity(paths: &ClaudePaths) -> Result<Option<OAuthAccount>, LiveError>;

/// Replace oauthAccount only; the file must exist (Missing otherwise). Pretty JSON with
/// 2-space indent, no trailing newline, other keys in place and order. backup_once first.
pub fn write_identity(paths: &ClaudePaths, account: &OAuthAccount) -> Result<(), LiveError>;
```

## store.rs (§5, §8.2, §10)

```rust
#[derive(Debug, thiserror::Error)] pub enum StoreError { /* Io, Corrupt{path}, UnsupportedVersion(u32),
    InvalidAbbr(String), DuplicateAbbr(String), InvalidColor(String), InvalidLabel(String),
    UnknownAccount(String), DuplicateAccount{email}, Keyring(String), InvalidSetting(String),
    InvalidId(String), MissingIdentity */ }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,                 // uuid v4, simple form
    pub account_uuid: String,
    pub organization_uuid: String,
    pub label: String,              // free text, 1..=40 chars after trim
    pub abbr: String,               // 1–2 chars, A–Z 0–9, upper case, unique
    pub color: String,              // "#rrggbb", lower case
    pub email: Option<String>,
    pub org_name: Option<String>,
    pub plan: Option<String>,       // plan_label(), e.g. "Ultra", "Team"
    pub oauth_account: OAuthAccount,
    pub added_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub login_state: LoginState,
    pub login_ends_at_ms: Option<i64>,  // refreshTokenExpiresAt of the stored login
    pub auto_burn: bool,                // "Auto-burn may use this account", default true
}

#[derive(… Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub enum LoginState { Ok, LoggedOut }

/// Settings §10 (no clearOrgCaches). Defaults: tray_text Percent, icon_limit Binding,
/// poll_active_secs 120, poll_inactive_secs 900, auto_burn false, suggest_switches true,
/// notify_on_switch true, mirror_live true, refresh_inactive true, hotkey "Ctrl+Alt+C",
/// start_at_login true, theme System, claude_config_dir None, history_dir None,
/// host_name None, read_sessions true, notify_usable_again true, notify_waiting true,
/// notify_waiting_hosts ThisHost, agents_read true, agents_switch false,
/// statusline_feed true, statusline StatuslineConfig::current_line().
/// `#[serde(default)]` per field so an older file without a new setting still loads; the
/// removed suggest_at and suggest_target_below are unknown keys and are ignored.
pub struct Settings { … }  // camelCase in the file
// step A's fields:
pub history_dir: Option<PathBuf>,       // None = <data>/history; a relative path is refused
pub host_name: Option<String>,          // None = the machine's host name; validated by paths::host_name
pub read_sessions: bool,
pub notify_usable_again: bool,
pub notify_waiting: bool,
pub notify_waiting_hosts: WaitingHosts,
pub agents_read: bool,
pub agents_switch: bool,
// step B's fields:
pub auto_burn: bool,                    // default false; on: switch by itself
pub suggest_switches: bool,             // default true; used only while auto_burn is off
pub statusline_feed: bool,              // default true; off: statusline writes no feed.json
pub statusline: StatuslineConfig,       // default current_line(); see "statusline — layout types"
#[serde(rename_all = "camelCase")] pub enum WaitingHosts { ThisHost, AllHosts }
pub enum TrayText { Percent, PercentReset, Off }
pub enum IconLimit { Binding, Session, Week }
pub enum Theme { System, Light, Dark }
impl Settings { pub fn validate(&self) -> Result<(), StoreError>; } // percents 1..=100, polls >= 30 s,
// an absolute claudeConfigDir, an absolute historyDir, a hostName that passes paths::host_name,
// and validate_statusline (below)

pub struct AccountsFile { pub version: u32, pub accounts: Vec<Account>, pub settings: Settings,
                          #[serde(default)] pub secrets_backend: Option<SecretsBackend> }
// version must be 1; anything else is UnsupportedVersion, never silently upgraded.

pub trait SecretStore: Send + Sync {
    fn get(&self, id: &str) -> Result<Option<Login>, StoreError>;
    fn set(&self, id: &str, login: &Login) -> Result<(), StoreError>;
    fn delete(&self, id: &str) -> Result<(), StoreError>;   // missing is fine
    fn encrypted(&self) -> bool;                             // false → UI warns (§5)
}
pub struct KeyringSecrets;            // keyring crate, service "seatswap", user = account id, JSON value
pub struct FileSecrets { dir: PathBuf } // <data>/secrets/<id>.json; folder 0700, files 0600, via write_atomic
impl FileSecrets { pub fn new(dir: PathBuf) -> FileSecrets; }
/// Where the logins are kept. Chosen once, on first use, and recorded in accounts.json, so the
/// logins never end up split between two places.
#[derive(… Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub enum SecretsBackend { Keyring, File }

/// recorded = AccountsFile.secrets_backend.
///   Some(Keyring) → KeyringSecrets; if the keyring does not answer (probe fails) → Err(Keyring(
///                   "the OS keyring is not available: <reason>; the logins are stored there")).
///                   Never falls back to files: that would hide the stored logins.
///   Some(File)    → FileSecrets at app.secrets_dir(), no probe.
///   None (first use) → probe (set/get/delete of "seatswap-probe"); OK → Keyring, else File.
/// Returns the store, the backend to record, and a warning when the backend is File
/// ("logins are stored unencrypted in <dir>") — shown by the UI every time (§5).
pub fn open_secrets(app: &AppPaths, recorded: Option<SecretsBackend>)
    -> Result<(Box<dyn SecretStore>, SecretsBackend, Option<String>), StoreError>;

pub struct Store { app: AppPaths, secrets: Box<dyn SecretStore> }
impl Store {
    pub fn new(app: AppPaths, secrets: Box<dyn SecretStore>) -> Store;
    pub fn secrets(&self) -> &dyn SecretStore;
    pub fn app_paths(&self) -> &AppPaths;
    /// Missing file → empty list + default settings. Corrupt → error (never overwritten).
    pub fn load(&self) -> Result<AccountsFile, StoreError>;
    /// Pretty JSON, write_atomic, 0600; creates the data folder (0700) if missing.
    pub fn save(&self, file: &AccountsFile) -> Result<(), StoreError>;
}

/// The one way to open the store (CLI and GUI): load accounts.json, open the secrets for the
/// recorded backend, record the backend on first use (save), return the store and the warning
/// of open_secrets.
pub fn open_store(app: AppPaths) -> Result<(Store, Option<String>), StoreError>;

impl AccountsFile {
    pub fn find(&self, id: &str) -> Option<&Account>;
    pub fn find_by_identity(&self, account_uuid: &str, organization_uuid: &str) -> Option<&Account>;
    /// Look up by id or by abbreviation (case-insensitive) — what the CLI accepts.
    pub fn find_by_key(&self, key: &str) -> Option<&Account>;
}

pub const PALETTE: [&str; 10] = ["#3b82f6", "#f59e0b", "#10b981", "#ef4444", "#8b5cf6",
                                 "#ec4899", "#14b8a6", "#f97316", "#6366f1", "#84cc16"];
/// "Acme Team" → "AT", "Acme" → "AC", "Private" → "PR", "x" → "X".
/// Letters/digits only; words split on whitespace and punctuation. No letter or digit → InvalidLabel.
pub fn default_abbr(label: &str) -> Result<String, StoreError>;
/// Trim, upper-case, 1–2 of A–Z0–9, not used by another account (`others` = their abbrs).
pub fn validate_abbr(abbr: &str, others: &[&str]) -> Result<String, StoreError>;
/// "#rrggbb" (any case) → lower case; anything else InvalidColor.
pub fn validate_color(color: &str) -> Result<String, StoreError>;
/// The identity's planDisplayName ("Free", "Pro", "Max", "Ultra"), else the login's
/// subscriptionType capitalised ("Team"). Never the rate-limit tier ("max 20x").
/// Recomputed whenever a login is stored for the account, through Account::note_stored_login.
pub fn plan_label(login: &Login, identity: &OAuthAccount) -> Option<String>;
impl Account {
    /// After a login was stored for this account (switch, sync_live, the engine's refresh):
    /// sets plan (plan_label), login_ends_at_ms and login_state = Ok; true if anything changed.
    /// The only place these three fields are written for an existing account.
    pub fn note_stored_login(&mut self, login: &Login) -> bool;
}
/// A new Account for a login + identity. Label: org name, else email; abbr: default_abbr made
/// unique by trying the label's first letter + each next letter/digit; colour: first PALETTE
/// entry not used. Identity missing accountUuid/organizationUuid → error. Already stored → DuplicateAccount.
pub fn new_account(login: &Login, identity: &OAuthAccount, label: Option<&str>, file: &AccountsFile,
                   now: DateTime<Utc>) -> Result<Account, StoreError>;
```

## api.rs (§3, §7.2, §7.3)

All HTTP lives here. `ureq` 2, timeout 20 s, header `User-Agent: seatswap/<CARGO_PKG_VERSION>` on every request.

```rust
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    Unauthorized,                       // 401
    InvalidGrant(String),               // 400 error=invalid_grant; the description
    OwnerMismatch { expected: (String, String), got: (String, String) },
    RateLimited { retry_after_secs: Option<u64> },  // 429
    Server(u16),                        // 5xx
    Http { status: u16, body: String },  // any other status; body cut to 300 chars
    Network(String),
    Decode(String),
}
impl ApiError { pub fn is_transient(&self) -> bool; } // RateLimited, Server, Network

pub struct Api { /* agent, usage_url, token_url */ }
impl Api {
    pub fn new() -> Api;
    pub fn with_urls(usage_url: &str, token_url: &str) -> Api;   // tests: local server
    pub fn usage(&self, access_token: &str) -> Result<Usage, ApiError>;
    /// POST the refresh (JSON body: grant_type, refresh_token, client_id, scope = the login's
    /// scopes joined by spaces). Check account.uuid / organization.uuid against `expected`
    /// (accountUuid, organizationUuid) → OwnerMismatch. Return the login with claudeAiOauth updated:
    /// accessToken; expiresAt = now_ms + expires_in*1000; refreshToken when the reply has one;
    /// refreshTokenExpiresAt = min(old, now_ms + refresh_token_expires_in*1000) when the reply has
    /// it, else unchanged; scopes from "scope" when present. All other fields and keys unchanged.
    pub fn refresh(&self, login: &Login, expected: (&str, &str), now_ms: i64) -> Result<Login, ApiError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage { pub limits: Vec<Limit>, pub extra_usage_enabled: bool }
pub struct Limit {
    pub kind: LimitKind,             // Session | WeeklyAll | WeeklyScoped | Other(String)
    pub percent: f64,
    pub severity: Severity,          // Normal | Warning | Critical | Other(String)
    pub resets_at: Option<DateTime<Utc>>,
    pub model: Option<String>,       // scope.model.display_name
    pub is_active: bool,
}
impl Usage {
    /// limits[] when present; else build Session from five_hour and WeeklyAll from seven_day
    /// (utilization → percent, severity Normal); null or missing parts are left out.
    /// Unknown keys ignored. A reply that is not an object → Decode.
    pub fn from_json(v: &serde_json::Value) -> Result<Usage, ApiError>;
    pub fn session(&self) -> Option<&Limit>;
    pub fn weekly_all(&self) -> Option<&Limit>;
    pub fn weekly_scoped(&self) -> impl Iterator<Item = &Limit>;
    /// The limit with is_active, else the highest percent, else None.
    pub fn binding(&self) -> Option<&Limit>;
}
/// §7.3: < 70 Fine, 70–89 Close, ≥ 90 or Critical severity → Critical, ≥ 100 → Locked.
pub enum LoadLevel { Fine, Close, Critical, Locked }
impl Limit {
    /// Whether this limit blocks at `now` (spec §2.7, §7.2): at 100 % or more and, for a session
    /// limit, its reset is after `now`. A week is read as 0 % after its reset by the callers.
    pub fn is_full_at(&self, now: DateTime<Utc>) -> bool;
    pub fn level(&self) -> LoadLevel;
}
impl LoadLevel { pub fn color(&self) -> &'static str; } // "#22c55e" "#f59e0b" "#ef4444" "#ef4444"
```

## switcher.rs (§6.1, §6.2, §6.4)

Everything that changes which login is live, or copies the live login into the store. Uses `locks`, `live`, `store`; never HTTP.

```rust
pub const CLAUDE_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
pub const APP_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    Lock(#[from] LockError),          // message for Busy on a Claude lock: "Claude Code is busy with its login, try again"
    Live(#[from] LiveError),
    Store(#[from] StoreError),
    UnknownAccount(String),           // the key given
    TargetLoggedOut { label: String },// stored login marked LoggedOut: show the re-login steps
    TargetHasNoLogin { label: String },// no secret stored for it
    CurrentNotSaved { email: Option<String> }, // live login belongs to no stored account, policy Refuse
    PartlyDone { state: String },     // e.g. "login switched to P, identity not: <error>"
}

/// What to do when the live login belongs to no stored account.
pub enum UnsavedPolicy { Refuse, Discard, Save { label: Option<String> } }

pub struct SwitchOutcome {
    pub from: Option<String>,   // account id that was live, if known
    pub to: String,             // target account id
    pub saved_new: Option<String>, // id of an account created by UnsavedPolicy::Save
    pub already_active: bool,   // target was already live: nothing written to Claude's files
}

/// §6.2. Order: app lock (APP_LOCK_TIMEOUT) → Claude locks (CLAUDE_LOCK_TIMEOUT) → read live
/// login + identity → find current by identity → save live login into secrets[cur] and identity
/// into cur.oauth_account, login_ends_at_ms, login_state Ok (if the live login is_ended(): mark cur
/// LoggedOut instead and save nothing) → unknown current: apply the policy (Refuse: release all
/// locks, return CurrentNotSaved — the caller asks the person and calls again) → target login from
/// secrets → live::write_login, then live::write_identity (a failure after the first write →
/// PartlyDone naming what was done) → target.last_used_at = now → store.save → locks released.
/// Target == current → refresh the stored copy only, already_active = true.
pub fn switch_to(claude: &ClaudePaths, store: &Store, target_key: &str, unsaved: UnsavedPolicy,
                 now: DateTime<Utc>) -> Result<SwitchOutcome, SwitchError>;

/// What the live login is, compared with the store. Read-only; takes no lock.
pub enum LiveState {
    NoLogin,                                  // no file or no claudeAiOauth
    Ended { account_id: Option<String> },     // is_ended()
    Known { account_id: String },
    Unknown { email: Option<String>, org_name: Option<String> },
}
pub fn live_state(claude: &ClaudePaths, file: &AccountsFile) -> Result<LiveState, SwitchError>;

/// "Save as account" (§6.1) and the watcher's mirror (§6.4 mirrorLive). Under the app lock, and
/// only when claude_locks_busy() is false (else Lock(Busy) — the caller retries later): read the live
/// login + identity; known account → store the login if it differs from the stored one, update
/// oauth_account / login_ends_at_ms / login_state Ok; unknown → create it with
/// store::new_account(label) only when `create_unknown` is true. Returns the resulting LiveState
/// and Synced::{Unchanged, Updated, Added}: Added only when this call created the account (the
/// caller that writes the history uses it for `accountAdded`; every other caller tests
/// `!= Synced::Unchanged`).
pub enum Synced { Unchanged, Updated, Added }
pub fn sync_live(claude: &ClaudePaths, store: &Store, create_unknown: Option<Option<String>>,
                 now: DateTime<Utc>) -> Result<(LiveState, Synced), SwitchError>;
// create_unknown: None = never create; Some(label) = create with that label (None = default label).

/// Remove an account from the list and its login from the secrets. Never touches Claude's files.
/// The CLI and the GUI call ops::remove_account instead, which adds the `accountRemoved` line.
pub fn remove_account(store: &Store, key: &str) -> Result<Account, SwitchError>;

/// Change label / abbr / colour, validated by store's validators.
pub fn edit_account(store: &Store, key: &str, label: Option<&str>, abbr: Option<&str>,
                    color: Option<&str>) -> Result<Account, SwitchError>;
```

## engine.rs (§6.3, §7)

Usage polling, refreshing inactive logins, the usage cache, and the derived views the CLI and the GUI show. Single-threaded; the GUI runs it on a worker thread.

```rust
/// The two calls the engine needs. `api::Api` implements it; tests use a fake.
pub trait Remote: Send {
    fn usage(&self, access_token: &str) -> Result<Usage, ApiError>;
    fn refresh(&self, login: &Login, expected: (&str, &str), now_ms: i64) -> Result<Login, ApiError>;
}
impl Remote for Api { … }

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct CacheEntry {
    pub usage: Option<Usage>,
    pub fetched_at: Option<DateTime<Utc>>,   // last successful fetch
    pub last_error: Option<String>,          // human text, never a token
    pub failures: u32,                       // consecutive transient failures
    pub backoff_until: Option<DateTime<Utc>>,
    /// The login state the history last has for this account (loginEnded / loginRenewed).
    /// Serde default: an old cache loads with None and the first poll seeds it without a line.
    pub recorded_login: Option<LoginState>,
}
/// The allBlocked / anyUsable state the history last has. Serde default: an old cache loads with
/// None, so the first poll may write AllBlocked. It moves only when its event was written.
pub struct RecordedUsable { pub all_blocked: bool, pub any_usable: bool }
#[derive(… Serialize, Deserialize)] pub struct UsageCache { pub version: u32 /* 1 */, pub entries: BTreeMap<String, CacheEntry>,
    #[serde(default)] pub recorded_usable: Option<RecordedUsable> }
impl UsageCache { pub fn load(app: &AppPaths) -> Result<UsageCache, EngineError>; pub fn save(&self, app: &AppPaths) -> Result<(), EngineError>; }
// Missing file → empty. Corrupt → EngineError::CacheCorrupt (the caller may delete it; never silently).

pub enum PollReason { Timer, PanelOpened, AfterSwitch, Wake }

pub struct Engine<R: Remote> { claude: ClaudePaths, store: Store, remote: R, cache: UsageCache }
impl<R: Remote> Engine<R> {
    pub fn new(claude: ClaudePaths, store: Store, remote: R) -> Result<Self, EngineError>; // loads the cache
    pub fn store(&self) -> &Store;
    pub fn claude_paths(&self) -> &ClaudePaths;
    /// Poll every account that is due (§7.1) and save the cache. Returns the fresh views.
    ///   active  : due when older than settings.poll_active_secs; PanelOpened → older than 30 s;
    ///             AfterSwitch/Wake → always. Token = the LIVE login's access token (read the file);
    ///             never refresh it. 401 → read the live file again and retry once; still 401 →
    ///             last_error "waiting for Claude Code to renew its login".
    ///   inactive: due when older than poll_inactive_secs; PanelOpened → older than 5 min; Wake → always.
    ///             Token = the stored login. If its access token expires within 60 s:
    ///             refresh_inactive off → skip (view shows the cached usage with its age);
    ///             on → refresh (§6.3): under the app lock, re-check with live::read_identity that the
    ///             account is still not live, Remote::refresh with expected identity, then FIRST
    ///             secrets.set(new login) (an error here: account LoggedOut + last_error), then update
    ///             login_ends_at_ms and save accounts.json, then fetch usage with the new token.
    ///             InvalidGrant → login_state LoggedOut, saved; the old tokens are never used again.
    ///             OwnerMismatch → last_error, nothing saved.
    ///   transient errors (ApiError::is_transient) → failures += 1, backoff_until = now +
    ///             min(30 min, 1 min · 2^(failures-1)) (RateLimited: at least retry_after); not due before.
    ///   LoggedOut accounts are not polled.
    pub fn poll(&mut self, reason: PollReason, now: DateTime<Utc>) -> Result<Vec<AccountView>, EngineError>;
    /// Views from the cache without any network call. Active account first, then list order.
    pub fn views(&self, now: DateTime<Utc>) -> Result<Vec<AccountView>, EngineError>;
}

#[derive(Debug, Clone, Serialize)] #[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String, pub label: String, pub abbr: String, pub color: String,
    pub email: Option<String>, pub plan: Option<String>,
    pub auto_burn: bool,                      // "Auto-burn may use this account"
    pub is_active: bool,
    pub login_state: LoginState,
    pub login_ends_at_ms: Option<i64>,
    pub login_days_left: Option<i64>,         // floor((ends - now) / 1 day), may be negative
    pub usage: Option<Usage>,
    pub usable: bool,                         // login ok, usage known, no blocking limit
    pub blocked_until: Option<DateTime<Utc>>,
    #[serde(skip)] pub blocker: Option<Blocker>,
    pub fetched_at: Option<DateTime<Utc>>,    // the later of the poll and the status line's feed
    pub last_error: Option<String>,
}

// Step B's suggestion is `burn_text::SwitchSuggestion { to, text }`, built from the decision and the
// lazy plan. It replaces the fixed `Suggestion` struct and `engine::suggestion`, which are gone.

/// §6.3: accounts whose login ends within 5 days (days_left 5..=2 → Soon, ≤ 1 → Tomorrow, < 0 → Ended).
pub enum LoginWarning { Soon { id: String, days_left: i64 }, Tomorrow { id: String }, Ended { id: String } }
pub fn login_warnings(views: &[AccountView]) -> Vec<LoginWarning>;
```

## watcher.rs (§6.4) — wave 3

Tells the GUI that the live login may have changed. It does not read or interpret the files: the
callback calls `switcher::sync_live` (mirrorLive on) or `switcher::live_state` (off).

```rust
pub const DEBOUNCE: Duration = Duration::from_millis(500);
pub const BUSY_RECHECK: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum WatchError { Notify(#[from] notify::Error), Io { path: PathBuf, source: std::io::Error } }

/// Watches the folder holding credentials_file and the folder holding identity_file (non-recursive:
/// Claude Code and seatswap replace the files by rename, so the folders are watched, not the
/// files; one watch when both are the same folder). Events for other names are ignored. After a
/// relevant event: wait until DEBOUNCE passes without another one, then until
/// locks::claude_locks_busy() is false (re-checked every BUSY_RECHECK), then call `on_change` once.
/// The callback runs on the watcher's own thread, never concurrently with itself. A missing folder
/// is an error (no create). Drop stops the watching and joins the thread.
pub struct Watcher { /* … */ }
pub fn watch(claude: &ClaudePaths, on_change: impl Fn() + Send + 'static) -> Result<Watcher, WatchError>;
```

## locks.rs / paths.rs additions — wave 3

```rust
// locks.rs: the one file-lock helper. acquire_app_lock becomes a call of it on app.switch_lock().
pub struct FileLock { /* File */ }            // AppLock is renamed to FileLock (one type, two uses)
pub fn acquire_file_lock(path: &Path, timeout: Duration) -> Result<FileLock, LockError>; // timeout 0 = try once
// paths.rs, AppPaths:
pub fn gui_lock(&self) -> PathBuf;    // <data>/gui.lock   — held for the life of the tray app
pub fn gui_socket(&self, runtime_dir: Option<&Path>) -> Result<PathBuf, PathsError>;
    // Unix socket of the running tray app. Some(dir) → <dir>/seatswap-<16 hex FNV-1a 64 of the data
    // folder's real path (fs::canonicalize)>.sock: one socket per data folder, whatever its spelling
    // (relative, absolute, through a link). The folder must exist; otherwise PathsError::DataDir
    // { path, source } (`seatswap panel` reports a missing one as "not running"). None → <data>/gui.sock.
    // A path of 108 bytes or more → PathsError::SocketPathTooLong { path } (sockaddr_un limit), never a
    // shortened path.
pub fn runtime_dir() -> Option<PathBuf>;   // free function: $XDG_RUNTIME_DIR via dirs::runtime_dir
pub fn absolute(path: &Path) -> Result<PathBuf, PathsError>;
    // std::path::absolute (lexical, no link resolution); failure → PathsError::Absolute { path, source }.
    // cli::run applies it to --data-dir and --claude-config-dir right after parsing, so everything
    // downstream (autostart entry, messages, files) sees absolute folders; autostart::startup_arguments
    // refuses a relative one (AutostartError::RelativePath) instead of resolving it again.
// paths.rs, free function (the path only; the GUI writes it with atomic::write_atomic):
pub fn autostart_file(xdg_config_home: &Path) -> PathBuf; // <xdg_config_home>/autostart/seatswap.desktop
// store.rs: reading accounts.json needs no secrets. Store::load calls it (one way).
pub fn load_accounts(app: &AppPaths) -> Result<AccountsFile, StoreError>;
// store.rs: StoreError::MissingIdentity { path } names the identity file.
// store.rs: Settings::validate rejects a claudeConfigDir that is not absolute (no `~` expansion).
// engine.rs: UsageCache::set_aside(app) -> Result<PathBuf, EngineError> renames a corrupt cache to
//   usage-cache.json.corrupt-<UTC yyyymmddThhmmss> and returns the new path (the GUI reports it).
// engine.rs: the active account is the one switcher::live_state calls Known — the only definition of
//   "live" (NoLogin, Ended, Unknown → no active account).
```

## paths.rs additions — step A

```rust
impl ClaudePaths {
    pub fn sessions_dir(&self) -> PathBuf;    // <config_dir>/sessions
    pub fn projects_dir(&self) -> PathBuf;    // <config_dir>/projects
    pub fn skills_dir(&self) -> PathBuf;      // <config_dir>/skills
}
impl AppPaths {
    pub fn history_dir(&self) -> PathBuf;          // <data_dir>/history
    pub fn machine_id_file(&self) -> PathBuf;      // <data_dir>/machine-id
    pub fn transcripts_state_dir(&self) -> PathBuf; // <data_dir>/transcripts-state
}
/// A `historyDir` must be absolute; a relative one is PathsError::HistoryDir, never resolved
/// against the current folder.
pub fn check_history_dir(path: &Path) -> Result<(), PathsError>;
/// The explicit absolute historyDir, else app.history_dir().
pub fn history_root(explicit: Option<&Path>, app: &AppPaths) -> Result<PathBuf, PathsError>;
/// The name this host writes its history under: `explicit` if set, else the machine's host name
/// (Linux: /proc/sys/kernel/hostname; Windows: %COMPUTERNAME%). 1..=63 characters of
/// [A-Za-z0-9._-], starting with a letter or a digit; anything else is PathsError::HostName.
/// The result is always in lower case.
pub fn host_name(explicit: Option<&str>) -> Result<String, PathsError>;
```

## history — records, folder, append, read (step A)

One subfolder per host, one JSON Lines file per kind and month; nothing else opens these files.

```rust
pub enum HistoryError { Io { path, source }, Json { path, line, source }, HostTaken { host, folder, machine_id },
                        Paths(PathsError), LineTooLong { kind }, NotFinite { kind } }
pub enum Kind { Samples, Windows, Events, Tokens, Sessions, Up }   // stems samples windows events tokens sessions up
pub trait Record: Serialize + DeserializeOwned { const KIND: Kind; fn at(&self) -> DateTime<Utc>; }

pub struct AccountRef { pub acc: String, pub org: String }        // accountUuid and organizationUuid
impl AccountRef { pub fn of(account: &Account) -> AccountRef; }
pub enum Source { Poll, Statusline }
pub struct LimitSample { pub k: LimitKind, pub m: Option<String>, pub p: f64, pub r: Option<DateTime<Utc>>, pub b: bool }
pub struct Sample { pub t: DateTime<Utc>, #[serde(flatten)] pub account: AccountRef, pub src: Source, pub l: Vec<LimitSample> }
pub struct WindowRecord { #[serde(flatten)] pub account: AccountRef, pub k: LimitKind, pub m: Option<String>,
    pub start: Option<DateTime<Utc>>, pub end: DateTime<Utc>, #[serde(rename = "final")] pub final_percent: f64,
    pub lost: Option<f64>, #[serde(rename = "lastSample")] pub last_sample: DateTime<Utc>,
    pub plan: Option<String>, pub uncertain: bool }
pub enum By { User, Cli, Auto, Agent, External }
pub struct SessionCounts { pub running: u32, pub busy: u32, pub tied: Option<u32> }
// `tied` (sessions on a model whose thinking is tied to the account, spec §7.4.2) is written only
// when known and left out of the line when None; until step B measures it, it is None. An old line
// with "tied":0 still reads.
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EventKind {
    Switch { from, to, by, reason, cwd, sessions, waited_secs },
    Blocked { account, k, m }, Unblocked { account, k, m }, AllBlocked, AnyUsable,
    AccountAdded { account, plan }, AccountRemoved { account }, PlanChanged { account, from, to },
    LoginEnded { account }, LoginRenewed { account }, Live { account },
    AgentCall { command, cwd, allowed },
    Waited { session, project, secs },
}
pub struct Event { pub t: DateTime<Utc>, #[serde(flatten)] pub kind: EventKind }
#[serde(rename_all = "camelCase")]
pub struct TokenRecord { pub t: DateTime<Utc>, pub session: String, pub cwd: String, pub project: ProjectKey,
    pub model: String, #[serde(rename = "in")] pub input: u64, pub out: u64, pub cw: u64, pub cr: u64,
    pub answers: u32, pub agent_answers: u32 }                // agentAnswers in the file
#[serde(rename_all = "camelCase")]
pub struct SessionRecord { pub t: DateTime<Utc>, pub session: String, pub cwd: String, pub project: ProjectKey,
    pub entrypoint: Option<String>, pub first: DateTime<Utc>, pub last: DateTime<Utc>,
    pub agents_started: u32, pub agents_finished: u32, pub closed: bool }  // agentsStarted, agentsFinished
pub struct UpRecord { pub t: DateTime<Utc> }

pub struct HistoryDir { /* root, host */ }
impl HistoryDir {
    /// Root = settings.history_dir or app.history_dir(); host = paths::host_name(settings.host_name).
    /// Creates <root>/<host>/ and its host.json {"machineId": "<uuid>"} (the id of app.machine_id_file(),
    /// created on first use); a host.json naming another machine is HostTaken.
    pub fn open(app: &AppPaths, settings: &Settings) -> Result<HistoryDir, HistoryError>;
    pub fn at(root: &Path, host: &str) -> HistoryDir;            // tests: no marker check
    pub fn host(&self) -> &str;
    pub fn root(&self) -> &Path;
    pub fn hosts(&self) -> Result<Vec<String>, HistoryError>;    // subfolders holding host.json, sorted
    pub fn append<R: Record>(&self, record: &R) -> Result<(), HistoryError>;
    /// Several records; all lines of one month file with one append, every line checked first.
    pub fn append_many<R: Record>(&self, records: &[R]) -> Result<(), HistoryError>;
    pub fn read<R: Record>(&self, range: &Range, hosts: &Hosts) -> Result<Read<R>, HistoryError>;
}
pub struct Range { pub from: DateTime<Utc>, pub to: DateTime<Utc> }   // from inclusive, to exclusive
pub enum Hosts { All, One(String) }
pub struct Hosted<R> { pub host: String, pub record: R }
pub struct Read<R> { pub records: Vec<Hosted<R>>, pub warnings: Vec<String> }
pub fn reset_minute(resets_at: DateTime<Utc>) -> DateTime<Utc>;       // round up to the full minute
```

Rules: a record goes to `<root>/<own host>/<stem>-YYYY-MM.jsonl` by its `at()` in UTC. A line,
with its newline, over `atomic::MAX_LINE_BYTES` is `LineTooLong` before the folder or file is made;
a record holding a non-finite number is `NotFinite`. Optional fields are omitted, never `null`.
`read` opens only the month files that overlap the range, skips a torn line or a line of a shape
this version does not know with a warning, and makes other broken lines an error only when they are
more than 1 % of a file's lines. `Hosts::All` is every folder with a `host.json` plus this dir's own
host, merged by `at()` and then host.

**Who writes which record.** The CLI writes the records that depend only on the command it just ran:
`switch`, `agentCall`, `accountAdded` and `accountRemoved`. Records that depend on what was written
before — samples, windows, blocked/unblocked, allBlocked/anyUsable, loginEnded/loginRenewed — are
written only by the tray app's engine. A second process's recorder does not see the other's lines and
would write a window or a blocked event twice. So `status --refresh` and `list --refresh` build their
engine without a recorder, and a CLI engine never moves `CacheEntry.recorded_login`.

```rust
pub struct Recorder { /* dir, per-account last line and remembered limits, written windows, last up,
                          read warnings */ }
impl Recorder {
    pub fn new(dir: HistoryDir) -> Recorder;
    pub fn dir(&self) -> &HistoryDir;
    /// One usage answer: appends a Sample when a limit or a reset time differs from the account's
    /// last written line, or that line is at least 1 h old. Before it, writes every window that
    /// ended and every Blocked / Unblocked crossing; returns the events written. The account's
    /// state is read from this host's files on first use.
    pub fn reading(&mut self, account: &AccountRef, plan: Option<&str>, usage: &Usage,
                   at: DateTime<Utc>, src: Source) -> Result<Vec<EventKind>, HistoryError>;
    pub fn event(&self, at: DateTime<Utc>, kind: EventKind) -> Result<(), HistoryError>;
    pub fn up(&mut self, at: DateTime<Utc>) -> Result<(), HistoryError>;   // at most once per 5 min
    pub fn tokens(&self, records: &[TokenRecord]) -> Result<(), HistoryError>;
                                    // all lines of one month file with one append
    pub fn session(&self, record: &SessionRecord) -> Result<(), HistoryError>;
    /// The warnings of the lines `reading` read back (a torn last line, a line of a shape this
    /// version does not know), each returned once; the app prints them, core never does.
    pub fn take_warnings(&mut self) -> Vec<String>;
}
pub fn sample_of(account: &AccountRef, usage: &Usage, at: DateTime<Utc>, src: Source) -> Sample;
/// A limit (matched by k and m) has ended when prev.r is Some(end) and next.t >= end, or next has
/// the limit with next.r > end + RESET_JITTER. A limit missing from next, or with r None, ends nothing.
/// end = prev.r, final = prev.p, lastSample = prev.t, uncertain = end - prev.t > 1 h;
/// start = end - 7 d for weekly kinds, end - 5 h for session, None otherwise;
/// lost = max(0, 100 - final) for weekly kinds, None otherwise.
pub fn closed_windows(prev: &Sample, next: &Sample, plan: Option<&str>) -> Vec<WindowRecord>;
/// The one reset-jitter rule: |a - b| <= RESET_JITTER (60 s). The two one-sided comparisons in
/// `write.rs` use RESET_JITTER directly to keep their one-sided meaning.
pub const RESET_JITTER: i64;
pub fn same_reset(a: DateTime<Utc>, b: DateTime<Utc>) -> bool;
```

## project.rs — from a folder to the project's identity (step A)

```rust
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum ProjectKey {
    Repo(String),     // "github.com/acme/acme-kb": host/path of remote origin, lower-case host, no protocol/user/.git
    Folder(String),   // no remote: the repository folder name; outside a repository: first folder under home, or "~"
}
/// Walk up from cwd to the first ancestor with .git (a folder, or a file "gitdir: <path>" as in
/// worktrees and submodules: follow it to the config). Read [remote "origin"] url. Nothing is run.
pub fn identify(cwd: &Path, home: &Path) -> ProjectKey;
/// The same project for https, ssh:// and scp-like remotes; credentials before @ are dropped;
/// file:// or a plain path gives None.
pub fn normalize_remote(url: &str) -> Option<String>;
/// A Windows path starting with a lower-case drive letter gets it in upper case; everything else
/// comes back unchanged. sessions and transcripts pass every cwd they read through this.
pub fn canonical_cwd(cwd: &str) -> String;
```

## sessions.rs — Claude Code's session files (step A)

```rust
pub enum SessionsError { Io { path, source } }
pub enum SessionStatus { Busy, Idle, Waiting, Other(String) }
#[serde(rename_all = "camelCase")]
pub struct SessionFile { pub pid: u32, pub session_id: String, pub cwd: String, pub started_at: i64,
    pub proc_start: String, pub version: Option<String>, pub kind: Option<String>,
    pub entrypoint: Option<String>, pub name: Option<String>,
    pub messaging_socket_path: Option<String>, pub status: SessionStatus, pub status_updated_at: i64 }
pub trait Liveness { fn alive(&self, file: &SessionFile) -> bool; }
/// Linux: pid exists and field 22 of /proc/<pid>/stat equals proc_start. Windows: the messaging
/// pipe named by messaging_socket_path is in the \\.\pipe\ listing read once by new(); nothing is
/// opened or connected to.
pub struct SystemProbe { /* Windows: the pipe names */ }
impl SystemProbe { pub fn new() -> std::io::Result<SystemProbe>; }
pub fn pipe_name(messaging_socket_path: &str) -> Option<&str>;
pub const TESTED_CLAUDE_CODE: &str = "2.1.322";
/// Every session file whose process is alive, sorted by started_at then pid. A missing folder is
/// an empty list. Only <digits>.json is read; an empty file and a file without status are ignored
/// quietly; any other file that does not parse is named in skipped.
pub fn running(claude: &ClaudePaths, probe: &dyn Liveness) -> Result<Running, SessionsError>;
pub struct Running { pub sessions: Vec<SessionFile>, pub skipped: Vec<String> }
pub fn newer_than_tested(version: &str) -> bool;
```

## transcripts.rs — incremental reading, answers, token records (step A)

```rust
pub enum TranscriptError { Io { path, source }, State { path, source }, Version { path, found } }
pub struct Answer { pub session: String, pub agent: Option<String>, pub message_id: String,
    pub at: DateTime<Utc>, pub cwd: String, pub model: String, pub input: u64, pub output: u64,
    pub cache_write: u64, pub cache_read: u64, pub thinking: bool }
pub struct UserTurn { pub session: String, pub at: DateTime<Utc> }
pub struct AgentMeta { pub session: String, pub agent: String, pub agent_type: Option<String>,
    pub description: Option<String>, pub finished: bool, pub spawn_depth: u32 }
#[derive(Default, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct ScanState { pub version: u32, pub files: BTreeMap<String, FileState>,
    pub sessions: BTreeMap<String, SessionSeen> /* private: changed folders */ }
pub struct FileState { pub offset: u64, pub grew: DateTime<Utc>, pub recent: Vec<String>,
    pub last_answer: Option<DateTime<Utc>> }
pub struct SessionSeen { pub folder: String, pub cwd: String, pub first: DateTime<Utc>,
    pub last: DateTime<Utc>, pub last_model: Option<String>, pub last_user_turn: Option<DateTime<Utc>>,
    pub thinking_since_user: u32, pub recorded: Option<RecordedSession> }
pub struct RecordedSession { pub last: DateTime<Utc>, pub closed: bool }
// `recorded` is the last session record written from this session: the answer time it covers and
// whether it was the closing one. It keeps a restart from writing the same record again; absent in
// an old state file means none.
impl ScanState {
    pub fn load(app: &AppPaths) -> Result<ScanState, TranscriptError>;  // missing folder = default
    /// The one way to remember the session record written from `session`: sets `recorded` and marks
    /// the session's project folder changed, so it reaches the state file even when its transcript
    /// did not grow. A session not in the state is not changed.
    pub fn set_recorded(&mut self, session: &str, recorded: RecordedSession);
    /// Mark one project folder for the next save.
    pub fn mark_changed(&mut self, folder: &str);
    /// Writes only the project folders that changed, as <folder>.json; before writing, drops the
    /// recent ids of a file that has not grown for 24 h, and removes entries whose transcript is gone.
    pub fn save(&mut self, claude: &ClaudePaths, app: &AppPaths, now: DateTime<Utc>)
        -> Result<u32, TranscriptError>;
}
pub struct Scan { pub answers: Vec<Answer>, pub user_turns: Vec<UserTurn>, pub complete: bool,
    pub warnings: Vec<String> /* private: the state after this scan */ }
impl Scan {
    pub fn state(&self) -> &ScanState;              // for token_records
    /// The project folders this scan changed; empty after save.
    pub fn changed_folders(&self) -> Vec<String>;
    /// Write this scan's changed folders, before its answers' token lines.
    pub fn save(&mut self, claude: &ClaudePaths, app: &AppPaths, now: DateTime<Utc>)
        -> Result<u32, TranscriptError>;
    /// Moves the state past what this scan read; call it only after the answers' token lines were
    /// written. A scan that is not committed leaves the old offsets, so the next pass reads the same
    /// lines again and nothing is counted twice.
    pub fn commit(&mut self, state: &mut ScanState);
}
/// Reads what is new in <folder>/<session>.jsonl and <folder>/<session>/subagents/agent-<id>.jsonl,
/// at most budget_bytes; only whole lines, and the offset stops after the last '\n'. A shorter file
/// is read again from 0 (warning); its recent ids keep answers from counting twice. A file that had
/// stopped growing and was replaced is not counted again, and the warning says so. Other line
/// types are skipped quietly. A user line counts only as a real user message whose origin is
/// absent or "human". An answer counts once per file by message id; "<synthetic>" is skipped.
/// The caller's state is not changed; the result carries the state to `commit`.
pub fn scan(claude: &ClaudePaths, state: &ScanState, budget_bytes: u64)
    -> Result<Scan, TranscriptError>;
pub fn agents_of(claude: &ClaudePaths, session: &str) -> Vec<AgentMeta>;
/// One TokenRecord per (session, hour, model); an agent's answers go to its parent session and use
/// the parent's folder when known. The group's cwd is that of its earliest answer.
pub fn token_records(answers: &[Answer], state: &ScanState, home: &Path) -> Vec<TokenRecord>;
```

Only numbers and names leave the parser: time, folder, model, block kinds, token counts, session
and agent ids. Message text is never stored, logged, put into a warning or sent anywhere.

## engine.rs additions — usable, models in use, due at a reset (step A)

```rust
impl<R: Remote> Engine<R> {
    pub fn with_recorder(self, recorder: Recorder) -> Self;   // every record_success also calls recorder.reading
    pub fn recorder(&mut self) -> Option<&mut Recorder>;
    /// The models of the running sessions (lower-case ids), or None when they are not known
    /// (readSessions off, or the CLI).
    pub fn set_models_in_use(&mut self, models: Option<BTreeSet<String>>);
}
pub enum Blocker { Week, Session, Model(String) }
/// Why this usage blocks the account, or None. Week: weekly_all >= 100. Session: session >= 100.
/// Model(m): a weekly_scoped limit of m >= 100 and a model in use contains m lower-cased; with
/// models_in_use None, only while the server marks that limit active.
pub fn blocker(usage: &Usage, models_in_use: Option<&BTreeSet<String>>) -> Option<Blocker>;
/// At least one view has a working login, and every view with a working login has a Blocker.
pub fn all_blocked(views: &[AccountView]) -> bool;
// AccountView gains:  pub usable: bool, pub blocked_until: Option<DateTime<Utc>>
```

When several limits are at 100 %, the blocking one is chosen by the latest reset time (a limit with
no reset wins over one with a time); on a tie Week comes before Model, Model before Session.
`blocked_until` is that limit's reset time. `record_success` passes the account's `AccountRef` and
plan to the recorder; a history error becomes the account's `last_error`, never a failed poll. An
account blocked by a limit is due once 30 s after that reset whatever the interval, if its last
success and last attempt are both before that moment. After a poll the engine writes `AllBlocked` when
`all_blocked` becomes true, and `AnyUsable` when some view becomes usable after none was; it remembers
both in `UsageCache.recorded_usable`, which moves only when its event was written, so a restart during
a block writes no second `AllBlocked`. An account
whose usage is unknown is not usable but not blocked either, so it never makes `all_blocked` true and
losing the last working login is not "usable again".

The same poll compares every stored account's login state with `CacheEntry.recorded_login`: `None`
is seeded without a line (an old cache or a new account); `Ok` that is now `LoggedOut` writes
`loginEnded`; `LoggedOut` that is now `Ok` writes `loginRenewed`. The event carries the poll's `now`.
The field moves only when its event was written, so a failed write is retried by the next poll and
an engine without a recorder moves nothing (the next tray-app engine writes it). A failed login-event
write becomes a `last_error` through the same helper as a failed `AllBlocked`/`AnyUsable` write, so it
never hides another error and never fails the poll; it carries its own prefix, so a successful write
of one event kind never clears the other kind's failure.

## app: CLI (§4)

`crates/app/src/cli.rs`, clap derive. `main.rs` parses; no subcommand → tray app when built with `gui`, else help; from an agent (§6.6 of the spec) it is refused instead (`no_subcommand(caller)`). Global options: `--claude-config-dir <DIR>` (else the `claudeConfigDir` setting, else `$CLAUDE_CONFIG_DIR`, else `~/.claude`), `--data-dir <DIR>` (else the platform folder). Every command prints errors as `seatswap: <message>` to stderr and exits 1 (2: an unsaved current login; 3: refused for an agent); success exits 0.

| Command | Does |
|---|---|
| `list` | One line per account: `*` for the live one, abbr, label, email, plan, session %, week %, "login ends in N d", "LOGGED OUT" when so. Usage from the cache, with its age ("5 min ago"); `--refresh` polls first (PollReason::PanelOpened). |
| `use <KEY>` | `switch_to`. KEY = id or abbr. When the live login is not saved: `--save-current [LABEL]` saves it first, `--discard-current` drops it; without either, exit 2 with the message "the current login (<email>) is not saved; rerun with --save-current or --discard-current". After the switch: "Switched to P — Private". |
| `next` | Switch to the next Ok account after the live one in list order (wraps), same unsaved handling as `use`. With `autoBurn` on, an agent's call prints `seatswap: auto-burn is on and decides` and switches nothing. |
| `add [--label L]` | `ops::save_live_login` (`sync_live` with create): saves the live login as a new account or updates the stored copy; a new account writes `accountAdded` with its plan. Prints the account. |
| `rm <KEY> [--yes]` | `ops::remove_account` (writes `accountRemoved`); without `--yes` asks "Remove AC — Acme? [y/N]" on a terminal, refuses when stdin is not a terminal. |
| `edit <KEY> [--label L] [--abbr A] [--color #rrggbb]` | `edit_account`. |
| `panel [main\|add\|settings]` | Asks the running tray app to open its pop-up (§ app: GUI, single instance); `settings` sends `window settings` and opens the large window instead, because the pop-up has no settings view (step C1). Not running → exit 1 "seatswap is not running". This is the hotkey on Linux v1: the person binds it in the desktop's keyboard settings (no global-shortcut crate is available). |
| `agent-skill install\|uninstall [--force]` | `agent_skill`: `<claude config dir>/skills/seatswap/SKILL.md` from the bundled text, through `atomic::ensure_private_dir` and `atomic::write_atomic` (0600). Install prints "already installed: <path>" when the file is identical, refuses a differing file without `--force`, else prints the path. Uninstall prints "not installed: <path>" when absent, refuses a differing file without `--force`, else prints "removed <path>" and removes the folder when it is empty. |
| `status` | The live account in one line: `AC 97% · 1h12m` (binding limit, time to its reset; format §11: `35m`, `1h05m`, `1d18h`, beyond 48 h `Sun 02:00` — `format::format_reset_at`; reset times are rounded up to the full minute first, `format::reset_minute`). `--json` prints `views` + `active` + `suggestion` + `autoBurn` + `next` + `burnError` (§7.8 of the spec); each view carries `usable`, the one definition of §7.2, and `slackHours` when known. `--waybar` prints one JSON line `{"text":"AC 97%","tooltip":"…","class":"fine|close|critical|locked|none","percentage":97}`; `--i3blocks` prints three lines (full text, short text, colour). `--refresh` polls first (PollReason::Timer). Only `--json` reads the history. |
| `statusline` | Claude Code's status line: reads its JSON from stdin, prints the one line and exits 0 (even when it prints `seatswap: <reason>`). Never gated, never recorded, no lock, no network. |
| `burn plan [--json]` | The regime and the lazy plan (§7.7 of the spec). |
| `burn simulate --from D --to D [--without KEY]…` | Replays the two local days for `Lazy` and `Eager`. |

### Agent gate and skill (step A)

```rust
pub enum Caller { Person, Agent }
/// Agent when `CLAUDECODE` is exactly "1", or `AI_AGENT` is set and not empty. `env` is passed in,
/// never the process's own environment.
pub fn caller(env: &dyn Fn(&str) -> Option<String>) -> Caller;
pub enum Gate { Allowed, Refused(&'static str) }
pub fn gate(command: &Command, caller: Caller, settings: &Settings) -> Gate;
pub enum SkillAction { Install { force: bool }, Uninstall { force: bool } }
pub enum CliError { … , AgentRefused(&'static str) }   // exit code 3
```

Gate, when `caller` is `Person`: everything is `Allowed`. When `Agent`: `list`, `status` and `burn`
need `agents_read`; `statusline` is always allowed; `use` and `next` need
`agents_switch`, and never with `--save-current` or
`--discard-current`; `add`, `rm`, `edit`, `agent-skill` and `panel` are always refused. The three
texts are `agents may not read usage (seatswap settings → Agents)`, `agents may not switch accounts
(seatswap settings → Agents)` and `agents may not change seatswap's accounts or settings`; a
refusal prints `seatswap: <text>` and exits 3. The store is opened first, so the call can be
recorded: a refused call changes no account, setting or login, and writes only its `agentCall`
event.

`command_text` names the subcommand and its positional values and flags, never free text
(`--label`, `--abbr`, `--color`, `--save-current`'s value); every call by an agent writes
`EventKind::AgentCall { command, cwd, allowed }`. `current_cwd` records the folder only when
`std::env::current_dir()` and its UTF-8 name can be read; otherwise it prints a warning and records
None, never a replaced name. A switch made by a caller `Agent` is recorded with `By::Agent` and the
same folder; the tray always announces it with `An agent switched to {abbr} (from {cwd with ~}).` or
`An agent switched to {abbr}.` when the folder is unknown, whatever `notifyOnSwitch` says. The
CLI tests' `command()` removes `CLAUDECODE` and `AI_AGENT` so a developer's terminal cannot change
a test's result.

## app: icon.rs (§8.1, §8.2) — wave 1, `gui` feature

```rust
pub struct IconSpec {
    pub abbr: String,                 // 1–2 chars, already validated
    pub account_color: [u8; 3],
    pub ring: Option<(f64, [u8; 3])>, // percent 0..=100+ and its LoadLevel colour; None = unknown → grey ring
    pub locked: bool,                 // lock badge bottom right
    pub monochrome: bool,             // white ring and disc outline, letters cut out
}
/// Straight (not premultiplied) RGBA, size_px × size_px, row-major — what tauri::image::Image::new_owned takes.
pub fn render(spec: &IconSpec, size_px: u32) -> Vec<u8>;
```

Drawing (tiny-skia, anti-aliased): transparent background; ring of width max(2, size/8) from 12 o'clock clockwise, filled part in the ring colour, rest grey `#555b66`; disc inside in the account colour; letters centred in white or `#0f1115`, whichever has the higher WCAG contrast against the disc; font `crates/app/assets/DejaVuSans-Bold.ttf` via `include_bytes!` and `ab_glyph`. One letter is drawn larger than two. If the measured text is wider than the disc's inner width, squeeze it horizontally (scale x only) before ever making it smaller; the font height never goes below 9 px at size ≥ 16, and at smaller sizes is as large as fits. Percent above 100 draws a full ring.

## app: GUI (§8, §9, §11) — wave 3, `gui` feature

Files: `crates/app/src/gui/{mod.rs, worker.rs, tray.rs, panel.rs, notify.rs, autostart.rs, ipc.rs, registry.rs}` (`registry.rs` on Windows only), `crates/app/ui/{index.html, panel.css, panel.js}`, `crates/app/capabilities/default.json`. The code is the library `seatswap` (`src/lib.rs`, `pub fn run() -> ExitCode`); two programs call it: `seatswap` (`src/main.rs`) and `seatswap-tray` (`src/bin/seatswap-tray.rs`, built without a console window on Windows for autostart and the Start menu; Linux builds it but does not install it). No subcommand → `gui::run(app: AppPaths, claude_override: Option<PathBuf>, data_override: Option<PathBuf>) -> ExitCode` (the same path resolution as the CLI, one function; the overrides are passed on so `seatswap panel` and autostart start the app with the same flags). Actions both front ends must take the same way live in `crates/app/src/ops.rs`: `save_live_login(claude, store, recorder: Option<&Recorder>, label, now) -> Result<(Account, Option<String>), String>` (a missing, ended or unknown login saved nothing and is an error; the `Option<String>` is the history error, never a failed add, and `accountAdded` is written only when `sync_live` returned `Synced::Added`), `remove_account(store, key, recorder: Option<&Recorder>, now) -> Result<(Account, Option<String>), String>` (the same shape for `accountRemoved`), `switch_message(store, account_id) -> Result<String, String>` ("Switched to AC — Acme"; an unreadable store or unknown id is an error, never the raw id) and `switched_without_list(error) -> String`. The action thread's external-switch check keeps the history read warnings it printed, so a torn month-file line is printed once per run, not at every live-account change.

**Threads.**
- *Main thread:* Tauri event loop, tray, menu, panel window.
- *Engine thread:* owns `Engine<Api>` (its own `Store` from `store::open_store`). Loop on a command channel with a 30 s tick: `Poll(PollReason)`, `Publish`, `Feed`, `HistoryChanged`, `Shutdown`. Each tick polls with `Timer` (the engine decides what is due). Wake detection: when the wall clock advanced more than 90 s beyond the monotonic clock between two ticks → `Poll(Wake)`. After each poll or `Publish` it builds a `Snapshot` and hands it to the main thread.
- *Action thread:* owns a second `Store` (from `open_store`). Runs switch / save current / edit / remove / save settings / pause-burn one at a time, each replying through a one-shot channel, then sends `Poll(AfterSwitch)` or `Publish` to the engine thread. Two stores are safe because every accounts.json change is load-modify-save under the app lock.
- *Watcher thread:* `watcher::watch`; its callback sends `SyncLive` to the action thread (sync_live when `mirrorLive`, else nothing), which then sends `Publish`.
- Tauri commands never block the main thread (`async` commands that wait on the reply with `tauri::async_runtime::spawn_blocking`).

**Snapshot** (serde camelCase; the panel's only data source; event `snapshot`, and command `get_snapshot`):

```rust
pub struct Snapshot {
    pub views: Vec<AccountView>,          // engine::views, active first
    pub active_id: Option<String>,
    pub live: LiveView,                   // { kind: "noLogin"|"ended"|"known"|"unknown", accountId?, email?, orgName? }
    pub burn: Option<BurnView>,           // step B: auto-burn's line, plan, waiting switch, suggestion
    pub warnings: Vec<LoginWarning>,      // engine::login_warnings (Serialize, camelCase, tag "kind")
    pub pending: Option<PendingSwitch>,   // { targetId, targetLabel, email, orgName }: a switch waits for "save or discard the current login?"
    pub secrets_warning: Option<String>,  // from open_store: logins stored unencrypted
    pub last_poll_at: Option<DateTime<Utc>>,
    pub error: Option<String>,            // last engine-wide error (store/cache unreadable)
    pub settings: Option<Settings>,      // None when accounts.json cannot be read: the panel disables Save
}
```

**Tauri commands** (all `Result<T, String>`, the error is the human message of the core error):
`get_snapshot() -> Snapshot` · `refresh_now()` (Poll(Wake)) · `switch_account(key, unsaved: "refuse"|"discard"|"save", label: Option<String>) -> String` (the success sentence; `CurrentNotSaved` sets `pending` and returns Err "not saved") · `cancel_pending()` · `save_current(label: Option<String>)` (sync_live with create) · `edit_account(key, label?, abbr?, color?)` · `remove_account(key)` · `save_setting(field: String, value: serde_json::Value) -> SavedSetting` (one field merged under the app lock, validated, applies startAtLogin) · `set_auto_burn(on)` (`SaveSetting { field: "autoBurn", value: on }`: the same save path, writes `autoBurnSet` only on a change) · `pause_burn(hours: Option<i64>)` (Some = pause, None = resume) · `icon_preview(abbr, color) -> Vec<IconPx { size, rgba: Vec<u8> }>` for 16, 22, 32 (the panel draws them with `ImageData`) · `fit_panel(height: f64)` · `hide_panel()`.
Event `navigate` (payload `"main"|"add"|"settings"`) when the panel is shown for a menu item.

**Tray** (`tray.rs`, redrawn on every snapshot): icon from `icon::render` at 22 px ×2 (44 px image) for the active account — ring = `settings.iconLimit` limit (binding/session/week) with its `LoadLevel` colour, `locked` at ≥ 100 %, grey ring while the usage is unknown, a grey "?" disc when no account is live; tooltip as §8.1; tray label (`set_title`) per `trayText`. Linux backend: tray-icon's `ksni` (StatusNotifierItem over D-Bus), switched on in `crates/app/Cargo.toml` next to Tauri's libappindicator, because libappindicator reports no clicks and drops the tooltip; with ksni the item has `ItemIsMenu = false`, a left click arrives as `TrayIconEvent::Click(Left)` (opens the panel), a right click shows the menu, and the tooltip is published (checked on the session bus, 2026-11-10). Without a StatusNotifierWatcher on the bus (plain GNOME without the AppIndicator extension, or a desktop panel that starts after seatswap at login) tray-icon cannot create the item and has no option to wait; `tray::start` then keeps the app running without an icon, says so on stderr at once and in a notification only if no tray has appeared after 30 s (`TRAY_GRACE`: at login Cinnamon starts its tray in the same second as seatswap, so the first try often comes too early) (`seatswap panel` still opens the pop-up), and creates the icon when the name `org.kde.StatusNotifierWatcher` appears (`gio::bus_watch_name`, GTK's own gio). Once the item exists, ksni registers it again whenever the watcher restarts. Menu as §8.3: one item per account (`AC  Acme   97 % · 1h12m` + "✓" for the live one, "logged out" when so), clicking switches with `refuse`; then "Save current login (…)…" when live is unknown; "Open panel…", "Add account…", "Settings…", "Quit". Number formatting is shared with the CLI (one module, `crates/app/src/format.rs`).

**Panel** (`panel.rs`): one window label `panel`, created on first open (`index.html#<view>`), kept and hidden afterwards. 380 px wide, height from `fit_panel`, no decorations, always on top, skip taskbar. Placement §9.2: X11 at the mouse, inside the monitor that holds it, above the mouse when the mouse is in the lower half, else below; Wayland (`XDG_SESSION_TYPE=wayland`) centred. `show` takes an `Anchor { cursor, monitor, scale }` once per opening and keeps it with the last requested logical height in `GuiState.panel: Mutex<Placement>`; `fit_panel` resizes and re-places from that anchor (`anchored_position(anchor, height, wayland)` → `place_panel`), so the panel keeps its bottom edge above the mouse, its top edge below it, and stays inside the monitor. Never read the window's position or size back to place it: on X11 they report the old values until the window system has applied a change (the headless run of 2026-11-10 drifted the panel to y = 836 and y = −386 that way). The window is built `resizable(true)`: GTK ignores `set_size` on a non-resizable window. Closes on Esc and the header's × (panel.js → `hide_panel`), on focus loss, and on a left click on the tray icon while open (`panel::toggle`, decision in the pure `tray_click(visible, focus_hidden_ago)`; a click within `REOPEN_GUARD` = 400 ms after a focus-loss hide is the click that caused it and does not reopen). Focus loss hides only if the panel is still unfocused `FOCUS_RECHECK` = 150 ms later and no pop-up of the process holds a GTK grab (`own_popup_open` → `gtk::grab_get_current`): WebKitGTK shows a `<select>` list as its own window with a grab, so the panel is unfocused for as long as the list is open; the list closes on the next click outside and hands the focus back. Focus on X11 (`take_focus`, `focus_x11`): `present_with_time` + `gdk_window.focus` with the X server's current time (`gdkx11::x11_get_server_time`) — Cinnamon's window manager refuses `set_focus`, which asks without a timestamp; and on every map the request is repeated up to `FOCUS_RETRIES` = 5 times, `FOCUS_RETRY_GAP` = 80 ms apart, until the window is active, because a freshly mapped window is not yet managed and the first request is ignored (both seen on Cinnamon 2026-11-10). Built hidden at startup (`panel::preload` in `setup`) so the page has loaded and measured itself before the first opening. A hidden panel does not appear when `show` is called: `show` stores the anchor, sets `PanelState.pending_show` and sends `navigate`; the page answers every navigation with `fit_panel` (setView), and that call sizes, places and only then shows it (`reveal`: move, then `show`), so the window manager maps it once, at its final size and place. If no height arrives within `SHOW_DEADLINE` = 300 ms it appears anyway with a log line. On Linux, nothing may map the window before the move is applied (`present_with_time` maps through GTK at once, and the panel flashed at its old place); focus is asked from the map handler instead. `.panel` has `flex-shrink: 0`: before the first showing the viewport has no width and a shrinking panel measured 2698 px tall. Opening it sends `Poll(PanelOpened)`.

**Notifications** (`notify.rs`, notify-rust): every notification is built from `base()` (summary "seatswap"; on Windows `.app_id("dev.seatswap.app")`), the only caller of `Notification::new()`. On Windows `register_app_id()` writes the display name "seatswap" under `HKCU\Software\Classes\AppUserModelId\dev.seatswap.app` once at startup, before the single-instance listener; a failure is a startup error `notification name: …`. After a switch when `notifyOnSwitch` ("Switched to P — Private"); auto-burn's suggestion (§7.4) once per target and hour, with a "Switch" action (`wait_for_response` on its own thread; click on the body opens the panel); login warnings once per account and level per run; an unknown live login once per identity per run ("New login found: <email>. Save it as an account?" → a click on its button or body opens the add view). What counts as a click is decided in one pure function, `click(response, button)`: the offered button (`NotificationResponse::Action`) or the body (`Default`); a notification that timed out, was dismissed or closed (`Closed`) does nothing. On Linux the body click only arrives when the notification announces the action `default` (`clickable()`); the notification service shows no button for it. Windows reports a body click by itself, so `default` is announced on Linux only.

**Single instance and `seatswap panel`** (`ipc.rs`): the tray app takes `acquire_file_lock(gui_lock, 0)`. Busy → another instance runs: send `open-panel main` to `gui_socket` and exit 0. Held → name the socket only now (the lock created the data folder on first start, and the name needs its real path), remove a leftover socket, listen on it (mode 0600 on Unix; on Windows the file has the data folder's permissions). Windows uses the same Unix socket through `uds_windows` (`std::os::windows::net` is not stable yet); there is no runtime folder, so the socket is `<data>\gui.sock`. One line per connection: `open-panel <view>` → `ok`, anything else → `error unknown command`.

**Autostart** (`autostart.rs`, one entry point `apply_autostart(enabled, arguments)`): at start and on every `save_setting`. The entry is text: a program path or folder argument that is not valid Unicode is `AutostartError::NotUnicode` on both platforms (the one `path_text`), never a path with replaced characters. Linux: `startAtLogin` on → write `autostart_file($XDG_CONFIG_HOME or ~/.config)` with `Exec=<current_exe>` via `write_atomic` (`Access::Shared`); off → remove it if present. Windows: on → the string value `seatswap` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` = `"<folder of current_exe>\seatswap-tray.exe" "<arg>" …` (each part in double quotes, `"` written `\"`, a run of backslashes before a `"` or at the end doubled); a missing `seatswap-tray.exe` (`NoTrayProgram`), a path that is not valid Unicode (`NotUnicode`) or a command over 260 UTF-16 units (`CommandTooLong`) is an error, never shortened; off → delete the value (missing is fine). `gui/registry.rs` (Windows only) is the one module that names `winreg`: `set_string`, `get_string` (tests only), `delete_value`, all under `HKEY_CURRENT_USER`.

**Panel UI** (`ui/`, plain HTML/CSS/JS, no build step, `window.__TAURI__` with `app.withGlobalTauri: true`): §11.1–11.2. Views `main` and `add` (the §6.1 steps; shows the unknown live login with a label field and "Save" when there is one); a `settings` request now opens the large window at Settings (step C1), because the pop-up has no settings view. The main view's footer carries the sessions line of §11.1 from `snapshot.sessions`. Every time on the page (reset countdowns, the header's poll age, a card's "usage 5 min ago") is an element with `data-time` + `data-time-style` whose text comes from the command `time_texts([{at, style}]) → [text]` (`countdown` → `format_reset_at`, `clock` → `format_reset_clock`, `age` → `format_age`), asked after each render and every second, so the page has no copy of these rules. Without `window.__TAURI__` it runs on a built-in fixture snapshot so it can be opened in a browser; there `time_texts` answers with stand-ins (the plain clock time, "a moment ago").

## app: sessions thread (`gui/sessions_rt.rs`) — step A

It runs only when `readSessions` is on: every wake asks which sessions are alive, reads the new
parts of their transcripts, writes token and session lines to the history, then hands a
`SessionsSummary` to the main thread and the models in use to the engine thread.

```rust
pub(crate) struct PassOutcome { pub summary: SessionsSummary, pub models: Option<BTreeSet<String>>,
                                pub wake_soon: bool }
pub(crate) struct SessionMemory { /* home, project per cwd, waits, last session record, closed */ }
impl SessionMemory { pub(crate) fn new(home: PathBuf) -> SessionMemory; }
pub(crate) struct SessionsHandle { /* stop flag, wake sender, join handle, latest summary */ }
impl SessionsHandle {
    pub(crate) fn latest(&self) -> Arc<Mutex<Option<SessionsSummary>>>;
    pub(crate) fn stop(&mut self);           // also joins the thread; Drop stops it
}
pub(crate) fn spawn(app: AppPaths, claude: ClaudePaths, settings: Settings,
                    ui: Sender<UiEvent>, engine: Sender<EngineCommand>) -> Result<SessionsHandle, String>;

/// One wake, without the thread: the sessions, new transcript answers, token lines, session lines
/// and waits. `state_error` is the load error that keeps this pass from saving the state; it is
/// shown in the summary. A pass that did not finish asks for a wake in one second.
pub(crate) fn run_pass(app: &AppPaths, claude: &ClaudePaths, probe: &dyn Liveness,
                       recorder: Option<&Recorder>, state: &mut ScanState,
                       memory: &mut SessionMemory, now: DateTime<Utc>,
                       state_error: Option<&str>) -> PassOutcome;
/// run_pass, then state.save; a save failure is a warning on the summary. The thread's start is
/// load_state(app) + SessionMemory::new(home) + one_pass, which is what the backfill test does.
fn one_pass(app: &AppPaths, claude: &ClaudePaths, probe: &dyn Liveness, recorder: Option<&Recorder>,
            state: &mut ScanState, memory: &mut SessionMemory, now: DateTime<Utc>,
            state_error: Option<&str>) -> PassOutcome;
```

`WAKE` = 60 s, `AFTER_INCOMPLETE` = 1 s, `SCAN_BUDGET` = 64 MiB per pass, `RECORD_EVERY_SECS` =
10 min: a running session's record is written when its summary changes and at most that often, and
one closing record when it is gone. A session seen in a transcript whose session file is not running
gets one closed record too (a short `claude -p` session, every session the backfill read); a later
scan that adds answers writes a new closed record, and `SessionSeen.recorded` keeps a restart from
writing the same one again. `SessionMemory.closed` remembers only a session already pruned from the
state, which `recorded` cannot cover. Token records are written on every pass from the answers that
pass read (`transcripts::token_records`). When there are token lines, the scanned state is saved
first; if that save fails no token line is written and nothing is committed (the next pass tries
again). If the token write fails, the old state's folders are saved back and nothing is committed.
A crash between the two steps loses those answers but never counts them twice. Without a recorder
the offsets stay, so the next pass with a working history reads the same answers. Waits are kept in
memory: a wait that ended writes
`EventKind::Waited { session, project, secs }` with the whole seconds (never 0 from a backwards
clock). Only "the session files cannot be read" is `SessionsSummary.unavailable` (the counts are
then 0); a history error, a state that cannot be read or written, an unknown `statusUpdatedAt`, a
count that does not fit u32 and a transcript read error are `SessionsSummary.warning`, with the
history error first and then the state. The thread keeps running. The app prints the recorder's read
warnings once on stderr as `seatswap: history: <warning>`; the action thread does the same for the
warnings of the month file `record_external_switch` reads.

## app: worker, ops and notifications additions — step A

```rust
// worker.rs
pub(crate) struct SessionsSummary { pub running: u32, pub busy: u32, pub tied: u32, pub tied_busy: u32,
    pub waiting: Vec<Waiting>, pub unavailable: Option<String>, pub warning: Option<String> }
pub(crate) struct Waiting { pub session: String, pub label: String, pub project: String,
    pub since: DateTime<Utc> }
pub(crate) struct Snapshot { … pub burn: Option<BurnView>, pub sessions: Option<SessionsSummary> /* None = readSessions off */ … }
pub(crate) enum EngineCommand { Poll(PollReason), Publish,
    ModelsInUse { models: Option<BTreeSet<String>>, tied_busy: u32 }, Feed, HistoryChanged, Shutdown }
pub(crate) enum UiEvent { Snapshot(Snapshot), Sessions(SessionsSummary) }

/// The timer poll as a fixed deadline: a command never moves it; `advance` jumps over `now`.
struct TickDeadline { next: Instant }
impl TickDeadline { fn new(now: Instant) -> TickDeadline; fn wait(&self, now: Instant) -> Duration;
                    fn due(&self, now: Instant) -> bool; fn advance(&mut self, now: Instant); }

/// History-read warnings already printed this run. The tray app keeps one set; the auto-burn
/// runtime uses the same type rather than a second set.
pub(crate) struct PrintedWarnings { seen: BTreeSet<String> }
impl PrintedWarnings { pub(crate) fn new() -> PrintedWarnings;
                       pub(crate) fn new_texts(&mut self, warnings: &[String]) -> Vec<String>; }

// ops.rs
pub(crate) fn open_history(app: &AppPaths, settings: &Settings) -> Result<Recorder, String>;
pub(crate) fn history_unwritable(error: &dyn std::fmt::Display) -> String;
pub(crate) fn record_switch(recorder: &Recorder, from: Option<AccountRef>, to: AccountRef,
    saved_new: Option<&Account>, by: By, reason: Option<String>, cwd: Option<String>,
    sessions: Option<SessionCounts>, at: DateTime<Utc>) -> Result<(), HistoryError>;
// saved_new = the account SwitchOutcome.saved_new created: its accountAdded is written before the switch.
// Both lines are tried even when the first fails; the first error is returned.

// notify.rs
pub(crate) enum Notification { Suggestion(SwitchSuggestion), LoginWarning(..), UnknownLogin { .. },
    UsableAgain { id: String, reason: String }, AllFull { next: Option<(String, DateTime<Utc>)> },
    Waiting(Waiting), WaitingOver { session: String } }
impl Deduper {
    /// All notifications that are due now: suggestions and warnings as before, plus usable again,
    /// all full, and waits.
    pub(crate) fn due(&mut self, snapshot: &Snapshot, now: DateTime<Utc>) -> Vec<Notification>;
    /// When the next waiting session becomes 15 s old; `None` when none is still waiting.
    pub(crate) fn recheck_at(&self, snapshot: &Snapshot, now: DateTime<Utc>)
        -> Option<DateTime<Utc>>;
}
pub(crate) fn send_usable_again(account: &str, reason: &str, on_switch: impl FnOnce() + Send + 'static,
    on_open: impl FnOnce() + Send + 'static);
pub(crate) fn send_all_full(next: Option<(&str, &str)>);
pub(crate) fn send_waiting(waiting: &Waiting, host: Option<&str>,
    on_open: impl FnOnce() + Send + 'static) -> WaitingHandle;
impl WaitingHandle { pub(crate) fn withdraw(self); }
```

`Notification::UsableAgain` needs `notifyUsableAgain` on, the account now usable, not active, and
the previous snapshot naming a blocker whose `blocked_until` is at or before `now` (the block ended
because its reset passed); it is remembered by (account id, previous reset minute), so one per
account and reset. A block that ends earlier sends nothing and does not use that key. `AllFull` only
when the previous snapshot had a usable account and `engine::all_blocked` says every account whose
login still works is now blocked; the next is the earliest `blocked_until`. `Waiting`
only after 15 s (`waiting.since` to `now`), once per session and wait; turning `notifyWaiting` off
withdraws the shown ones and sends nothing new; a session that stops waiting gets `WaitingOver`,
which withdraws its notification. The texts come from one place each: `usable_again_text` =
`{abbr} is usable again: {reason}.`, with reason `its 5 h window reset`, `its week reset` or
`its {model} week reset`; `all_full_text` = `All accounts are full. Next: {abbr} at {time}.` or
`All accounts are full.`; `waiting_text` = `{label} is waiting for you ({project}, on {host}).` or,
without a host name, `{label} is waiting for you ({project}).`. On Linux a waiting notification
never times out and can be withdrawn; on Windows it follows the system's rules and the pop-up keeps
the count.

---

## Step B: status line, feed and auto-burn

The new modules, each with one job:

| Module | Job |
|---|---|
| `core::feed` | the status line's live numbers: the feed file, its account, merging into the cache |
| `core::burn::{rates, plan, decide, tied, cost, replay, live, burn_next}` | auto-burn: rhythm and rates, slack and the two plans, the decision, thinking across a switch, switch cost, replay, one read of the history into the plan's inputs, and the status line's next-account file |
| `app::statusline` (`mod`, `render`, `width`) | Claude Code's payload, the line's facts, rendering and terminal width |
| `app::burn_text` | every word auto-burn shows, for the tray app and the CLI |
| `app::gui::burn_rt` | the tray app's auto-burn loop: learn, decide, switch, view |

## statusline — layout types in store.rs (step B)

`Settings.statusline` and the account's `auto_burn` live in `store.rs`; the layout types are the
same file (spec §7.6).

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct StatuslineConfig {
    #[serde(default = "default_statusline_left")]  pub left: Vec<Segment>,
    #[serde(default = "default_statusline_right")] pub right: Vec<Segment>,
    #[serde(default = "default_statusline_margin")] pub margin: u16,
}
#[derive(Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct Segment {
    pub kind: SegmentKind,
    #[serde(default = "default_color_rule")] pub color: ColorRule,
    #[serde(default, skip_serializing_if = "String::is_empty")] pub prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")] pub suffix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub drop: Option<u8>, // 1 dropped first; None never
    #[serde(default, skip_serializing_if = "Option::is_none")] pub time_format: Option<TimeFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub text: Option<String>,
}
#[serde(rename_all = "camelCase")]
pub enum SegmentKind { Account, Plan, FiveHourPercent, FiveHourReset, WeekPercent, WeekReset,
    ModelPercent, AutoBurn, Model, Effort, Folder, GitBranch, UserHost, Clock, Text, Separator }
#[serde(tag = "rule", rename_all = "camelCase")]
pub enum ColorRule { None, Fixed { color: String }, ByLoad { warn: u32, bad: u32 },
    ByModelFamily, ByEffort, Account }
#[serde(rename_all = "camelCase")]
pub enum TimeFormat { CompactLeft, Seatswap, Clock }
impl StatuslineConfig { pub fn current_line() -> StatuslineConfig; } // the preset "My current line"
fn validate_statusline(config: &StatuslineConfig) -> Result<(), StoreError>;
// Fixed colour is #rrggbb; ByLoad needs warn < bad <= 100; drop is never 0; a Text segment has text.
```

## feed.rs — the status line's live numbers (step B)

```rust
// account_for matches with history::same_reset, whose RESET_JITTER is 60 seconds.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct FeedLimit { pub percent: f64, pub resets_at: DateTime<Utc> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct Feed { pub at: DateTime<Utc>, pub five_hour: Option<FeedLimit>,
    pub seven_day: Option<FeedLimit>, pub model: Option<String> }

/// Write <data>/feed.json when its numbers differ from the file's or the file is at least
/// 10 s older than feed.at. A file that does not parse is overwritten by this call.
pub fn write_if_due(app: &AppPaths, feed: &Feed) -> io::Result<bool>;
/// Missing file → None; a file that does not parse → InvalidData.
pub fn read(app: &AppPaths) -> io::Result<Option<Feed>>;
/// The one stored account whose cached weekly_all reset is within RESET_JITTER of
/// feed.seven_day.resets_at. None without a seven_day, or when none or more than one matches.
pub fn account_for(feed: &Feed, cache: &UsageCache, accounts: &[Account]) -> Option<String>;
/// usage with the session and weekly-all numbers taken from the feed when it is not older: the
/// session when its reset is new or the same window's percent is not lower, and never when its
/// reset is at or before feed.at; the weekly percent when it is not lower. The week's reset,
/// per-model limits and flags stay.
pub fn merged(usage: &Usage, feed: &Feed) -> Usage;
```

`feed.json` is the shape above. `AppPaths::feed_file()` is `<data_dir>/feed.json`. The status line
writes it; the tray app only reads it.

## burn/mod.rs — constants and shared types (step B)

```rust
pub const LEARN_MIN_WU: f64 = 20.0;    // rates are known from this much work on an account
pub const RHYTHM_WEEKS: i64 = 4;       // the rhythm averages 4 weeks; the newer two count double
pub const RATES_DAYS: i64 = 14;        // rates are learned from this many days of samples
pub const STEP_MIN: i64 = 15;          // the plan's step
pub const HORIZON_H: i64 = 168;        // the plan's horizon
pub const MIN_DWELL_MIN: i64 = 30;     // shortest time between two switches
pub const SLACK_MARGIN_H: f64 = 2.0;   // S2's margin, in typical working hours
pub const MANUAL_HOLD_H: i64 = 4;      // a manual switch holds early switching this long
pub const MIN_GAIN_H: f64 = 0.5;       // early switching must save at least this
pub const GAIN_FACTOR: f64 = 2.0;      // and at least this many times its extra switches' cost
pub const MIN_COST_SAMPLES: u32 = 3;   // a measured switch cost needs this many switches
pub const FRESH_MIN: i64 = 20;         // a decision needs readings at most this old
pub const SWITCH_RELEARN_MIN: i64 = 1; // a switch event that lands late is learned at the next tick
pub const STALE_ASK_SECS: i64 = 60;    // stale readings ask for a poll at most this often
pub const LEARN_RETRY_MIN: i64 = 5;    // a failed history read is tried again this much later
pub const SWITCH_RETRY_MIN: i64 = 10;  // the same switch decision is sent again this much later

const OUTPUT_WEIGHT: f64 = 5.0;        // the four weights of a work unit, nowhere else
const CACHE_WRITE_WEIGHT: f64 = 1.25;
const CACHE_READ_WEIGHT: f64 = 0.1;
const HOURS: usize = 168;

pub fn work_units(input: u64, output: u64, cache_write: u64, cache_read: u64) -> f64;
/// Work units as hours at `typical_wu`; None when that is 0.0. The one conversion
/// used by the regime, `burn status`, `burn plan` and `burn_text::hours`.
pub fn wu_hours(wu: f64, typical_wu: f64) -> Option<f64>;
#[derive(Debug, Clone, PartialEq)] pub struct Rhythm { pub wu: [f64; HOURS] }
impl Rhythm { pub fn at(&self, t: DateTime<Utc>, offset: FixedOffset) -> f64;
              pub fn typical_hour(&self) -> f64; } // mean over hours with wu > 0; 0.0 if none
#[derive(Debug, Clone, Copy)] pub struct Rates { pub c: f64, pub s: Option<f64> }
```

## burn/burn_next.rs — the status line's next account (step B)

```rust
pub const REWRITE_AFTER_MIN: i64 = FRESH_MIN / 2;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct BurnNext { pub account: String, pub at: DateTime<Utc>, pub written: DateTime<Utc> }
pub fn write(app: &AppPaths, next: &BurnNext) -> io::Result<()>; // atomic, private, mode 0600
pub fn read(app: &AppPaths, now: DateTime<Utc>) -> io::Result<Option<BurnNext>>;
pub fn remove(app: &AppPaths) -> io::Result<()>;                 // a missing file is success
```

`AppPaths::burn_next_file()` is `<data_dir>/burn-next.json`. `read` returns `None` when the file is
missing or its `written` is older than `FRESH_MIN`; an unparsable file is `InvalidData`, which the
status line prints as its `seatswap: <reason>` line. `REWRITE_AFTER_MIN` is the tray app's
heartbeat: it rewrites the same account and time at least this often so a running app stays fresh.

## burn/rates.rs — rhythm, live account, rates (step B)

```rust
pub fn rhythm(tokens: &[Hosted<TokenRecord>], now: DateTime<Utc>, offset: FixedOffset) -> Rhythm;
pub fn account_at(events: &[Hosted<Event>], host: &str, t: DateTime<Utc>) -> Option<AccountRef>;
pub fn rates(account: &AccountRef, samples: &[Hosted<Sample>], tokens: &[Hosted<TokenRecord>],
             events: &[Hosted<Event>], now: DateTime<Utc>) -> Option<Rates>;
```

`rhythm` spreads a record's hour over the local clock hours it covers. `rates` pairs consecutive
samples of the same weekly window at most 6 h apart in the last 14 days, counts the work of token
records whose host had the account live, and returns `None` until the week's work reaches
`LEARN_MIN_WU`; `s` is `None` for an uncapped account or too little session work. A pair whose
earlier sample is at 100 % is left out; a rate at or below 0 is unknown (`None`), never a number.

## burn/plan.rs — slack, choice and the two plans (step B)

```rust
#[derive(Debug, Clone, PartialEq)] pub struct SessionState { pub percent: f64, pub resets_at: Option<DateTime<Utc>> }
#[derive(Clone)] pub struct AccountState { pub id: String, pub may_use: bool, pub login_ok: bool,
    pub week_percent: f64, pub week_resets_at: DateTime<Utc>, pub session: Option<SessionState>,
    pub model_blocked: bool, pub rates: Option<Rates> }
#[derive(Clone)] pub struct Inputs { pub now: DateTime<Utc>, pub offset: FixedOffset,
    pub accounts: Vec<AccountState>, pub active: Option<String>, pub rhythm: Rhythm }
impl Inputs { pub fn check(&self) -> Result<(), PlanError>; }
pub enum PlanError { InvalidPercent{account,limit,percent}, SessionWithoutReset{account,percent},
    UnknownActive{id}, DuplicateAccount{id}, LastSwitchAfterNow{at,now}, OldestReadingAfterNow{at,now} }
pub fn blocked(account: &AccountState, at: DateTime<Utc>) -> bool;
pub fn usable(account: &AccountState, at: DateTime<Utc>) -> bool;       // may_use && !blocked
pub fn usable_again(account: &AccountState, at: DateTime<Utc>) -> Option<DateTime<Utc>>;
pub fn rates_known(account: &AccountState) -> bool;
pub fn slack(account: &AccountState, inputs: &Inputs) -> Option<f64>;
pub fn choose(inputs: &Inputs, at: DateTime<Utc>) -> Option<String>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Policy { Lazy, Eager }
#[derive(Debug, Clone, PartialEq, Eq)] pub struct Segment { pub account: Option<String>,
    pub from: DateTime<Utc>, pub to: DateTime<Utc> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Fill { Window, Week }
#[derive(Debug, Clone, PartialEq)] pub struct Plan { pub segments: Vec<Segment>,
    pub blocked_wu: f64, pub switches: u32, pub left_at_reset: BTreeMap<String, f64>,
    pub full_at: BTreeMap<String, DateTime<Utc>>, pub full_by: BTreeMap<String, Fill> }
pub fn follow(inputs: &Inputs, policy: Policy) -> Plan;
pub(super) fn rhythm_walk(inputs: &Inputs) -> Walk<'_>; // the plan and `decide::latest_reaching`
```

`follow` plays 168 h in 15-minute steps. Lazy stays on the current account until it is not usable,
then calls `choose`; eager also moves when `choose` names another usable account whose slack is at
least `SLACK_MARGIN_H` × the typical hour below the current one's and the last switch is at least
`MIN_DWELL_MIN` ago. An account without rates is played with `c = 0` and `s = 0`: it never fills
and never causes an eager move. An active account with `may_use` false stays until it is blocked.
`choose` is the usable account with the least slack, ties by the earlier weekly reset and then id;
when any usable account has no rates it uses the fixed order instead (capped accounts with an open
window first, then the rest, each by earlier reset and id).

## burn/decide.rs — the regime and the decision (step B)

```rust
#[derive(Debug, Clone, Copy, PartialEq)] pub struct SwitchCost { pub wu: f64, pub samples: u32 }
#[derive(Clone)] pub struct Situation { pub inputs: Inputs, pub cost: Option<SwitchCost>,
    pub last_switch_at: Option<DateTime<Utc>>, pub manual_hold_until: Option<DateTime<Utc>>,
    pub tied_busy: u32, pub oldest_reading: DateTime<Utc>, pub sessions_off: bool }
impl Situation { pub fn check(&self) -> Result<(), PlanError>; }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum RegimeWhy { Learning, SessionsOff, NoCostYet, NotWorthIt, Pays }
#[derive(Debug, Clone, PartialEq)] pub struct Regime { pub early: bool, pub why: RegimeWhy,
    pub lazy_blocked_h: f64, pub eager_blocked_h: f64, pub extra_switches: u32, pub cost_h: Option<f64> }
pub fn regime(situation: &Situation) -> Regime;
#[derive(Debug, Clone, PartialEq, Eq)] pub enum Reason { NoneActive,
    ActiveFull { until: Option<DateTime<Utc>> }, WindowOpen, ResetNear }
#[derive(Debug, Clone, PartialEq, Eq)] pub enum Decision { Stay, Stale,
    Switch { to: String, reason: Reason },
    Wait { to: String, latest: DateTime<Utc>, tied_busy: u32, reason: Reason },
    AllFull { next: Option<(String, DateTime<Utc>)> } }
pub fn decide(situation: &Situation, regime: &Regime) -> Decision;
```

`decide` in this order: a reading older than `FRESH_MIN` → `Stale`; the active account blocked or no
active account → `choose` names a target → `Switch` (`NoneActive` when there was none, otherwise
`ActiveFull`), else `AllFull`; a manual hold in the future → `Stay`; early switching on, an active
account auto-burn may use, `choose` names another account with enough less slack, and the dwell has
passed → `Switch` (`WindowOpen` when the target has a 5 h limit, else `ResetNear`), or `Wait` when
a thinking-tied session is busy and the target still has slack; otherwise `Stay`. A target slack of
0 or less switches at once. `regime.early` is `why == Pays`; it needs rates for every account
auto-burn may use, a typical hour, at least `MIN_COST_SAMPLES`, and both gain rules of
`docs/spec.md` §7.4.1. When no rates were learned because `readSessions` is off, `why` is
`SessionsOff`, not `Learning`; the fact travels in `Situation.sessions_off`. `latest_reaching`
walks `plan::rhythm_walk`, so the waiting time and the plan cannot count work differently.

## burn/tied.rs — thinking across a switch (step B)

```rust
pub fn thinking_tied(model_id: &str) -> bool;
```

The ids are: tied `claude-fable-5-2`, `claude-mythos-5-2`,
`claude-sonnet-5-5`, `claude-haiku-5-5`; not tied `claude-opus-5-5`, `claude-opus-5`,
`claude-fable-5-1`, `claude-mythos-5-1`, `claude-fable-5`, `claude-mythos-5`, `claude-sonnet-5`,
and every id starting with `claude-opus-4`, `claude-sonnet-4`, `claude-haiku-4`, `claude-3`. One
trailing `[...]` suffix such as `[1m]` is stripped; any other id is tied.

## burn/cost.rs — what a switch costs (step B)

```rust
pub fn switch_cost(events: &[Hosted<Event>], samples: &[Hosted<Sample>],
                   tokens: &[Hosted<TokenRecord>], rates: &BTreeMap<AccountRef, Rates>,
                   range: &Range) -> Option<SwitchCost>;
```

For each `Switch` event in `range`: the target's weekly percent gained between its first and last
sample in the 10 minutes after the switch, minus `c` × the work units done between those samples,
divided by `c` (extra work units). Switches without two samples or without the target's rates are
left out; a negative result counts as 0. The result is the mean and its sample count, `None` when
none remains.

## burn/replay.rs — replay recorded work (step B)

```rust
#[derive(Debug, Clone, PartialEq)] pub struct ReplayResult { pub policy: Policy,
    pub worked_wu: f64, pub blocked_wu: f64, pub switches: u32,
    pub expired: BTreeMap<AccountRef, f64>, pub typical_wu: f64, pub warnings: Vec<String> }
#[derive(Debug, thiserror::Error)] pub enum ReplayError {
    History(#[from] HistoryError), Plan(#[from] PlanError),
    Unplayable { account: String, at: DateTime<Utc>, missing: &'static str } }
pub fn replay(history: &HistoryDir, range: &Range, rates: &BTreeMap<AccountRef, Rates>,
              without: &[AccountRef], policy: Policy) -> Result<ReplayResult, ReplayError>;
```

The replay reads the token and sample records of `range` from all hosts. Each account starts from
its first sample in the range; a sample without a weekly limit or a weekly reset is
`ReplayError::Unplayable`. Work is recorded per UTC hour and spread evenly over its four steps.
`without` leaves accounts out; the replay starts with no active account. Ids inside the play are
`"<acc>/<org>"`; `expired` names the full `AccountRef` again. `typical_wu` is the mean of the
recorded hours with work (0.0 when none); the reads' warnings are kept for the caller.

## burn/live.rs — one history read into one situation (step B)

```rust
#[derive(Debug, Clone)] pub struct Learned { pub rhythm: Rhythm,
    pub rates: BTreeMap<AccountRef, Rates>, pub cost: Option<SwitchCost>,
    pub warnings: Vec<String>, pub events: Vec<Hosted<Event>>, pub host: String,
    pub sessions_off: bool }
pub enum Readiness { Ready(Situation), Unread { accounts: Vec<String> }, NoAccounts }
pub fn learn(history: &HistoryDir, now: DateTime<Utc>, offset: FixedOffset,
             accounts: &[Account], read_sessions: bool) -> Result<Learned, HistoryError>;
pub fn situation(learned: &Learned, views: &[AccountView], accounts: &[Account],
                 active: Option<&str>, now: DateTime<Utc>, offset: FixedOffset,
                 tied_busy: u32) -> Result<Readiness, PlanError>;
pub fn next_account(situation: &Situation, decision: &Decision, plan: &Plan) -> Option<String>;
pub struct Assessment { pub situation: Situation, pub regime: Regime, pub decision: Decision,
                        pub plan: Plan, pub next: Option<String> }
/// regime → decide → follow(Lazy) → next. `cached_regime` is an earlier regime; None computes one.
pub fn assess(situation: Situation, cached_regime: Option<&Regime>) -> Assessment;
```

`learn` reads 4 weeks of samples and tokens and 14 days of events from all hosts, computes the
rhythm, the rates of `accounts` and the switch cost; `read_sessions` off means no rates are learned
and `sessions_off` is set. `situation` puts every account with a reading
into `Inputs` (`may_use` = the account's `auto_burn`, `login_ok` = `loginState == "ok"`), and returns
`Unread` when an account auto-burn may use, or the active one, has no reading and is not logged
out. The manual hold
is the latest of any non-auto switch's `MANUAL_HOLD_H` and an `autoBurn` pause; a resume clears the
manual hold; a manual hold ends at the first block of the account switched to, from the history or
the state now, and stays ended. `next_account` is the target of a
`Switch`/`Wait`, otherwise the first account of the lazy plan other than the active one.

## engine.rs additions — feed, stale poll, 30 s near full (step B)

```rust
pub(crate) const NEAR_FULL_POLL_SECS: u64 = 30;
pub enum PollReason { Timer, PanelOpened, AfterSwitch, Wake, Stale }
pub struct CacheEntry { …, #[serde(default)] pub fed_at: Option<DateTime<Utc>> }
impl<R: Remote> Engine<R> {
    pub fn apply_feed(&mut self, feed: &Feed, now: DateTime<Utc>) -> Result<Option<String>, EngineError>;
}
```

`apply_feed` matches the feed's account with `feed::account_for`, ignores a feed at or before the
entry's `fetched_at` or `fed_at`, merges with `feed::merged`, writes the cache and, through the
recorder, one `Sample` with `src: statusline`. `AccountView.fetched_at` is the later of
`fetched_at` and `fed_at`. `PollReason::Stale` polls only an account whose last reading or attempt
is older than `FRESH_MIN`; the interval timer is not used for it. `NEAR_FULL_POLL_SECS` replaces
`pollActive` while the active account's binding limit is at least 90 % and below 100 %.

## app: statusline (step B)

```rust
pub(crate) struct Payload { workspace: Option<Workspace>, model: Option<Model>, effort: Option<Effort>,
    rate_limits: Option<RateLimits> }
pub(crate) struct Workspace { current_dir: Option<String> }
pub(crate) struct Model { id: Option<String>, display_name: Option<String> }
pub(crate) struct Effort { level: Option<String> }
pub(crate) struct RateLimits { five_hour: Option<RateLimit>, seven_day: Option<RateLimit> }
pub(crate) struct RateLimit { used_percentage: f64, resets_at: i64 }
pub(crate) fn parse(stdin: &[u8]) -> Payload;                 // any parse error → Payload::default()
pub(crate) fn feed_from_payload(payload: &Payload, now: DateTime<Utc>) -> Option<Feed>;
pub(crate) fn publish_feed(app: &AppPaths, settings: &Settings, payload: &Payload,
                           now: DateTime<Utc>) -> Result<(), String>;
pub(crate) fn facts(app: &AppPaths, payload: &Payload, file: &AccountsFile, live: &LiveState,
                    cache: &UsageCache, home: &Path, user: String,
                    now: DateTime<Utc>) -> Result<Facts, String>;

pub(crate) struct Facts { pub user: String, pub host: String, pub home: String,
    pub now: DateTime<Utc>, pub account: Option<AccountFacts>, pub five_hour: Option<Limit2>,
    pub week: Option<Limit2>, pub model_limits: Vec<(String, f64)>,
    pub burn_next: Option<(String, DateTime<Utc>)>, pub git_branch: Option<String> }
pub(crate) struct AccountFacts { pub abbr: String, pub color: String, pub plan: Option<String> }
pub(crate) struct Limit2 { pub percent: f64, pub resets_at: Option<DateTime<Utc>> }

pub(crate) fn render<Tz: TimeZone>(config: &StatuslineConfig, payload: &Payload, facts: &Facts,
    width: Option<u16>, tz: &Tz) -> String where Tz::Offset: Display;
pub(crate) fn visible_width(text: &str) -> usize;
pub(crate) fn compact_left(seconds: i64, week: bool) -> String; // also used by burn_text::duration
pub(crate) fn clock(at: DateTime<Utc>, now: DateTime<Utc>, offset: FixedOffset) -> String; // §7.4.5
pub(crate) fn weekday_clock(at: DateTime<Utc>, offset: FixedOffset) -> String;            // §7.4.5
pub(crate) fn git_branch(cwd: &Path) -> Option<String>;
pub(crate) fn terminal_width() -> Option<u16>;
```

`facts` shows the payload's numbers only when the feed belongs to the live account; without a
matching feed it shows the cache. `burn_next` comes from `burn_next::read(app, now)` mapped through
the stored accounts to their abbreviations: a missing file, an unknown account id or an old `written`
is `None`, and an unreadable file is the line's error. `publish_feed` does nothing when
`statusline_feed` is off or the payload
has no `rate_limits`/`seven_day`; when `write_if_due` wrote, it sends `feed` to the tray app (no
socket is not an error). `render` is pure; `facts` is the one part that reads `.git/HEAD`. `width`
walks the `/proc` ancestors' pseudo-terminals (Linux, `rustix::termios::tcgetwinsize`) and falls
back to `$COLUMNS`, else `None`. `format::clock` is the local `HH:MM` today and `Mon HH:MM` when not
today; `weekday_clock` is always `Mon HH:MM`; both are the §7.4.5 texts and moved here from
`burn_text` so the line and the auto-burn words share them. `compact_left` rounds minutes up and
drops a zero smaller unit (`2h`, not `2h00m`); `week` chooses days/hours, otherwise hours/minutes.

## app: burn_text (step B)

```rust
#[derive(Debug, thiserror::Error)] pub enum TextError { ViewMissing { account: String },
    ResetMissing { account: String }, FillMissing { account: String }, NoActive }
#[derive(Debug, Clone, PartialEq, Eq, Serialize)] #[serde(rename_all = "camelCase")]
pub struct SwitchSuggestion { pub to: String, pub text: String }
pub fn abbr(views: &[AccountView], id: &str) -> Result<String, TextError>;
pub fn unread_line(views: &[AccountView], accounts: &[String]) -> Result<String, TextError>;
pub fn no_accounts_line() -> &'static str;
pub fn why(why: RegimeWhy) -> &'static str;
pub fn segment_line(account: Option<&str>, views: &[AccountView], from: DateTime<Utc>,
                    to: DateTime<Utc>, now: DateTime<Utc>, offset: FixedOffset) -> Result<String, TextError>;
pub fn policy_name(policy: Policy) -> &'static str; // "Lazy" | "Eager"
pub fn replay_report(result: &ReplayResult, accounts: &[Account]) -> Result<String, TextError>;
pub fn reason(reason: &Reason, target: &str, active: Option<&str>, views: &[AccountView],
              now: DateTime<Utc>, offset: FixedOffset) -> Result<String, TextError>;
pub fn line(plan: &Plan, active: Option<&str>, views: &[AccountView], learning: &[String],
            now: DateTime<Utc>) -> Result<Option<String>, TextError>;        // feature "gui"
pub fn planned(plan: &Plan, active: Option<&str>) -> BTreeMap<String, DateTime<Utc>>; // feature "gui"
pub fn switch_notification(decision: &Decision, active: Option<&str>, views: &[AccountView],
    now: DateTime<Utc>, offset: FixedOffset) -> Result<Option<String>, TextError>; // feature "gui"
pub fn suggestion(decision: &Decision, plan: &Plan, active: Option<&str>, views: &[AccountView],
    now: DateTime<Utc>, offset: FixedOffset) -> Result<Option<SwitchSuggestion>, TextError>;
```

Every text is here; the tray app and the CLI do not keep a second copy. Missing values are
`TextError`, never a guessed abbreviation or time. `unread_line` is
`Waiting for a first reading of AC, LB.` in account order; `no_accounts_line` is the fixed
`No account auto-burn may use has a working login.` `why(SessionsOff)` is
`reading sessions is off: no rates are learned`. `suggestion` returns `Some` only for `Switch`
and `Wait`, and adds the "Staying on … is expected to let … expire" sentence only when the lazy
plan leaves more than 1 % of the target's week at its reset.

## app: auto-burn in the tray app (`gui/burn_rt.rs`, step B)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)] #[serde(rename_all = "camelCase")]
pub(crate) struct WaitingSwitch { pub to: String, pub latest: DateTime<Utc>, pub tied_busy: u32 }
#[derive(Debug, Clone, Default, Serialize)] #[serde(rename_all = "camelCase")]
pub(crate) struct BurnView { pub on: bool, pub paused_until: Option<DateTime<Utc>>,
    pub early: bool, pub why: String, pub learning: Vec<String>, pub line: Option<String>,
    pub planned: BTreeMap<String, DateTime<Utc>>, pub waiting: Option<WaitingSwitch>,
    pub suggestion: Option<SwitchSuggestion> }
pub(crate) struct BurnTick<'a> { pub app: &'a AppPaths, pub history: Option<&'a HistoryDir>,
    pub views: &'a [AccountView], pub accounts: &'a [Account], pub settings: &'a Settings,
    pub active: Option<&'a str>, pub now: DateTime<Utc>, pub offset: FixedOffset,
    pub tied_busy: u32 }
#[derive(Debug, Default)] pub(crate) struct BurnReport { pub view: Option<BurnView>,
    pub error: Option<String>, pub warnings: Vec<String>, pub notification: Option<String> }
pub(crate) struct BurnRuntime { /* learned, last learn/regime, last sent switch, view */ }
impl BurnRuntime {
    pub(crate) fn new(action: Sender<ActionCommand>, engine: Sender<EngineCommand>) -> BurnRuntime;
    pub(crate) fn force_learn(&mut self);
    pub(crate) fn after_snapshot(&mut self, tick: &BurnTick<'_>) -> BurnReport;
}
```

`after_snapshot` learns at the first tick, once an hour, after a switch, after a pause, when the
active account changes, and `LEARN_RETRY_MIN` minutes after a failed read. It then builds the
situation and calls `live::assess` with the cached regime when the regime is not due, so the tray
app, `burn status` and `burn plan` take the same steps in the same order. Only `auto_burn` on sends
`ActionCommand::Switch { by: By::Auto, reason: Some(text) }`;
`Wait` fills `view.waiting`; `AllFull` is left to the step A notification; `Stale` asks
`EngineCommand::Poll(PollReason::Stale)`, at most once per `STALE_ASK_SECS`. An `Unread` situation
keeps the strip with `burn_text::unread_line` as its line and still asks the stale poll;
`NoAccounts` keeps it with `burn_text::no_accounts_line`. With `auto_burn` off and
`suggest_switches` on, `Switch` and `Wait` fill `view.suggestion`. The switch is retried after
`SWITCH_RETRY_MIN` minutes; the notification is shown only after the reply says the switch landed.
`publish_next` writes `<data>/burn-next.json` (`Switch`: `to` and now; `Wait`: `to` and `latest`;
otherwise the lazy plan's next account and its first use), removes it when auto-burn is off,
paused or nothing is planned, and rewrites at least every `burn_next::REWRITE_AFTER_MIN` minutes.
The runtime remembers the warnings it printed with `worker::PrintedWarnings`, shared rather than a
second `BTreeSet`. `BurnView` is part of `Snapshot.burn`; the
settings toggle calls `worker::save_setting` and writes `EventKind::AutoBurnSet`, the pause calls
`ActionCommand::PauseBurn { hours }` and writes `EventKind::AutoBurn`.

## app: CLI — `burn` and `status` (step B)

`seatswap statusline` is handled before the context is opened: it never opens the store's secret
backend, takes no lock and writes no history. The other additions:

- `seatswap status --json` reads the history once through `live::learn` and `live::situation` and
  assembles through `live::assess`; a history or planner error becomes `burnError`, never a failed
  command. The plain, waybar and i3blocks forms do not read the history.
- `seatswap burn plan [--json]` uses the same read and `live::assess` with `tied_busy = 0` (the CLI
  has no session count). Blocked hours come from `wu_hours` on the lazy and eager `blocked_wu`. An
  account auto-burn may use (or the active one) with a login and no reading is an error
  `no reading for …`; no account at all is `no account has a login with a reading`.
- `seatswap burn simulate --from D --to D [--without ABBR]…` maps the two local days to a range
  ending at the local midnight after `--to`, learns the rates from the history as of now, and
  replays `Policy::Lazy` and `Policy::Eager`. Both replays read the same files; each warning is
  printed once on standard error.
- `seatswap next` by an agent with `auto_burn` on returns
  `seatswap: auto-burn is on and decides` and exit code 0 without switching or touching a login;
  the agent call is recorded like any other.
- The gate: `statusline` is allowed for every caller and never recorded; `burn plan` and
  `burn simulate` follow `agents_read` like `list` and `status`.
- The bundled `agent-skill` text adds the `views[].slackHours`, `autoBurn` and `next` rows and
  rule 1: "If `autoBurn.on` is true, do not switch: auto-burn decides."

### `status --json`

```json
{"views":[{"id":"…","label":"…","abbr":"AC","color":"#3b82f6","email":null,"plan":"Team",
  "autoBurn":true,"isActive":true,"loginState":"ok","loginEndsAtMs":null,"loginDaysLeft":null,
  "usage":{…},"usable":true,"blockedUntil":null,"fetchedAt":"2026-11-15T09:41:12Z",
  "lastError":null,"slackHours":3.5}],
 "active":"…","suggestion":{"toId":"…","text":"Switch to AC now: …"},
 "autoBurn":{"on":false,"pausedUntil":null},"next":"…","burnError":null}
```

`views[]` is `AccountView` serialized flat with the same fields as `list --json`, plus
`slackHours` only when known. `blocker` is `#[serde(skip)]` and never appears. `suggestion` is
`{toId, text}` or null and only when auto-burn is off and `suggestSwitches` is on. `next` is the
`live::next_account` answer or null. `burnError` is null unless the history read failed; then
`next` is null and no view has `slackHours`.

### `burn plan --json`

```json
{"regime":{"early":true,"why":"switching before 100 % pays","blockedLazyH":2.0,
  "blockedEagerH":0.0,"extraSwitches":4,"costH":0.1},
 "segments":[{"account":"…","from":"2026-11-15T07:00:00Z","to":"2026-11-15T09:00:00Z"},
             {"account":null,"from":"…","to":"…"}],
 "leftAtReset":{"…":12.0}}
```

`blockedLazyH` and `blockedEagerH` are null when the rhythm has no typical hour (no hours can be
computed); `costH` is also null while no switch cost is known. `account` is null for a stretch with
nothing usable. The text form is the `why` words followed by one `segment_line` per segment.

## app: worker and panel additions (step B)

```rust
pub(crate) enum EngineCommand { …, Feed, HistoryChanged } // Feed: apply feed.json; HistoryChanged: learn again
pub(crate) enum ActionCommand { …, PauseBurn { hours: Option<i64> } } // Some: pause; None: resume
#[tauri::command] pub(crate) async fn set_auto_burn(state: State<GuiState>, on: bool) -> Result<(), String>;
#[tauri::command] pub(crate) fn pause_burn(state: State<GuiState>, hours: Option<i64>) -> Result<(), String>;
```

`SaveSetting` merges one field into the stored settings; only a change of `auto_burn` writes
`EventKind::AutoBurnSet`. `PauseBurn` refuses `hours <= 0`, writes `EventKind::AutoBurn { on, paused_until }` and sends
`EngineCommand::HistoryChanged`. The engine reads `feed.json` on `Feed` and calls `apply_feed`; it
does the same once at start, so a feed written while no tray app ran is not lost. The pop-up's
`set_auto_burn` sends `SaveSetting { field: "autoBurn", value: on }` (one write path), and `pause_burn`
is the Pause 4 h / Resume button.

## Files step B writes

- **`<data folder>/feed.json`** (§7.6): atomic, mode 0600, written by `seatswap statusline` only
  when `statuslineFeed` is on, the payload has a week, and the numbers changed or the file is 10 s
  old. The tray app only reads it.
- **`<data folder>/burn-next.json`** (§7.6): atomic, mode 0600, written by the tray app's auto-burn
  loop with `{"account","at","written"}`; rewritten at least every 10 minutes while a switch is
  planned, and removed when auto-burn is off, paused or nothing is planned. `seatswap statusline`
  only reads it.
- **History events** in `<root>/<host>/events-YYYY-MM.jsonl`:
  - `{"t":"…","type":"autoBurn","on":true,"pausedUntil":"2026-11-15T13:10:00Z"}` for a pause;
    `pausedUntil` is absent (not null) for a resume. `on` is the setting at that moment.
  - `{"t":"…","type":"autoBurnSet","on":true}` when a settings save turns auto-burn on or off.
    It is not a pause or a resume.
  - every switch, including an automatic one, is a `switch` event with `by` (`"user"`, `"cli"`,
    `"auto"`, `"agent"` or `"external"`) and, for an automatic switch, the reason text in
    `reason`, plus `sessions` when known: `{"t":"…","type":"switch","to":{"acc":"…","org":"…"},
    "by":"auto","reason":"AC is full until 14:40","sessions":{"running":3,"busy":1,
    "tied":1}}`. `from`, `cwd`, `waitedSecs` and `tied` are omitted when unknown.
- **History samples**: a merged feed writes a `sample` line with `"src":"statusline"`; its limits,
  account and time are the same shape a poll writes.


## Step C1: the large window, Settings, money and projects

The new modules, each with one job:

| Module | Job |
|---|---|
| `app::gui::window` | the large window's label, areas, target grammar and opener |
| `app::gui::lookup_rt` | the look-up thread: models.dev and Frankfurter, the money view, own prices |
| `core::money` | API prices with dates, the dollar rate, the look-ups, the value of token records |
| `core::projects` | projects, joins and organizations from the session records and the shared project log |
| `app::ui::common.js` | the page helpers both windows use |
| `app::ui::window.js`, `areas/settings.js` | the window's tabs and areas, the Settings page |

## gui/window.rs and the page helpers (step C1)

```rust
#[cfg(feature = "gui")]
pub(crate) const WINDOW_LABEL: &str = "main";
pub(crate) const AREAS: [&str; 4] = ["sessions", "statistics", "statusline", "settings"];

/// "<area>" or "<area>/<sub>", the sub optionally "<name>?<item>"; a-z, 0-9 and '-' only.
pub(crate) fn parse_target(target: &str) -> Result<(&str, Option<&str>), String>;

/// Create (1280 × 800, resizable, decorated, in the taskbar, not always on top) or focus
/// the window, and send it the target as `open-area` (an existing window) or as its
/// address `window.html#<target>` (a new one). A refused target is the error text.
#[cfg(feature = "gui")]
pub(crate) fn show(app: &AppHandle, target: &str) -> Result<(), String>;
#[cfg(feature = "gui")]
#[tauri::command]
pub(crate) fn open_window(app: AppHandle, target: String) -> Result<(), String>;
```

`parse_target` accepts `settings`, `settings/accounts`, `settings/accounts?<id>`; it refuses an
unknown area, an empty sub (`settings/`), a second `/`, a character outside `a-z 0-9 -` (also in the
item after `?`), and an empty item. `show` refuses the same targets before it touches a window.
The pop-up keeps the event name `navigate`; the large window has `open-area`, because `emit`
reaches every window.

The CLI and the IPC use the same grammar: `ipc::window_line(target)` is `window <target>`;
`ipc::send_window` writes that line and `parse_command` accepts it only when `parse_target` does.
`Command::Window { target: Option<String> }` defaults to `sessions` and validates before sending.
`seatswap panel settings` sends `window settings` (`cli::panel_settings_line`); the tray menu's
"Settings…" and "Open seatswap…" call `show(app, "settings")` and `show(app, "sessions")`.

`crates/app/ui/common.js` is the pop-up's helpers moved out of `panel.js`, plus the controls both
pages use. Exported: `PALETTE`, `TRAY_GREY`, `LEVEL_COLORS`, `TAURI_GLOBAL`, `HAS_TAURI`, `isObject`,
`clamp`, `clone`, `errorMessage`, `isColor`, `hexToRgb`, `rgba`, `rgbToHex`, `mixHex`,
`relativeLuminance`, `contrastText`, `limitLevel`, `shortPercent`, `clear`, `setStyles`, `applyProps`,
`appendChildren`, `h`, `banner`, `applyTheme`, `invoke`, `listen`, `money`, `tokens`, `textInput`,
`selectInput`, `switchButton`, `sanitizeAbbrChar`, `abbrLetterBoxes`, `colorSwatches`, `hexField`,
`iconPreview`. `money(amount, currency)` gives `"1,234 EUR"` for `(1234, "EUR")` and two decimals
below 10, `"6.47 EUR"`, and throws on a negative or non-finite amount or a currency that is not
three capital letters; `tokens(n)` returns `"12.4 M"`, `"845 k"` or `"0"` and throws on an invalid
count. `window.html`, `window.css` and `window.js` are the second page; `window.js` reads the area
from `location.hash`, draws the tabs, imports `./areas/<area>.js` on demand and calls its
`render(container, { sub, snapshot, invoke, listen, fromOpen })`. A module that does not exist yet
shows "Comes with a later version."; a module that throws shows its error in the area. A `snapshot`
calls the current area's `update(snapshot)` when it has one and has rendered, and `render` only for
an area without `update`. The old content stays until the area replaces it. `fromOpen` is true when
an `open-area` request caused the render, so an item it names is applied even when the address
already names the same item. `crates/app/capabilities/default.json` names both windows:
`"windows": ["panel", "main"]`.

## The Settings page and its save queue (step C1)

`areas/settings.js` exports `render(container, context)` and `update(snapshot)`; the seven groups
are `general`, `accounts`, `auto-burn`, `notifications`, `projects`, `agents`, `history`. The
address is `settings/<group>[?<item>]`, with `groupFrom` accepting the aliases `gen`, `acc`,
`autoburn`/`burn`, `noti`, `proj`, `hist`; an item opens that account's editor (and an `open-area`
request applies it even when the address already names it). A group click writes
`settings/<group>` and the `hashchange` redraws it.

`update(snapshot)` stores the snapshot and its settings the way `takeSettings` does, then redraws
the open group in place from the stored views. It sends no save. While an input, select or textarea
inside the area has focus it draws nothing; the next snapshot after that field lost focus draws.
The only view it reads is the projects view, when the snapshot's list of account ids changed and
the Accounts group is open. `render` reads money (`loadMoney`), the
price list, projects (`loadProjects`), history (`loadHistory`), the agent skill and the agent calls
only when their group is opened. A save or edit that changes a view (the money rows, an own price,
a project edit, a saved host setting, the agent skill) reads that view again; `listenMoney` handles
`UiEvent::Money`, and the price list is also read then while it is open. Every view keeps a request
token so a slow reply cannot overwrite a newer one (`moneyToken`, `priceToken`, `projectsToken`,
`historyToken`, `skillToken`, `callsToken`). A render or update keeps the old content until the new
one replaces it, so the area is never blank while a view loads.

**Money and the price cell.** In General → Money, `rateRow` shows the rate in effect with "looked up
{date}", "your own rate" or, for USD, "USD itself"; without a rate it says "no rate yet: API values
are shown in USD", and `state.money.rateError` is shown under that row. `ownRateRow` reads and saves
`usdRate` as `{ currency, value }` (empty saves `null`) and is disabled while no currency is set or
the currency is USD; a stored rate for another currency leaves the field empty and the row says
"your rate 0.95 was for EUR and is not used for CHF". `apiPricesRow` adds "{n} models of models.dev's
list were left out" with the entries in the line's `title` when `leftOut` is not empty.
`drawPriceList` ends with `addModelRow`: a provider select (the three `PRICE_PROVIDERS`), a model id
field and the four prices, saving through `set_price_override` with `<provider>/<model id>`, so an
own price exists even with look-ups off and no list. In the Accounts group the price cell shows the
amount with `row.priceCurrency`; when that differs from the settings' currency it shows a warning
mark whose tooltip is "in EUR: enter it again for CHF", and entering the same amount again saves it
in the new currency.

The save queue is the page's one write path, in this order:

```js
function queuedSave(run)            // every save runs on `state.saving`, one after another
function saveField(field, value)    // a setting: invokes save_setting with one field and its value
function saveAccount(account, patch, field) // invokes edit_account { key, ...patch }; the patch is one of label, abbr, color, autoBurn, price
async function saveNow(field, value) // refuses without state.settings; keeps the server's error text
```

`state.pendingSaves` counts the queued and running saves. `takeSettings(snapshot)` ignores the
snapshot's settings while it is above 0, so a poll cannot undo a save that has not answered yet.
`saveNow` sends `save_setting(field, value)` and takes `settings` from the reply as the page's copy
(`state.settings` and `state.snapshot.settings`); the reply's `warning` ("settings saved; start at
login could not be set: …") keeps the saved values and is shown under the row, because only the
autostart entry failed. A refused save leaves the error under its row, and `setting()` builds that
error line from `state.errors[field]`, so the next redraw keeps it until that field saves
successfully; the caller puts the control back to `savedValue(field)`. The worker merges one field
under the app lock (`merge_setting`): the field must be a key of the settings' camelCase JSON
object, the value replaces only it, the result is deserialised and validated, and a typed `~` or
`~/` in `claudeConfigDir` or `historyDir` is expanded with `format::expand_tilde`. `label`, the
chip's letters and colour, `autoBurn` and `price` save through `edit_account`; `price` is sent as a
number, `null` to clear it, or left out to keep it.

## store.rs additions — plan, price and the money settings (step C1)

```rust
pub struct Account {
    // …
    pub plan: Option<String>,   // plan_label(login, identity); serde default missing = None
    #[serde(default)]
    pub price: Option<f64>,     // per month, in Settings.currency; missing = None
    #[serde(default)]
    pub price_currency: Option<String>, // the currency `price` was entered in; missing = None
}
pub struct OwnRate { pub currency: String, pub value: f64 }
pub struct Settings {
    // …
    #[serde(default = "default_notify_login_ends")] pub notify_login_ends: bool, // true
    #[serde(default)] pub currency: Option<String>,                             // None
    #[serde(default)] pub usd_rate: Option<OwnRate>,                            // None
    #[serde(default = "default_look_up_prices")] pub look_up_prices: bool,      // true
}

pub struct LoginNote { pub changed: bool, pub plan_changed: bool, pub previous_plan: Option<String> }
impl Account { pub fn note_stored_login(&mut self, login: &Login) -> LoginNote; }

/// The identity's planDisplayName, else subscriptionType capitalised plus its level:
/// "_<n>x" → " <n>x" ("Team 5x"); another tier → " (<tier without default_claude_>)";
/// no tier → nothing.
pub fn plan_label(login: &Login, identity: &OAuthAccount) -> Option<String>;
/// A price is None or a finite number above 0; anything else is StoreError::InvalidPrice.
pub fn validate_price(price: Option<f64>) -> Result<Option<f64>, StoreError>;
```

`Settings::validate` refuses a currency that is not three capital letters, an `OwnRate.currency`
that is not three capital letters, and an `OwnRate.value` that is not finite and above 0. An older
`accounts.json` without `price`, `priceCurrency`, `notifyLoginEnds`, `currency`, `usdRate` or
`lookUpPrices` loads with `None`, `None`, `true`, `None`, `None`, `true`.

`switcher::edit_account` takes the price as `Option<Option<f64>>` (`None` keeps it, `Some(None)`
clears it, `Some(Some(v))` sets it after `validate_price`) and returns
`Edited { account, price_changed }`. Under the same app lock and from the same load of
`accounts.json`, setting a price stores `account.price_currency = settings.currency`; clearing it
clears the currency too. Setting a price while no currency is set is refused with
`SwitchError::PriceWithoutCurrency` ("set a currency in Settings → General before a price"), and
nothing is written. A save of the same amount counts as a change when `price_currency` differs from
the settings' currency, so a `PriceChanged` with the new currency is recorded. The panel sends
`price` as JSON: absent, `null`, or a number, because `Option<Option<f64>>` cannot tell absent from
null over JSON. The worker writes `EventKind::PriceChanged { account, amount, currency }` only when
`price_changed`, with `currency` taken from `edited.account.price_currency`. `ops::record_plan_change`
writes `EventKind::PlanChanged` from a `switcher::PlanChange { account, from, to }` after a switch
(CLI or app), a live sync or the mirror.

## api.rs additions (step C1)

```rust
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
pub const FRANKFURTER_URL: &str = "https://api.frankfurter.dev/v1/latest";
const MAX_MODELS_DEV_BYTES: usize = 32 * 1024 * 1024;

pub enum Fetched<T> { NotModified, New { etag: Option<String>, body: T } }

impl Api {
    /// GET the list with `If-None-Match: <etag>` when there is one; 200 → New with the reply's
    /// ETag; 304 → NotModified; a body over 32 MiB (Content-Length or read) is ApiError::Decode.
    pub fn models_dev(&self, etag: Option<&str>) -> Result<Fetched<serde_json::Value>, ApiError>;
    /// GET "<base>?base=USD&symbols=<currency>" and read {date, rates:{<currency>: rate}}.
    /// A currency that is not three capital letters, a missing date or rate is ApiError::Decode.
    pub fn usd_rate(&self, currency: &str) -> Result<(NaiveDate, f64), ApiError>;
    /// Tests only: point the two look-up URLs at a local server.
    pub fn with_lookup_urls(mut self, models_dev: &str, frankfurter: &str) -> Api;
}
```

Both requests carry `User-Agent: seatswap/<version>` and no other header of the account's; the
look-ups never send a token or any user data. All HTTP still goes through `Api`.

## money (step C1)

```rust
pub const PRICE_PROVIDERS: [&str; 3] = ["anthropic", "openai", "ollama-cloud"];
const MONEY_VERSION: u32 = 1;

pub enum MoneyError { Io { path, source }, Json { path, source }, UnsupportedVersion { path, version },
    ModelsDev(String), Api(ApiError), Lock(LockError), BadRate { currency: String, rate: f64 },
    History(HistoryError) }

/// USD per million tokens.
pub struct ModelPrice { pub input: f64, pub output: f64, pub cache_write: f64,
    pub cache_read: f64, pub tiered: bool }
pub struct PricePoint { pub from: DateTime<Utc>, pub price: ModelPrice }
pub struct PriceChange { pub at: DateTime<Utc>, pub added: Vec<String>,
    pub changed: Vec<String>, pub removed: Vec<String> }
pub struct CurrentPrice { pub key: String, pub price: ModelPrice, pub own: bool,
    pub since: Option<DateTime<Utc>> }
pub struct ModelsDevList { pub prices: BTreeMap<String, ModelPrice>, pub left_out: Vec<String> }
/// Why a value is missing; Display is the text a page shows.
pub enum Missing { NoCurrency, NoPrice { model: String }, NoAccount, NoAccountPrice,
    PriceInOtherCurrency { currency: String }, PriceWithoutCurrency, NoReading,
    UnpricedInWeek { model: String } }

pub struct Prices { pub version: u32, pub checked_at: Option<DateTime<Utc>>,
    pub etag: Option<String>, pub last_change: Option<PriceChange>,
    pub models: BTreeMap<String, Vec<PricePoint>>,           // key → points in time order
    pub overrides: BTreeMap<String, ModelPrice>,             // own prices, win over points
    pub missing: BTreeSet<String> }                          // reported removed once
impl Prices {
    pub fn load(app: &AppPaths) -> Result<Prices, MoneyError>; // missing = empty; bad/other version = error
    pub fn save(&self, app: &AppPaths) -> Result<(), MoneyError>; // pretty, atomic, mode 0600
    pub fn at(&self, key: &str, at: DateTime<Utc>) -> Option<&ModelPrice>; // override, last point ≤ at, first
    pub fn current(&self) -> Vec<CurrentPrice>;               // override else last point, sorted
    pub fn take(&mut self, list: BTreeMap<String, ModelPrice>, at: DateTime<Utc>) -> Option<PriceChange>;
}

pub struct FxPoint { pub date: NaiveDate, pub rate: f64 } // units of the file's currency per 1 USD
pub struct Fx { pub version: u32, pub currency: String, pub points: Vec<FxPoint> }
impl Fx { pub fn load(app) -> Result<Fx, MoneyError>; pub fn save(&self, app) -> Result<(), MoneyError>;
          pub fn take(&mut self, date: NaiveDate, rate: f64); // replace that day, else insert in order
          pub fn at(&self, date: NaiveDate) -> Option<f64>; } // last point ≤ date, else first

pub struct Rate { pub value: f64, pub date: Option<NaiveDate>, pub own: bool }
/// settings.usdRate when its currency is the settings' currency; "USD" is 1.0 with no date;
/// else Fx::at(date) for the same currency. None without a currency, and a rate of another
/// currency is never used.
pub fn usd_rate(settings: &Settings, fx: &Fx, date: NaiveDate) -> Option<Rate>;

pub enum Looked {
    NotModified,
    Taken { change: Option<PriceChange>, left_out: Vec<String> },
}
/// Models.dev → prices.json. Load for the ETag, request, take the app lock, load again (an own
/// price written meanwhile is kept), take the list, store the ETag and checkedAt. NotModified
/// moves checkedAt only. Any error saves nothing. `left_out` names each model the list skipped
/// and why; a later list replaces it.
pub fn look_up_prices(api: &Api, app: &AppPaths, now: DateTime<Utc>) -> Result<Looked, MoneyError>;
/// Frankfurter → fx.json under the app lock. "USD" sends nothing and returns None; a rate that is
/// not finite and above 0 is BadRate; a file for another currency starts empty for this one.
pub fn look_up_rate(api: &Api, app: &AppPaths, currency: &str) -> Result<Option<FxPoint>, MoneyError>;

/// models.dev's body → the three providers' prices and the models it left out. No provider of
/// the three = error; a model without id is skipped; input and output are required;
/// cache_write/cache_read default to 0; a cost that is negative, non-finite or not a number
/// leaves that model out of the list, named in `left_out`; tiers = tiered.
pub fn prices_from_models_dev(body: &serde_json::Value) -> Result<ModelsDevList, MoneyError>;
pub fn price_key(provider: &str, model_id: &str) -> String; // "<provider>/<model id>"
/// "<provider>/<model id>" → its two parts; the provider must be one of PRICE_PROVIDERS and the
/// model id must not be empty, else the Err text a page shows.
pub fn parse_price_key(key: &str) -> Result<(&str, &str), String>;

/// One record at API prices, in USD; None when its model has no price. Anthropic/<model>.
pub fn api_usd(record: &TokenRecord, prices: &Prices) -> Option<f64>;
pub fn percent_value(monthly_price: f64) -> f64; // monthly × 12 / 52 / 100

pub struct Valued { pub api: Option<f64>, pub subscription: Option<f64>,
    pub account: Option<AccountRef>, pub api_currency: Option<String>,
    pub api_missing: Option<Missing>, pub subscription_missing: Option<Missing> }

pub struct Valuer { … }
impl Valuer {
    /// Reads samples, windows and tokens of the range widened by 7 days, and every event over all
    /// history (a record's account and price are the last Live/Switch/PriceChanged at or before it).
    pub fn build(history: &HistoryDir, range: &Range, prices: &Prices, fx: &Fx,
                 settings: &Settings, accounts: &[Account]) -> Result<Valuer, MoneyError>;
    /// api = api_usd × rate of the record's day (USD, labelled, when there is no rate; None with
    /// its reason when the model has no price or no currency is set);
    /// subscription = usd / the week's api_sum × used × percent_value(price), None with its
    /// reason when the account or its price is missing, the week has no reading, an unpriced model
    /// is in the week, or the week has no API value to split.
    pub fn value(&self, host: &str, record: &TokenRecord) -> Valued;
}
```

`Prices::take` adds a point only for a new model or one whose price differs from its last point; a
model missing from the list keeps its points and enters `missing` once; a model that comes back
leaves it. `Prices::at` never looks into the future: before the first point it uses the first point,
so the oldest price counts for the past. The account's price is the last `PriceChanged` at or before
the record, and a stored price without an event uses `Account.price_currency` like an event's
currency: it counts only while that currency is the settings' currency, and a price whose currency
is `None` does not count (`Missing::PriceWithoutCurrency`).

## lookup_rt (step C1)

```rust
const PRICE_EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Serialize)] pub(crate) struct MoneyView {
    pub currency: Option<String>, pub rate: Option<Rate>, pub look_up: bool,
    pub models_with_price: u32, pub prices_checked_at: Option<DateTime<Utc>>,
    pub last_change: Option<PriceChange>, pub left_out: Vec<String>,
    pub rate_error: Option<String>, pub last_error: Option<String> }

#[derive(Serialize)] pub(crate) struct PriceRow {
    pub key: String, pub price: ModelPrice, pub own: bool, pub tiered: bool,
    pub since: Option<DateTime<Utc>> }

pub(crate) struct LookupClient { … }
impl LookupClient {
    pub(crate) fn view(&self) -> Result<MoneyView, String>;   // the last round's view
    pub(crate) fn now(&self) -> Result<MoneyView, String>;    // one round now, answered by the thread
    pub(crate) fn set_override(&self, key: String, price: Option<ModelPrice>) -> Result<(), String>;
}
pub(crate) fn spawn(app: AppPaths, api: Api, ui: Sender<UiEvent>) -> Result<LookupHandle, String>;
pub(crate) struct LookupRuntime { … }
impl LookupRuntime {
    pub(crate) fn round(&mut self, settings: &Settings, now: DateTime<Utc>) -> MoneyView;
    pub(crate) fn settings_failed(&mut self, error: String) -> MoneyView; // no request
    pub(crate) fn view(&self) -> MoneyView;
}
pub(crate) fn price_rows(app: &AppPaths) -> Result<Vec<PriceRow>, MoneyError>;
```

The thread reads `accounts.json` at every round, so `lookUpPrices` counts without a restart. With
it on, a round asks models.dev and then, when the currency is set and not `USD` and the newest
`fx.json` point is older than today's local day, asks Frankfurter once; `last_rate_day` remembers a
successful answer for `(day, currency)`, so a weekend's old ECB date does not cause an hourly
request, while a failure is tried again next round. With `lookUpPrices` off no request is made and
the view still shows what is stored. The runtime's constructor fills the view from the files before
the first round, so the page never shows zero prices while `prices.json` holds some. Every changed
view is sent as `UiEvent::Money(view)`; the main thread emits it to both windows. `set_override`
first checks the key with `money::parse_price_key` (a known provider and a non-empty model id), then
takes the app lock, changes `prices.json` through `Prices`, and rebuilds the view; a price that is
negative, non-finite or not a number is refused. A failed rate look-up goes into `rate_error`, so
the rate row shows it; `last_error` keeps the price look-up's errors and the `prices.json` /
`fx.json` read errors. `left_out` is the last list's skipped models and is kept until a later list
names others.

## projects (step C1)

```rust
#[serde(tag = "edit", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProjectEdit {
    Rename { key: ProjectId, name: String },
    Join { key: ProjectId, into: ProjectId },
    Unjoin { key: ProjectId },
    Assign { key: ProjectId, org: Option<String> },
    OrgRename { org: String, name: String },
    OrgJoin { org: String, into: String },
    AccountOrg { account: AccountRef, org: Option<String> },
}
impl ProjectEdit { pub fn validate(&self) -> Result<(), ProjectsError>; }
// project and organization shown names: 1..=40 characters, not only spaces;
// an organization identity (Assign.org, AccountOrg.org, OrgJoin.org/into) only non-empty,
// because it comes from a login's orgName or a repository owner, not from typing;
// a project or organization cannot be joined into itself; a folder id names its host,
// a repository id does not.

#[serde(rename_all = "camelCase")]
pub struct ProjectId {
    #[serde(flatten)] pub key: ProjectKey,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub host: Option<String>,
}
impl ProjectId { pub fn of(key: &ProjectKey, host: &str) -> ProjectId; } // host kept for a folder key only

pub enum ProjectsError { NameLength { what, length }, NameBlank { what }, EmptyOrg,
    FolderWithoutHost, RepoHasHost { host }, JoinIntoItself { what }, History(HistoryError) }

pub struct ProjectsLogRecord { pub t: DateTime<Utc>, #[serde(flatten)] pub edit: ProjectEdit }
impl Record for ProjectsLogRecord { const KIND: Kind = Kind::Projects; } // "projects"

pub struct Project { pub key: ProjectId, pub name: String, pub repo: Option<String>,
    pub org: Option<String>, pub joined: Vec<JoinedKey>, pub folders: BTreeMap<String, Vec<String>>,
    pub sessions: u32 }
pub struct JoinedKey { pub key: ProjectId, pub name: String } // the key's default name
pub struct Org { pub id: String, pub name: String, pub accounts: Vec<AccountRef>, pub projects: u32 }
pub struct AccountEntry { pub id: String, pub account: AccountRef } // the store's id and its AccountRef
pub struct Projects { pub projects: Vec<Project>, pub orgs: Vec<Org>,
    pub account_refs: Vec<AccountEntry>, /* names, assigned, joins, org_join: private */ }

impl Projects {
    pub fn resolve(log: &[Hosted<ProjectsLogRecord>], sessions: &[Hosted<SessionRecord>],
                   accounts: &[Account]) -> Projects;
    pub fn of(&self, id: &ProjectId) -> Project; // an id that is not a row, with the log's names/joins
}
```

`resolve` first collects each session's `ProjectId::of(key, host)`: a repository id has no host and
is the same project everywhere, a folder id carries its host, so the same folder name on two hosts is
two projects until a `Join` makes one. It then applies the log in time order, ties by host name and
then by line order: `Rename` and `Assign` for the same id replace the previous; `Join` follows the
chain and, when it would close a cycle, drops the earliest record of that cycle; `Unjoin` removes
the id's join; `OrgRename` and `OrgJoin` work the same for organizations. Ids are grouped under
their join root; a group with no session is not a row. A project's name is the last rename of its
root or the root key's default name; its organization is the last assignment of the root or the
root key's default (the repository owner); its `joined` ids carry their default names, because a
joined id's own rename is not shown. Organization identities come from every project's
organization, every account's `org_name`, every `OrgRename` and every `AccountOrg` organization, and
both ends of every `OrgJoin`, joined by `OrgJoin`. `AccountOrg` records are applied in log order:
the latest record for an account puts that one account in that organization's root, or in none.
Nothing else moves an account, so an edit for one account leaves every other account where it is.
`Projects::of` uses the same maps for an id without a row.

## app: history additions — hosts, import and export (step C1)

```rust
pub struct HostInfo { pub host: String, pub this_host: bool, pub first_record: Option<DateTime<Utc>>,
    pub last_record: Option<DateTime<Utc>>, pub bytes: u64 }
pub struct Imported { pub copied: Vec<String>, pub conflicts: Vec<HostConflict> }
pub struct HostConflict { pub host: String, pub reason: String }
impl HistoryDir {
    /// One entry per host folder under the root, this host's own included when it is still
    /// empty, sorted by host. `first_record` is the first record of the oldest file of any kind;
    /// `last_record` the newest `up` or `samples` line; `bytes` the folder's size.
    pub fn host_infos(&self) -> Result<Vec<HostInfo>, HistoryError>;
    /// The window records of the range, one per (account, kind, end within `same_reset`): the
    /// record with the latest `lastSample` counts (spec §1.5), a tie goes to the host name that
    /// sorts first, so read order does not decide. Sorted by end, then account.
    pub fn finished_windows(&self, range: &Range, hosts: &Hosts)
        -> Result<Vec<Hosted<WindowRecord>>, HistoryError>;
    /// Copy every file of `from`'s host folders that is missing here or longer there; keep a
    /// longer file; never read or write this host's folder; refuse this folder and a folder
    /// without a host folder. A host folder that is not here yet is copied as it is. One that
    /// is here is copied only when both sides' `host.json` name this machine; another machine
    /// id, or no `host.json` on either side, is a conflict ("cannot tell whether …"), and
    /// nothing of that folder is copied. A read or write error stops the whole import; a
    /// conflict does not. Returns what was copied and the conflicts, both sorted by host.
    pub fn import(&self, from: &Path) -> Result<Imported, HistoryError>;
}
pub fn export_csv(history: &HistoryDir, range: &Range, out: &mut dyn Write) -> Result<(), HistoryError>;
pub fn export_json(history: &HistoryDir, range: &Range, out: &mut dyn Write) -> Result<(), HistoryError>;
```

The export has one row per window — windows that several hosts recorded are one row, the
record with the latest `lastSample` winning (from `finished_windows`) — and one per UTC day and
project key (`kind: "tokens"`, `dayUtc`, `project` as `repo:<id>`/`folder:<id>`, input, output,
cacheWrite, cacheRead). Both formats make their rows once through the same function: every
window first, then the token rows month by month, so memory holds one month of token records
plus the rows. CSV writes the header
`kind,account,org,limit,model,start,end,percent,day_utc,project,input,output,cache_write,cache_read`
and then the rows, quoting per RFC 4180; JSON writes `{"windows":[…],"dailyTokens":[…]}` with
camelCase fields. A project's shown name is never exported; a name changes, the key does not.

## app: panel commands (step C1)

New and changed `#[tauri::command]`s; all return the error text as `String`.

```rust
// money
async fn money_view(state) -> Result<MoneyView, String>;
async fn price_list(state) -> Result<Vec<PriceRow>, String>;
async fn set_price_override(state, key: String, price: Option<ModelPrice>) -> Result<(), String>;
async fn lookup_now(state) -> Result<MoneyView, String>;
// accounts: changed, `price` is absent / null / a number
async fn edit_account(state, key: String, label: Option<String>, abbr: Option<String>,
    color: Option<String>, auto_burn: Option<bool>, price: Option<serde_json::Value>) -> Result<(), String>;
// projects
async fn projects_view(state) -> Result<Projects, String>;
async fn projects_edit(state, edit: ProjectEdit) -> Result<Projects, String>;
// agents
struct AgentCallRow { at: DateTime<Utc>, command: String, cwd: Option<String>, allowed: bool }
struct SkillRow { path: String, installed: bool, differs: bool }
async fn agent_calls(state, limit: u32) -> Result<Vec<AgentCallRow>, String>;
async fn skill_status(state) -> Result<SkillRow, String>;
async fn skill_install(state, force: bool) -> Result<SkillRow, String>;
// history
struct HistoryView { host: String, dir: String, hosts: Vec<HostInfo>, bytes: u64,
    saved_host: Option<String>, saved_dir: Option<String> }
struct ImportResult { copied: Vec<String>, conflicts: Vec<HostConflict>, view: HistoryView }
async fn history_view(state) -> Result<HistoryView, String>;
async fn history_import(state, from: String) -> Result<ImportResult, String>;
async fn history_export(state, format: String, to: String,
    from: Option<DateTime<Utc>>, until: Option<DateTime<Utc>>) -> Result<(), String>;
```

The commands use the history folder the tray app opened at startup (`state.history`), never one from
a newer snapshot, until a restart; `history_view`'s `saved_host` and `saved_dir` are the saved values
that a restart would use. Paths from the page are expanded with `~` and must be absolute.
`history_export` refuses an existing file and writes through `atomic` with mode 0600.
`projects_from` reads all hosts' `ProjectsLogRecord` and `SessionRecord` over all time, prints torn
lines' warnings on stderr and writes each folder with `~`; `edit_projects` validates first, appends
to this host's own log only, then returns the new resolution.

`worker.rs`: `Snapshot.accounts: Vec<AccountRow>` carries the stored fields the cards do not
(`plan`, `price`, `orgName`, `autoBurn`, login end); `UiEvent` gains `Money(MoneyView)`;
`Worker.lookup: Option<LookupHandle>` is the thread; `Worker.history` is the one `HistoryDir`.
`AccountRow { id, label, abbr, color, email, orgName, plan, price, autoBurn, loginEndsAtMs,
loginDaysLeft, isActive }`. `save_setting(field, value)` takes the app lock, loads
`accounts.json`, replaces that one field with `merge_setting`, validates the whole `Settings` and
writes the file, then writes the one history event of a change (only `AutoBurnSet` for `autoBurn`
today) and applies the autostart entry; it answers `SavedSetting { settings, warning }`, where an
autostart failure is `warning` = `settings saved; start at login could not be set: <error>`, not a
failed save. `remember_settings` puts the reply's merged settings into the snapshot.

## app: CLI — `window`, `prices` and `history` (step C1)

```rust
pub(crate) enum Command {
    // …
    Window { target: Option<String> },              // default "sessions"
    Prices { refresh: bool },
    History { #[command(subcommand)] action: HistoryAction },
}
pub(crate) enum HistoryAction {
    Export { #[arg(long, value_enum)] format: ExportFormat,
             #[arg(long)] from: Option<String>, #[arg(long)] to: Option<String> },
    Import { folder: String },
}
pub(crate) enum ExportFormat { Csv, Json }
```

- `seatswap window [AREA]`: validates the target with `window::parse_target`, then sends
  `window <target>` to the running app; no app running is the IPC module's error.
- `seatswap prices [--refresh]`: with `--refresh`, refuses when `lookUpPrices` is off
  ("look-ups are off (lookUpPrices); turn them on in the settings to refresh"), otherwise calls
  `money::look_up_prices`; then prints the stored prices as a table: `USD per million tokens`,
  then `key`, `input`, `output`, `cache write`, `cache read`; a tiered key has ` *` and a closing
  note that the base price is shown. No prices prints "no prices yet: run `seatswap prices
  --refresh` while online.".
- `seatswap history export --format csv|json [--from YYYY-MM-DD] [--to YYYY-MM-DD]`: writes to
  standard output; the days are UTC days, `--to` inclusive (the range ends at the next midnight),
  either end open when absent, `--from` after `--to` is a usage error.
- `seatswap history import <folder>`: `~` is expanded, the folder must be absolute; prints
  `copied <host>` per host at least one file was copied for, or `nothing new`, then
  `conflict <host>: <reason>` per conflict, and exits with an error when there was one (after
  printing what was copied).

The gate for an agent: `window` is refused like `panel` (`AGENTS_MAY_NOT_CHANGE`); `prices` follows
`agentsRead`, and `--refresh` is refused even with it on (it writes `prices.json`); `history export`
follows `agentsRead`, `history import` is refused (`AGENTS_MAY_NOT_CHANGE`). `seatswap panel
settings` sends `window settings` instead of a pop-up view.

## Files step C1 writes

- **`<data folder>/prices.json`** (§11.5): pretty JSON, atomic, mode 0600, written under the app
  lock by the look-up thread and by `seatswap prices --refresh`, and by an own price
  (`set_price_override`). `version`, optional `checkedAt`, `etag`, `lastChange`, `models` (key →
  dated points), `overrides` (key → price) and `missing` (keys reported removed once). A file of
  another version is refused, never replaced.
- **`<data folder>/fx.json`** (§11.5): `{"version":1,"currency":"EUR","points":[{"date":"…",
  "rate":0.90481}]}`, the same way, only while the currency is set and not `USD`.
- **`<history root>/<host>/projects-YYYY-MM.jsonl`** (§11.6): one line per project or organization
  edit, `{"t":"…","edit":"join","key":{…},"into":{…}}`; append-only, this host's own file only,
  atomic through `atomic::append_lines`.
- **History events**: `{"t":"…","type":"priceChanged","account":{"acc":"…","org":"…"},
  "amount":180.0,"currency":"EUR"}` when an account's price changes (amount and currency omitted
  when null); `{"t":"…","type":"planChanged","account":{…},"from":"Team","to":"Team 5x"}` when a
  stored login changes the plan label.
