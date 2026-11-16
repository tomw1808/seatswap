//! The other providers (spec §7): one reader per provider, read-only, into
//! seatswap's own `Usage` and `Limit` types so cards, colours and history
//! need no second implementation. Codex's `auth.json` is only ever read and
//! Ollama's own server is only ever asked for its balance and usage; no token
//! is refreshed, stored or printed, and nothing of a provider is written.

use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::api::{ApiError, Usage};
use crate::history::AccountRef;

pub mod codex;
pub mod ollama;

/// The `org` of a Codex [`AccountRef`]; its `acc` is the account id.
pub const CODEX_ORG: &str = "codex";
/// The `org` of an Ollama [`AccountRef`]; its `acc` is the same name.
pub const OLLAMA_ORG: &str = "ollama";

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("No Codex login found in {path}. If Codex keeps its login in the system keyring, seatswap cannot read it.")]
    NoLogin { path: PathBuf },
    #[error("Codex keeps its login in the system keyring. seatswap reads the login file only.")]
    KeyringLogin,
    #[error("Codex's login has expired. Run codex once to renew it.")]
    LoginExpired,
    #[error("Ollama is not running on {url}.")]
    NotRunning { url: String },
    #[error("Ollama on {url} refused the request (HTTP {status}). Is it signed in?")]
    OllamaRefused { url: String, status: u16 },
    #[error("{0}")]
    Setting(String),
    #[error(transparent)]
    Api(ApiError),
    #[error("Unexpected answer: {0}")]
    Shape(String),
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A provider's credit balance, in US dollars.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Credits {
    pub left: f64,
    pub allowance: Option<f64>,
    pub until: Option<DateTime<Utc>>,
    pub purchased: Option<f64>,
}

/// One provider's answer, in the types seatswap already uses for an account.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderReading {
    pub account: AccountRef,
    pub plan: Option<String>,
    pub usage: Option<Usage>,
    pub credits: Option<Credits>,
    /// The spend of the last 24 hours, when the provider has such a reading:
    /// `None` for Codex, `Some(Err)` when Ollama's usage answer could not be
    /// read (the balance is still in the reading).
    pub spend_24h: Option<Result<f64, String>>,
}
