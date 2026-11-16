//! seatswap core: everything except the GUI. See docs/spec.md and docs/implementation.md.
//!
//! Module owners (one module per job; do not add a second way to do what a module does):
//! - `types`    the login and identity value types shared by every module
//! - `paths`    where Claude Code's files and seatswap's own files live
//! - `atomic`   the one way to write a file (temp file + fsync + rename, mode kept)
//!   and the only module that knows file modes and private files and folders
//! - `burn`     auto-burn: work units, rhythm, rates, slack, plans, the decision (spec §2)
//! - `claude_settings` the one key seatswap writes in Claude Code's settings.json
//! - `codex_sessions` Codex's session files
//! - `duration` every duration as one text: `2h 31min`, `12min`, `3d 4h` (review item 42)
//! - `feed`     the status line's live numbers: its file, its account, and merging them
//! - `harness`  DeepSeek Harness sessions: flash workers and their jobs
//! - `history`  the history folder: record types, appending, reading a range, merging hosts
//! - `project`  from a folder to the project's identity
//! - `sessions` Claude Code's session files and which are alive
//! - `stats`    everything the statistics pages show, computed from the history
//! - `transcripts` Claude Code's transcripts: answers, user turns, token records
//! - `locks`    Claude Code's six lock folders + seatswap's own switch.lock
//! - `live`     read/write the live login (.credentials.json) and identity (.claude.json)
//! - `live_sessions` the rows of the Sessions area: running and closed sessions, their agents and values
//! - `money`    API prices with their dates, the dollar rate, the value of tokens
//! - `projects` projects and organizations: names, joins, the shared log (spec §3.6)
//! - `providers` one reader per other provider (Codex, Ollama), read-only, into the usage types here
//! - `store`    accounts.json (no secrets) + secrets (OS keyring, or 0600 files)
//! - `api`      the HTTP endpoints: usage, token refresh, Codex, prices, the dollar rate
//! - `switcher` the switch algorithm (spec §6.2) and saving the current login
//! - `engine`   usage polling, refreshing inactive logins, usage cache (spec §6.3, §7)
//! - `watcher`  noticing changes Claude Code makes to the live login (spec §6.4)

pub mod api;
pub mod atomic;
pub mod burn;
pub mod claude_settings;
pub mod codex_sessions;
pub mod duration;
pub mod engine;
pub mod feed;
pub mod harness;
pub mod history;
pub mod live;
pub mod live_sessions;
pub mod locks;
pub mod money;
pub mod paths;
pub mod project;
pub mod projects;
pub mod providers;
pub mod sessions;
pub mod stats;
pub mod store;
pub mod switcher;
pub mod transcripts;
pub mod types;
pub mod watcher;

pub use types::{Login, OAuthAccount, LOGIN_KEYS};
