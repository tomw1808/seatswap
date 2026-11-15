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
}

#[derive(… Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub enum LoginState { Ok, LoggedOut }

/// Settings §10 (no clearOrgCaches). Defaults: tray_text Percent, icon_limit Binding,
/// poll_active_secs 120, poll_inactive_secs 900, suggest_at 90, suggest_target_below 50,
/// notify_on_switch true, mirror_live true, refresh_inactive true, hotkey "Ctrl+Alt+C",
/// start_at_login true, theme System, claude_config_dir None, history_dir None,
/// host_name None, read_sessions true, notify_usable_again true, notify_waiting true,
/// notify_waiting_hosts ThisHost, agents_read true, agents_switch false.
/// `#[serde(default)]` per field so an older file without a new setting still loads.
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
#[serde(rename_all = "camelCase")] pub enum WaitingHosts { ThisHost, AllHosts }
pub enum TrayText { Percent, PercentReset, Off }
pub enum IconLimit { Binding, Session, Week }
pub enum Theme { System, Light, Dark }
impl Settings { pub fn validate(&self) -> Result<(), StoreError>; } // percents 1..=100, polls >= 30 s,
// an absolute claudeConfigDir, an absolute historyDir, and a hostName that passes paths::host_name

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
impl Limit { pub fn level(&self) -> LoadLevel; }
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
/// and whether anything was written.
pub fn sync_live(claude: &ClaudePaths, store: &Store, create_unknown: Option<Option<String>>,
                 now: DateTime<Utc>) -> Result<(LiveState, bool), SwitchError>;
// create_unknown: None = never create; Some(label) = create with that label (None = default label).

/// Remove an account from the list and its login from the secrets. Never touches Claude's files.
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
}
#[derive(… Serialize, Deserialize)] pub struct UsageCache { pub version: u32 /* 1 */, pub entries: BTreeMap<String, CacheEntry> }
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
    pub is_active: bool,
    pub login_state: LoginState,
    pub login_ends_at_ms: Option<i64>,
    pub login_days_left: Option<i64>,         // floor((ends - now) / 1 day), may be negative
    pub usage: Option<Usage>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// §7.4: the active account's binding limit ≥ settings.suggest_at and another Ok account's binding
/// limit < suggest_target_below (or unknown usage → not a candidate). The candidate with the lowest
/// percent wins. None otherwise.
pub struct Suggestion { pub from_id: String, pub to_id: String, pub from_percent: f64, pub to_percent: f64,
                        pub from_resets_at: Option<DateTime<Utc>> }
pub fn suggestion(views: &[AccountView], settings: &Settings) -> Option<Suggestion>;

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
pub struct SessionCounts { pub running: u32, pub busy: u32, pub tied: u32 }
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

```rust
pub struct Recorder { /* dir, per-account last line and remembered limits, written windows, last up */ }
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
    pub fn session(&self, record: &SessionRecord) -> Result<(), HistoryError>;
}
pub fn sample_of(account: &AccountRef, usage: &Usage, at: DateTime<Utc>, src: Source) -> Sample;
/// A limit (matched by k and m) has ended when prev.r is Some(end) and next.t >= end, or next has
/// the limit with next.r > end + 60 s. A limit missing from next, or with r None, ends nothing.
/// end = prev.r, final = prev.p, lastSample = prev.t, uncertain = end - prev.t > 1 h;
/// start = end - 7 d for weekly kinds, end - 5 h for session, None otherwise;
/// lost = max(0, 100 - final) for weekly kinds, None otherwise.
pub fn closed_windows(prev: &Sample, next: &Sample, plan: Option<&str>) -> Vec<WindowRecord>;
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
    pub thinking_since_user: u32 }
impl ScanState {
    pub fn load(app: &AppPaths) -> Result<ScanState, TranscriptError>;  // missing folder = default
    /// Writes only the project folders that changed, as <folder>.json; before writing, drops the
    /// recent ids of a file that has not grown for 24 h, and removes entries whose transcript is gone.
    pub fn save(&mut self, claude: &ClaudePaths, app: &AppPaths, now: DateTime<Utc>)
        -> Result<u32, TranscriptError>;
}
pub struct Scan { pub answers: Vec<Answer>, pub user_turns: Vec<UserTurn>, pub complete: bool,
    pub warnings: Vec<String> }
/// Reads what is new in <folder>/<session>.jsonl and <folder>/<session>/subagents/agent-<id>.jsonl,
/// at most budget_bytes; only whole lines, and the offset stops after the last '\n'. A shorter file
/// is read again from 0 (warning); its recent ids keep answers from counting twice. A file that had
/// stopped growing and was replaced is not counted again, and the warning says so. Other line
/// types are skipped quietly. A user line counts only as a real user message whose origin is
/// absent or "human". An answer counts once per file by message id; "<synthetic>" is skipped.
pub fn scan(claude: &ClaudePaths, state: &mut ScanState, budget_bytes: u64)
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
// AccountView gains:  pub usable: bool, pub blocked_until: Option<DateTime<Utc>>
```

When several limits are at 100 %, the blocking one is chosen by the latest reset time (a limit with
no reset wins over one with a time); on a tie Week comes before Model, Model before Session.
`blocked_until` is that limit's reset time. `record_success` passes the account's `AccountRef` and
plan to the recorder; a history error becomes the account's `last_error`, never a failed poll. An
account blocked by a limit is due once 30 s after that reset whatever the interval, if its last
success and last attempt are both before that moment. After a poll the engine writes `AllBlocked` or
`AnyUsable` when "some account is usable" changed since the previous poll.

## app: CLI (§4)

`crates/app/src/cli.rs`, clap derive. `main.rs` parses; no subcommand → tray app when built with `gui`, else help; from an agent (§6.6 of the spec) it is refused instead (`no_subcommand(caller)`). Global options: `--claude-config-dir <DIR>` (else the `claudeConfigDir` setting, else `$CLAUDE_CONFIG_DIR`, else `~/.claude`), `--data-dir <DIR>` (else the platform folder). Every command prints errors as `seatswap: <message>` to stderr and exits 1 (2: an unsaved current login; 3: refused for an agent); success exits 0.

| Command | Does |
|---|---|
| `list` | One line per account: `*` for the live one, abbr, label, email, plan, session %, week %, "login ends in N d", "LOGGED OUT" when so. Usage from the cache, with its age ("5 min ago"); `--refresh` polls first (PollReason::PanelOpened). |
| `use <KEY>` | `switch_to`. KEY = id or abbr. When the live login is not saved: `--save-current [LABEL]` saves it first, `--discard-current` drops it; without either, exit 2 with the message "the current login (<email>) is not saved; rerun with --save-current or --discard-current". After the switch: "Switched to P — Private". |
| `next` | Switch to the next Ok account after the live one in list order (wraps), same unsaved handling as `use`. |
| `add [--label L]` | `sync_live` with create: saves the live login as a new account or updates the stored copy. Prints the account. |
| `rm <KEY> [--yes]` | `remove_account`; without `--yes` asks "Remove AC — Acme? [y/N]" on a terminal, refuses when stdin is not a terminal. |
| `edit <KEY> [--label L] [--abbr A] [--color #rrggbb]` | `edit_account`. |
| `panel [main\|add\|settings]` | Asks the running tray app to open its pop-up (§ app: GUI, single instance). Not running → exit 1 "seatswap is not running". This is the hotkey on Linux v1: the person binds it in the desktop's keyboard settings (no global-shortcut crate is available). |
| `agent-skill install\|uninstall [--force]` | `agent_skill`: `<claude config dir>/skills/seatswap/SKILL.md` from the bundled text, through `atomic::ensure_private_dir` and `atomic::write_atomic` (0600). Install prints "already installed: <path>" when the file is identical, refuses a differing file without `--force`, else prints the path. Uninstall prints "not installed: <path>" when absent, refuses a differing file without `--force`, else prints "removed <path>" and removes the folder when it is empty. |
| `status` | The live account in one line: `AC 97% · 1h12m` (binding limit, time to its reset; format §11: `35m`, `1h05m`, `1d18h`, beyond 48 h `Sun 02:00` — `format::format_reset_at`; reset times are rounded up to the full minute first, `format::reset_minute`). `--json` prints the views as JSON (`views` array + `active` id + `suggestion`); each view carries `usable`, the one definition of §7.2. `--waybar` prints one JSON line `{"text":"AC 97%","tooltip":"…","class":"fine|close|critical|locked|none","percentage":97}`; `--i3blocks` prints three lines (full text, short text, colour). `--refresh` polls first (PollReason::Timer). |

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

Gate, when `caller` is `Person`: everything is `Allowed`. When `Agent`: `list` and `status` need
`agents_read`; `use` and `next` need `agents_switch`, and never with `--save-current` or
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

Files: `crates/app/src/gui/{mod.rs, worker.rs, tray.rs, panel.rs, notify.rs, autostart.rs, ipc.rs, registry.rs}` (`registry.rs` on Windows only), `crates/app/ui/{index.html, panel.css, panel.js}`, `crates/app/capabilities/default.json`. The code is the library `seatswap` (`src/lib.rs`, `pub fn run() -> ExitCode`); two programs call it: `seatswap` (`src/main.rs`) and `seatswap-tray` (`src/bin/seatswap-tray.rs`, built without a console window on Windows for autostart and the Start menu; Linux builds it but does not install it). No subcommand → `gui::run(app: AppPaths, claude_override: Option<PathBuf>, data_override: Option<PathBuf>) -> ExitCode` (the same path resolution as the CLI, one function; the overrides are passed on so `seatswap panel` and autostart start the app with the same flags). Actions both front ends must take the same way live in `crates/app/src/ops.rs`: `save_live_login(claude, store, label, now) -> Result<Account, String>` (a missing, ended or unknown login saved nothing and is an error), `switch_message(store, account_id) -> Result<String, String>` ("Switched to AC — Acme"; an unreadable store or unknown id is an error, never the raw id) and `switched_without_list(error) -> String`.

**Threads.**
- *Main thread:* Tauri event loop, tray, menu, panel window.
- *Engine thread:* owns `Engine<Api>` (its own `Store` from `store::open_store`). Loop on a command channel with a 30 s tick: `Poll(PollReason)`, `Publish`, `Shutdown`. Each tick polls with `Timer` (the engine decides what is due). Wake detection: when the wall clock advanced more than 90 s beyond the monotonic clock between two ticks → `Poll(Wake)`. After each poll or `Publish` it builds a `Snapshot` and hands it to the main thread.
- *Action thread:* owns a second `Store` (from `open_store`). Runs switch / save current / edit / remove / save settings one at a time, each replying through a one-shot channel, then sends `Poll(AfterSwitch)` or `Publish` to the engine thread. Two stores are safe because every accounts.json change is load-modify-save under the app lock.
- *Watcher thread:* `watcher::watch`; its callback sends `SyncLive` to the action thread (sync_live when `mirrorLive`, else nothing), which then sends `Publish`.
- Tauri commands never block the main thread (`async` commands that wait on the reply with `tauri::async_runtime::spawn_blocking`).

**Snapshot** (serde camelCase; the panel's only data source; event `snapshot`, and command `get_snapshot`):

```rust
pub struct Snapshot {
    pub views: Vec<AccountView>,          // engine::views, active first
    pub active_id: Option<String>,
    pub live: LiveView,                   // { kind: "noLogin"|"ended"|"known"|"unknown", accountId?, email?, orgName? }
    pub suggestion: Option<Suggestion>,   // engine::suggestion (Serialize, camelCase)
    pub warnings: Vec<LoginWarning>,      // engine::login_warnings (Serialize, camelCase, tag "kind")
    pub pending: Option<PendingSwitch>,   // { targetId, targetLabel, email, orgName }: a switch waits for "save or discard the current login?"
    pub secrets_warning: Option<String>,  // from open_store: logins stored unencrypted
    pub last_poll_at: Option<DateTime<Utc>>,
    pub error: Option<String>,            // last engine-wide error (store/cache unreadable)
    pub settings: Option<Settings>,      // None when accounts.json cannot be read: the panel disables Save
}
```

**Tauri commands** (all `Result<T, String>`, the error is the human message of the core error):
`get_snapshot() -> Snapshot` · `refresh_now()` (Poll(Wake)) · `switch_account(key, unsaved: "refuse"|"discard"|"save", label: Option<String>) -> String` (the success sentence; `CurrentNotSaved` sets `pending` and returns Err "not saved") · `cancel_pending()` · `save_current(label: Option<String>)` (sync_live with create) · `edit_account(key, label?, abbr?, color?)` · `remove_account(key)` · `save_settings(settings: Settings)` (validated; applies startAtLogin) · `icon_preview(abbr, color) -> Vec<IconPx { size, rgba: Vec<u8> }>` for 16, 22, 32 (the panel draws them with `ImageData`) · `fit_panel(height: f64)` · `hide_panel()`.
Event `navigate` (payload `"main"|"add"|"settings"`) when the panel is shown for a menu item.

**Tray** (`tray.rs`, redrawn on every snapshot): icon from `icon::render` at 22 px ×2 (44 px image) for the active account — ring = `settings.iconLimit` limit (binding/session/week) with its `LoadLevel` colour, `locked` at ≥ 100 %, grey ring while the usage is unknown, a grey "?" disc when no account is live; tooltip as §8.1; tray label (`set_title`) per `trayText`. Linux backend: tray-icon's `ksni` (StatusNotifierItem over D-Bus), switched on in `crates/app/Cargo.toml` next to Tauri's libappindicator, because libappindicator reports no clicks and drops the tooltip; with ksni the item has `ItemIsMenu = false`, a left click arrives as `TrayIconEvent::Click(Left)` (opens the panel), a right click shows the menu, and the tooltip is published (checked on the session bus, 2026-11-10). Without a StatusNotifierWatcher on the bus (plain GNOME without the AppIndicator extension, or a desktop panel that starts after seatswap at login) tray-icon cannot create the item and has no option to wait; `tray::start` then keeps the app running without an icon, says so on stderr at once and in a notification only if no tray has appeared after 30 s (`TRAY_GRACE`: at login Cinnamon starts its tray in the same second as seatswap, so the first try often comes too early) (`seatswap panel` still opens the pop-up), and creates the icon when the name `org.kde.StatusNotifierWatcher` appears (`gio::bus_watch_name`, GTK's own gio). Once the item exists, ksni registers it again whenever the watcher restarts. Menu as §8.3: one item per account (`AC  Acme   97 % · 1h12m` + "✓" for the live one, "logged out" when so), clicking switches with `refuse`; then "Save current login (…)…" when live is unknown; "Open panel…", "Add account…", "Settings…", "Quit". Number formatting is shared with the CLI (one module, `crates/app/src/format.rs`).

**Panel** (`panel.rs`): one window label `panel`, created on first open (`index.html#<view>`), kept and hidden afterwards. 380 px wide, height from `fit_panel`, no decorations, always on top, skip taskbar. Placement §9.2: X11 at the mouse, inside the monitor that holds it, above the mouse when the mouse is in the lower half, else below; Wayland (`XDG_SESSION_TYPE=wayland`) centred. `show` takes an `Anchor { cursor, monitor, scale }` once per opening and keeps it with the last requested logical height in `GuiState.panel: Mutex<Placement>`; `fit_panel` resizes and re-places from that anchor (`anchored_position(anchor, height, wayland)` → `place_panel`), so the panel keeps its bottom edge above the mouse, its top edge below it, and stays inside the monitor. Never read the window's position or size back to place it: on X11 they report the old values until the window system has applied a change (the headless run of 2026-11-10 drifted the panel to y = 836 and y = −386 that way). The window is built `resizable(true)`: GTK ignores `set_size` on a non-resizable window. Closes on Esc and the header's × (panel.js → `hide_panel`), on focus loss, and on a left click on the tray icon while open (`panel::toggle`, decision in the pure `tray_click(visible, focus_hidden_ago)`; a click within `REOPEN_GUARD` = 400 ms after a focus-loss hide is the click that caused it and does not reopen). Focus loss hides only if the panel is still unfocused `FOCUS_RECHECK` = 150 ms later and no pop-up of the process holds a GTK grab (`own_popup_open` → `gtk::grab_get_current`): WebKitGTK shows a `<select>` list as its own window with a grab, so the panel is unfocused for as long as the list is open; the list closes on the next click outside and hands the focus back. Focus on X11 (`take_focus`, `focus_x11`): `present_with_time` + `gdk_window.focus` with the X server's current time (`gdkx11::x11_get_server_time`) — Cinnamon's window manager refuses `set_focus`, which asks without a timestamp; and on every map the request is repeated up to `FOCUS_RETRIES` = 5 times, `FOCUS_RETRY_GAP` = 80 ms apart, until the window is active, because a freshly mapped window is not yet managed and the first request is ignored (both seen on Cinnamon 2026-11-10). Built hidden at startup (`panel::preload` in `setup`) so the page has loaded and measured itself before the first opening. A hidden panel does not appear when `show` is called: `show` stores the anchor, sets `PanelState.pending_show` and sends `navigate`; the page answers every navigation with `fit_panel` (setView), and that call sizes, places and only then shows it (`reveal`: move, then `show`), so the window manager maps it once, at its final size and place. If no height arrives within `SHOW_DEADLINE` = 300 ms it appears anyway with a log line. On Linux, nothing may map the window before the move is applied (`present_with_time` maps through GTK at once, and the panel flashed at its old place); focus is asked from the map handler instead. `.panel` has `flex-shrink: 0`: before the first showing the viewport has no width and a shrinking panel measured 2698 px tall. Opening it sends `Poll(PanelOpened)`.

**Notifications** (`notify.rs`, notify-rust): every notification is built from `base()` (summary "seatswap"; on Windows `.app_id("dev.seatswap.app")`), the only caller of `Notification::new()`. On Windows `register_app_id()` writes the display name "seatswap" under `HKCU\Software\Classes\AppUserModelId\dev.seatswap.app` once at startup, before the single-instance listener; a failure is a startup error `notification name: …`. After a switch when `notifyOnSwitch` ("Switched to P — Private"); the switch suggestion (§7.4) once per account and reset window, with a "Switch" action (`wait_for_response` on its own thread; click on the body opens the panel); login warnings once per account and level per run; an unknown live login once per identity per run ("New login found: <email>. Save it as an account?" → a click on its button or body opens the add view). What counts as a click is decided in one pure function, `click(response, button)`: the offered button (`NotificationResponse::Action`) or the body (`Default`); a notification that timed out, was dismissed or closed (`Closed`) does nothing. On Linux the body click only arrives when the notification announces the action `default` (`clickable()`); the notification service shows no button for it. Windows reports a body click by itself, so `default` is announced on Linux only.

**Single instance and `seatswap panel`** (`ipc.rs`): the tray app takes `acquire_file_lock(gui_lock, 0)`. Busy → another instance runs: send `open-panel main` to `gui_socket` and exit 0. Held → name the socket only now (the lock created the data folder on first start, and the name needs its real path), remove a leftover socket, listen on it (mode 0600 on Unix; on Windows the file has the data folder's permissions). Windows uses the same Unix socket through `uds_windows` (`std::os::windows::net` is not stable yet); there is no runtime folder, so the socket is `<data>\gui.sock`. One line per connection: `open-panel <view>` → `ok`, anything else → `error unknown command`.

**Autostart** (`autostart.rs`, one entry point `apply_autostart(enabled, arguments)`): at start and on `save_settings`. The entry is text: a program path or folder argument that is not valid Unicode is `AutostartError::NotUnicode` on both platforms (the one `path_text`), never a path with replaced characters. Linux: `startAtLogin` on → write `autostart_file($XDG_CONFIG_HOME or ~/.config)` with `Exec=<current_exe>` via `write_atomic` (`Access::Shared`); off → remove it if present. Windows: on → the string value `seatswap` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` = `"<folder of current_exe>\seatswap-tray.exe" "<arg>" …` (each part in double quotes, `"` written `\"`, a run of backslashes before a `"` or at the end doubled); a missing `seatswap-tray.exe` (`NoTrayProgram`), a path that is not valid Unicode (`NotUnicode`) or a command over 260 UTF-16 units (`CommandTooLong`) is an error, never shortened; off → delete the value (missing is fine). `gui/registry.rs` (Windows only) is the one module that names `winreg`: `set_string`, `get_string` (tests only), `delete_value`, all under `HKEY_CURRENT_USER`.

**Panel UI** (`ui/`, plain HTML/CSS/JS, no build step, `window.__TAURI__` with `app.withGlobalTauri: true`): §11.1–11.4. Views `main`, `add` (the §6.1 steps; shows the unknown live login with a label field and "Save" when there is one), `settings` (every §10 setting, in the groups of §11.4). The main view's footer carries the sessions line of §11.1 from `snapshot.sessions`. Every time on the page (reset countdowns, the suggestion's clock time, the header's poll age, a card's "usage 5 min ago") is an element with `data-time` + `data-time-style` whose text comes from the command `time_texts([{at, style}]) → [text]` (`countdown` → `format_reset_at`, `clock` → `format_reset_clock`, `age` → `format_age`), asked after each render and every second, so the page has no copy of these rules. Without `window.__TAURI__` it runs on a built-in fixture snapshot so it can be opened in a browser; there `time_texts` answers with stand-ins (the plain clock time, "a moment ago").

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
pub(crate) fn run_pass(claude: &ClaudePaths, probe: &dyn Liveness, recorder: Option<&Recorder>,
                       state: &mut ScanState, memory: &mut SessionMemory, now: DateTime<Utc>,
                       state_error: Option<&str>) -> PassOutcome;
/// run_pass, then state.save; a save failure is a warning on the summary. The thread's start is
/// load_state(app) + SessionMemory::new(home) + one_pass, which is what the backfill test does.
fn one_pass(app: &AppPaths, claude: &ClaudePaths, probe: &dyn Liveness, recorder: Option<&Recorder>,
            state: &mut ScanState, memory: &mut SessionMemory, now: DateTime<Utc>,
            state_error: Option<&str>) -> PassOutcome;
```

`WAKE` = 60 s, `AFTER_INCOMPLETE` = 1 s, `SCAN_BUDGET` = 64 MiB per pass, `RECORD_EVERY_SECS` =
10 min: a running session's record is written when its summary changes and at most that often, and
one closing record when it is gone. Token records are written on every pass from the answers that
pass read (`transcripts::token_records`), so one answer is counted once even when it arrives over
two passes. Waits are kept in memory: a wait that ended writes `EventKind::Waited { session,
project, secs }` with the whole seconds (never 0 from a backwards clock). A session file that
cannot be read, a count that does not fit u32, or a transcript state error is shown in
`SessionsSummary.unavailable` / `warning`; the thread keeps running.

## app: worker, ops and notifications additions — step A

```rust
// worker.rs
pub(crate) struct SessionsSummary { pub running: u32, pub busy: u32, pub waiting: Vec<Waiting>,
    pub unavailable: Option<String>, pub warning: Option<String> }
pub(crate) struct Waiting { pub session: String, pub label: String, pub project: String,
    pub since: DateTime<Utc> }
pub(crate) struct Snapshot { … pub sessions: Option<SessionsSummary> /* None = readSessions off */ … }
pub(crate) enum EngineCommand { Poll(PollReason), Publish, ModelsInUse(Option<BTreeSet<String>>), Shutdown }
pub(crate) enum UiEvent { Snapshot(Snapshot), Sessions(SessionsSummary) }

// ops.rs
pub(crate) fn open_history(app: &AppPaths, settings: &Settings) -> Result<Recorder, String>;
pub(crate) fn history_unwritable(error: &dyn std::fmt::Display) -> String;
pub(crate) fn record_switch(recorder: &Recorder, from: Option<AccountRef>, to: AccountRef, by: By,
    reason: Option<String>, cwd: Option<String>, sessions: Option<SessionCounts>,
    at: DateTime<Utc>) -> Result<(), HistoryError>;

// notify.rs
pub(crate) enum Notification { Suggestion(..), LoginWarning(..), UnknownLogin { .. },
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
the previous snapshot naming a blocker; it is remembered by (account id, previous reset minute), so
one per account and reset. `AllFull` only when the previous snapshot had a usable account and every
account whose login still works is now blocked; the next is the earliest `blocked_until`. `Waiting`
only after 15 s (`waiting.since` to `now`), once per session and wait; turning `notifyWaiting` off
withdraws the shown ones and sends nothing new; a session that stops waiting gets `WaitingOver`,
which withdraws its notification. The texts come from one place each: `usable_again_text` =
`{abbr} is usable again: {reason}.`, with reason `its 5 h window reset`, `its week reset` or
`its {model} week reset`; `all_full_text` = `All accounts are full. Next: {abbr} at {time}.` or
`All accounts are full.`; `waiting_text` = `{label} is waiting for you ({project}, on {host}).` or,
without a host name, `{label} is waiting for you ({project}).`. On Linux a waiting notification
never times out and can be withdrawn; on Windows it follows the system's rules and the pop-up keeps
the count.

