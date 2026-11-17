//! The provider thread (step-d.md Task 4): Codex's and Ollama's cards, polled
//! every five minutes while their switch is on, and Codex's saved logins (R8).
//!
//! The thread opens the store once at spawn ([`store::open_store`]) and keeps
//! it. A Codex round saves the live login, asks every other saved login with its
//! own token, and keeps the logins that expired on a failed renewal out of the
//! requests until their stored copy changes. Only Codex's stored copies and its
//! live `auth.json` are touched; no Claude login is read or written.
//!
//! The thread reads `accounts.json` at every round, so a switch, a Codex folder
//! or an Ollama URL counts without a restart. A card that is off is never read
//! and never asked, unless its subscription has a price. Every change of the
//! lists is one [`UiEvent::Providers`] for the main thread, Codex first; the
//! card list holds only the cards that are on, and the Codex login list is empty
//! while the Codex card is off.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

use seatswap_core::api::{Api, Usage};
use seatswap_core::history::{AccountRef, HistoryDir, Recorder, Source};
use seatswap_core::paths::{AppPaths, CodexPaths, PathsError};
use seatswap_core::providers::{self, Credits, ProviderReading, CODEX_ORG, OLLAMA_ORG};
use seatswap_core::store::{self, ProviderSubscription, Settings, Store};
use seatswap_core::types::CodexLogin;

use crate::ops;

use super::worker::{PrintedWarnings, UiEvent};
use super::ReportedErrors;

/// The provider cards are polled every five minutes (step-d.md Task 4).
pub(crate) const PROVIDER_POLL_SECS: u64 = 300;

/// An open pop-up polls a provider only when its last attempt is older than
/// this.
const PROVIDER_OPEN_SECS: i64 = 60;

/// An expired answer always carries its message and the copy it failed from
/// (the only constructor is [`CodexAnswer::failed`]); an answer without them is
/// a stated error, never a default. Such a login is asked again next round
/// rather than remembered wrongly as expired.
const EXPIRED_WITHOUT_MESSAGE: &str =
    "a saved Codex login was marked expired without a message; it is asked again at the next round";
const EXPIRED_WITHOUT_COPY: &str =
    "a saved Codex login was marked expired without the copy it failed from; it is asked again at the next round";

/// One provider card as the panel shows it. Only a card that is switched on is
/// in the snapshot; a missing value stays `None`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderView {
    pub provider: String,
    pub title: String,
    pub plan: Option<String>,
    pub usage: Option<Usage>,
    pub credits: Option<Credits>,
    pub spend_24h: Option<f64>,
    pub spend_24h_error: Option<String>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// One saved Codex login as the pop-up lists it (R8): the subscription's look
/// and plan, the last reading's usage, and whether the login is the live one.
/// `state` is `"ok"`, `"expired"` after a failed renewal, or `"error"` for any
/// other failed reading; `message` says why and is `None` when ok.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexLoginView {
    pub account: AccountRef,
    pub abbr: String,
    pub label: String,
    pub color: String,
    pub plan: Option<String>,
    pub live: bool,
    pub usage: Option<Usage>,
    pub state: CodexLoginState,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CodexLoginState {
    Ok,
    Expired,
    Error,
}

/// The two lists the provider thread hands to the UI in one event: the cards
/// that are on, and the saved Codex logins (R8).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderLists {
    pub cards: Vec<ProviderView>,
    pub codex_logins: Vec<CodexLoginView>,
}

/// The provider one card reads, in the list's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Codex,
    Ollama,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Provider::Codex => CODEX_ORG,
            Provider::Ollama => OLLAMA_ORG,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Provider::Codex => "OpenAI (Codex)",
            Provider::Ollama => "Ollama",
        }
    }

    fn is_on(self, settings: &Settings) -> bool {
        match self {
            Provider::Codex => settings.codex_card,
            Provider::Ollama => settings.ollama_card,
        }
    }

    /// Whether this provider's own setting changed between two saves.
    fn setting_changed(self, before: &Settings, after: &Settings) -> bool {
        match self {
            Provider::Codex => before.codex_home != after.codex_home,
            Provider::Ollama => before.ollama_url != after.ollama_url,
        }
    }

    fn view_mut<'a>(
        self,
        codex: &'a mut Option<ProviderView>,
        ollama: &'a mut Option<ProviderView>,
    ) -> &'a mut Option<ProviderView> {
        match self {
            Provider::Codex => codex,
            Provider::Ollama => ollama,
        }
    }

    fn tried_mut<'a>(
        self,
        codex: &'a mut Option<DateTime<Utc>>,
        ollama: &'a mut Option<DateTime<Utc>>,
    ) -> &'a mut Option<DateTime<Utc>> {
        match self {
            Provider::Codex => codex,
            Provider::Ollama => ollama,
        }
    }
}

/// The commands the window, the settings save and a Codex action send to the
/// provider thread.
enum ProvidersCommand {
    /// The pop-up opened.
    Opened,
    /// A settings save wrote `accounts.json`.
    SettingsSaved,
    /// A Codex login was switched or forgotten: poll Codex now.
    CodexChanged,
    Stop,
}

/// What the commands share with the thread.
#[derive(Clone)]
pub(crate) struct ProvidersClient {
    commands: Sender<ProvidersCommand>,
}

impl ProvidersClient {
    /// The pop-up opened: poll a card whose last attempt is older than a
    /// minute.
    pub(crate) fn opened(&self) -> Result<(), String> {
        super::send_to_worker(self.commands.send(ProvidersCommand::Opened))
    }

    /// A settings save: poll a card that was just switched on or whose folder
    /// or URL changed, and drop the view of one that was switched off.
    pub(crate) fn settings_saved(&self) -> Result<(), String> {
        super::send_to_worker(self.commands.send(ProvidersCommand::SettingsSaved))
    }

    /// A Codex login was switched or forgotten: ask the thread to poll Codex at
    /// once, as a settings save pokes it.
    pub(crate) fn codex_changed(&self) -> Result<(), String> {
        super::send_to_worker(self.commands.send(ProvidersCommand::CodexChanged))
    }
}

/// The handle the tray keeps. Dropping it stops and joins the thread.
pub(crate) struct ProvidersHandle {
    client: ProvidersClient,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ProvidersHandle {
    pub(crate) fn client(&self) -> ProvidersClient {
        self.client.clone()
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.client.commands.send(ProvidersCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ProvidersHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The way to find Codex's folder. Only [`spawn`] passes
/// [`CodexPaths::resolve`]; a test passes a folder in its tempdir.
pub(crate) type CodexPathsLookup =
    Box<dyn Fn(Option<&Path>) -> Result<CodexPaths, PathsError> + Send>;

/// The way the thread opens the store (R8): [`store::open_store`] in the app;
/// a test answers with its own `Store` and can fail an open to show the retry.
pub(crate) type StoreOpener = Box<dyn Fn() -> Result<Store, String> + Send>;

/// The way to read the settings now. The app loads `accounts.json`; a test
/// answers with its own. The settings the request used are passed in, so the
/// app can ignore them and a test can answer "the same".
pub(crate) type SettingsLookup = Box<dyn Fn(&Settings) -> Result<Settings, String> + Send>;

/// The way the runtime reports a history write failure: in the app this is
/// [`ReportedErrors::report`] (a notification and one line on stderr, once per
/// text); a test counts the calls instead.
pub(crate) type ReportError = Box<dyn Fn(&str) + Send>;

/// Start the provider thread. It is always started, whatever the two switches
/// say; the thread reads the settings again at every round.
pub(crate) fn spawn(
    app: AppPaths,
    history: Result<HistoryDir, String>,
    api: Api,
    ui: Sender<UiEvent>,
    reported_errors: Arc<ReportedErrors>,
    printed_warnings: Arc<Mutex<PrintedWarnings>>,
) -> Result<ProvidersHandle, String> {
    let (commands, received) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("seatswap-providers".to_string())
        .spawn(move || {
            run(
                app,
                history,
                api,
                ui,
                thread_stop,
                received,
                reported_errors,
                printed_warnings,
            )
        })
        .map_err(|error| format!("cannot start the provider thread: {error}"))?;
    Ok(ProvidersHandle {
        client: ProvidersClient { commands },
        stop,
        thread: Some(thread),
    })
}

#[allow(clippy::too_many_arguments)]
fn run(
    app: AppPaths,
    history: Result<HistoryDir, String>,
    api: Api,
    ui: Sender<UiEvent>,
    stop: Arc<AtomicBool>,
    commands: Receiver<ProvidersCommand>,
    reported_errors: Arc<ReportedErrors>,
    printed_warnings: Arc<Mutex<PrintedWarnings>>,
) {
    let app_for_lookup = app.clone();
    // The one store this thread keeps (R8). It opens on the first Codex round;
    // an open that failed is tried again at every round, and its error is the
    // OpenAI card's error until an open succeeds, never a silent "no logins".
    let store_app = app.clone();
    let store_reporter = Arc::clone(&reported_errors);
    let store_opener: StoreOpener = Box::new(move || match store::open_store(store_app.clone()) {
        Ok((store, warning)) => {
            if let Some(warning) = warning {
                store_reporter.report(&warning);
            }
            Ok(store)
        }
        Err(error) => Err(error.to_string()),
    });
    let mut runtime = ProvidersRuntime::new(
        history,
        api,
        Some(ui),
        app.clone(),
        store_opener,
        Box::new(CodexPaths::resolve),
        Box::new(move |_| {
            store::load_accounts(&app_for_lookup)
                .map(|file| file.settings)
                .map_err(|error| error.to_string())
        }),
        Box::new(move |text: &str| reported_errors.report(text)),
        printed_warnings,
    );
    // The first round is at the start, before the first five minutes passed.
    let mut next = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let wait = next.saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(ProvidersCommand::Opened) => {
                if let Some(settings) = settings_of(&app, &mut runtime) {
                    runtime.opened(&settings, Utc::now());
                }
            }
            Ok(ProvidersCommand::SettingsSaved) => {
                if let Some(settings) = settings_of(&app, &mut runtime) {
                    runtime.settings_saved(&settings, Utc::now());
                }
            }
            Ok(ProvidersCommand::CodexChanged) => {
                if let Some(settings) = settings_of(&app, &mut runtime) {
                    runtime.codex_changed(&settings, Utc::now());
                }
            }
            Ok(ProvidersCommand::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                if let Some(settings) = settings_of(&app, &mut runtime) {
                    runtime.poll_round(&settings, Utc::now());
                }
                next = Instant::now() + Duration::from_secs(PROVIDER_POLL_SECS);
            }
        }
    }
}

/// The settings, or `None` after reporting why they could not be read: a
/// failed load is reported once per distinct text, like a history error.
fn settings_of(app: &AppPaths, runtime: &mut ProvidersRuntime) -> Option<Settings> {
    match store::load_accounts(app) {
        Ok(file) => Some(file.settings),
        Err(error) => {
            runtime.report_error(&error.to_string());
            None
        }
    }
}

/// One run over the two cards. Tests drive it directly with settings and a
/// fixed time, so no test waits for the thread's timer.
pub(crate) struct ProvidersRuntime {
    api: Api,
    ui: Option<Sender<UiEvent>>,
    /// The app paths; the subscription list and its prices live in
    /// `accounts.json` there.
    app: AppPaths,
    /// The store the Codex round reads and saves in; none until an open
    /// succeeds. An open that failed is tried again at every round, and its
    /// error is the OpenAI card's error until then.
    store: Option<Store>,
    /// The one way the thread opens its store.
    store_opener: StoreOpener,
    codex_paths: CodexPathsLookup,
    /// The way the current settings are read once a request has ended.
    settings_lookup: SettingsLookup,
    /// The recorder, or the sentence that says why there is none.
    recorder: Result<Recorder, String>,
    /// The way a history write failure is reported. The app passes the one
    /// reporter, which prints and notifies once per text for the whole run.
    report: ReportError,
    /// The app's one list of printed history warnings, shared with the engine:
    /// the recorder's warnings (a torn line, an Ollama hour that came back
    /// smaller) are printed through it once per text.
    printed_warnings: Arc<Mutex<PrintedWarnings>>,
    /// The settings of the last round or save; the save compares against it.
    settings: Option<Settings>,
    codex: Option<ProviderView>,
    ollama: Option<ProviderView>,
    /// The saved Codex logins the pop-up lists; empty while the card is off.
    codex_logins: Vec<CodexLoginView>,
    /// A saved login whose renewal failed, with the stored copy it failed from:
    /// that copy is not asked again until it changes ([`CodexLogin::same_tokens`]).
    codex_failed: Vec<FailedCodex>,
    codex_tried: Option<DateTime<Utc>>,
    ollama_tried: Option<DateTime<Utc>>,
    /// The lists the UI received last; a send happens only when they change.
    published: (Vec<ProviderView>, Vec<CodexLoginView>),
}

/// One saved login whose renewal failed: the account, the stored copy that
/// failed, and the error's text. A new stored copy is asked again.
#[derive(Debug, Clone, PartialEq)]
struct FailedCodex {
    account: AccountRef,
    copy: CodexLogin,
    message: String,
}

/// One Codex round's work before the settings are read again: the live login's
/// answer for the card, every other saved login's answer, and whether the list
/// of saved logins could be read at all.
struct CodexRound {
    card: CodexCard,
    /// The account `sync_live` saw in `auth.json`, when it saved one.
    live: Option<AccountRef>,
    answers: Vec<CodexAnswer>,
    /// The saved logins could not be read: the pop-up keeps the ones it had.
    keep_logins: bool,
}

impl CodexRound {
    /// A round that read nothing: `text` is the card's error and the pop-up
    /// keeps the list it had, never a silent "no logins".
    fn unreadable(text: String) -> CodexRound {
        CodexRound {
            card: CodexCard::Failed(text),
            live: None,
            answers: Vec::new(),
            keep_logins: true,
        }
    }
}

/// The live Codex login's answer, as the card shows it.
enum CodexCard {
    Reading(Box<ProviderReading>),
    Failed(String),
}

/// One saved Codex login's answer in a round, before it is recorded or shown.
struct CodexAnswer {
    account: AccountRef,
    state: CodexLoginState,
    message: Option<String>,
    /// The reading's usage; a failed answer keeps its last one from the rows.
    usage: Option<Usage>,
    /// The reading to record and to make a subscription for, when the answer
    /// is a successful one.
    reading: Option<ProviderReading>,
    /// The stored copy a failed renewal is remembered from.
    copy: Option<CodexLogin>,
    /// Whether this is the login `sync_live` saw in `auth.json`.
    live: bool,
}

impl CodexAnswer {
    fn failed(
        account: AccountRef,
        state: CodexLoginState,
        message: String,
        copy: Option<CodexLogin>,
    ) -> CodexAnswer {
        CodexAnswer {
            account,
            state,
            message: Some(message),
            usage: None,
            reading: None,
            copy,
            live: false,
        }
    }
}

impl ProvidersRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        history: Result<HistoryDir, String>,
        api: Api,
        ui: Option<Sender<UiEvent>>,
        app: AppPaths,
        store_opener: StoreOpener,
        codex_paths: CodexPathsLookup,
        settings_lookup: SettingsLookup,
        report: ReportError,
        printed_warnings: Arc<Mutex<PrintedWarnings>>,
    ) -> ProvidersRuntime {
        ProvidersRuntime {
            api,
            ui,
            app,
            store: None,
            store_opener,
            codex_paths,
            settings_lookup,
            recorder: history.map(Recorder::new),
            report,
            printed_warnings,
            settings: None,
            codex: None,
            ollama: None,
            codex_logins: Vec::new(),
            codex_failed: Vec::new(),
            codex_tried: None,
            ollama_tried: None,
            published: (Vec::new(), Vec::new()),
        }
    }

    /// The cards that are switched on, Codex first.
    pub(crate) fn views(&self) -> Vec<ProviderView> {
        let mut views = Vec::new();
        if let Some(view) = &self.codex {
            views.push(view.clone());
        }
        if let Some(view) = &self.ollama {
            views.push(view.clone());
        }
        views
    }

    /// The saved Codex logins the snapshot carries; empty while the card is off.
    #[cfg(test)]
    pub(crate) fn codex_logins(&self) -> Vec<CodexLoginView> {
        self.codex_logins.clone()
    }

    /// The start and every five minutes: every card that is switched on, and
    /// every provider whose subscription has a price even while its card is off
    /// (spec R4).
    pub(crate) fn poll_round(
        &mut self,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Vec<ProviderView> {
        let previous = self.settings.replace(settings.clone());
        self.settle(settings, previous.as_ref());
        let mut due = Vec::new();
        for provider in [Provider::Codex, Provider::Ollama] {
            if provider.is_on(settings) || self.priced(provider) {
                due.push(provider);
            }
        }
        self.poll(&due, settings, now);
        self.publish();
        self.views()
    }

    /// The pop-up opened: a card whose last attempt is older than a minute.
    pub(crate) fn opened(&mut self, settings: &Settings, now: DateTime<Utc>) -> Vec<ProviderView> {
        let previous = self.settings.replace(settings.clone());
        let changed = self.settle(settings, previous.as_ref());
        let mut due = Vec::new();
        for provider in [Provider::Codex, Provider::Ollama] {
            if !provider.is_on(settings) {
                continue;
            }
            if changed.contains(&provider) {
                due.push(provider);
                continue;
            }
            let tried = match provider {
                Provider::Codex => self.codex_tried,
                Provider::Ollama => self.ollama_tried,
            };
            let older = tried.is_none_or(|tried| {
                now.signed_duration_since(tried) > chrono::Duration::seconds(PROVIDER_OPEN_SECS)
            });
            if older {
                due.push(provider);
            }
        }
        self.poll(&due, settings, now);
        self.publish();
        self.views()
    }

    /// A settings save: poll a card that was just switched on or whose folder
    /// or URL changed; a card that was switched off loses its view at once.
    pub(crate) fn settings_saved(
        &mut self,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Vec<ProviderView> {
        let previous = self.settings.replace(settings.clone());
        let changed = self.settle(settings, previous.as_ref());
        let mut due = Vec::new();
        for provider in [Provider::Codex, Provider::Ollama] {
            if !provider.is_on(settings) {
                continue;
            }
            let was_on = previous
                .as_ref()
                .is_some_and(|previous| provider.is_on(previous));
            if !was_on || changed.contains(&provider) {
                due.push(provider);
            }
        }
        self.poll(&due, settings, now);
        self.publish();
        self.views()
    }

    /// A Codex login was switched or forgotten: poll Codex at once, the way a
    /// settings save pokes the thread, so the live mark and the rows follow the
    /// switch before the next five-minute round.
    pub(crate) fn codex_changed(
        &mut self,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Vec<ProviderView> {
        let previous = self.settings.replace(settings.clone());
        self.settle(settings, previous.as_ref());
        let mut due = Vec::new();
        if Provider::Codex.is_on(settings) || self.priced(Provider::Codex) {
            due.push(Provider::Codex);
        }
        self.poll(&due, settings, now);
        self.publish();
        self.views()
    }

    /// Drop the view of a card that is now off, and of one whose folder or URL
    /// changed since `previous`: the old numbers belong to another account or
    /// server. Returns the cards whose folder or URL changed.
    fn settle(&mut self, settings: &Settings, previous: Option<&Settings>) -> Vec<Provider> {
        let mut changed = Vec::new();
        for provider in [Provider::Codex, Provider::Ollama] {
            if !provider.is_on(settings) {
                *provider.view_mut(&mut self.codex, &mut self.ollama) = None;
                *provider.tried_mut(&mut self.codex_tried, &mut self.ollama_tried) = None;
                if provider == Provider::Codex {
                    self.codex_logins.clear();
                }
            } else if previous.is_some_and(|previous| provider.setting_changed(previous, settings))
            {
                *provider.view_mut(&mut self.codex, &mut self.ollama) = None;
                if provider == Provider::Codex {
                    self.codex_logins.clear();
                }
                changed.push(provider);
            }
        }
        changed
    }

    fn poll(&mut self, due: &[Provider], settings: &Settings, now: DateTime<Utc>) {
        for provider in due {
            match provider {
                Provider::Codex => self.poll_codex(settings, now),
                Provider::Ollama => self.poll_ollama(settings, now),
            }
        }
    }

    /// The settings as they are after a request ended, or `None` after
    /// reporting a setting read that failed: a save that switches a card off or
    /// moves it is a queued command, so the request has ended before that save
    /// ran.
    fn settings_after_request(&mut self, used: &Settings) -> Option<Settings> {
        match (self.settings_lookup)(used) {
            Ok(current) => Some(current),
            Err(text) => {
                self.report_error(&text);
                None
            }
        }
    }

    /// Whether a card is still read after the request: it is on, or its
    /// subscription has a price, and it has not moved. A card that is on and
    /// unpriced is still shown. A card that is off but whose subscription has a
    /// price is still recorded; it has no view (spec §7.9).
    fn keeps(&mut self, provider: Provider, used: &Settings, current: &Settings) -> bool {
        if provider.setting_changed(used, current) {
            return false;
        }
        provider.is_on(current) || self.priced(provider)
    }

    /// One Ollama round: the balance and the 24 h usage, then the same checks
    /// every provider answer gets before it is recorded or shown.
    fn poll_ollama(&mut self, settings: &Settings, now: DateTime<Utc>) {
        let result = providers::ollama::read(&settings.ollama_url, &self.api);
        self.ollama_tried = Some(now);
        let Some(current) = self.settings_after_request(settings) else {
            return;
        };
        let on = current.ollama_card;
        let keep = self.keeps(Provider::Ollama, settings, &current);
        if !keep {
            return;
        }
        let view = match result {
            Ok(reading) => {
                self.record(&reading, now);
                self.ensure_subscription(&reading, now);
                on.then(|| success(Provider::Ollama, reading, now))
            }
            Err(error) => {
                let text = error.to_string();
                if on {
                    Some(failure(Provider::Ollama, self.ollama.as_ref(), text))
                } else {
                    // The card is off, so there is no view to show the error;
                    // report it the way every provider error is.
                    self.report_error(&text);
                    None
                }
            }
        };
        self.ollama = view;
    }

    /// One Codex round (R8): save the live login, read it, then read every other
    /// saved login with its own stored token, all before the settings are read
    /// again. A login whose renewal failed is not asked again until its stored
    /// copy changes.
    fn poll_codex(&mut self, settings: &Settings, now: DateTime<Utc>) {
        let round = self.read_codex_round(settings);
        self.codex_tried = Some(now);
        let Some(current) = self.settings_after_request(settings) else {
            return;
        };
        let on = current.codex_card;
        let keep = self.keeps(Provider::Codex, settings, &current);
        if !keep {
            return;
        }

        let mut live_answer = None;
        let card = match round.card {
            CodexCard::Reading(reading) => {
                let reading = *reading;
                live_answer = Some(CodexAnswer {
                    account: round
                        .live
                        .clone()
                        .unwrap_or_else(|| reading.account.clone()),
                    state: CodexLoginState::Ok,
                    message: None,
                    usage: reading.usage.clone(),
                    // The reading is recorded here; the saved logins' below.
                    reading: None,
                    copy: None,
                    live: true,
                });
                self.record(&reading, now);
                self.ensure_subscription(&reading, now);
                on.then(|| success(Provider::Codex, reading, now))
            }
            CodexCard::Failed(text) => {
                if let Some(live) = &round.live {
                    live_answer = Some(CodexAnswer {
                        account: live.clone(),
                        state: CodexLoginState::Error,
                        message: Some(text.clone()),
                        usage: None,
                        reading: None,
                        copy: None,
                        live: true,
                    });
                }
                if on {
                    Some(failure(Provider::Codex, self.codex.as_ref(), text))
                } else {
                    // The card is off, so there is no view to show the error.
                    self.report_error(&text);
                    None
                }
            }
        };
        self.codex = card;

        let mut answers = round.answers;
        if let Some(answer) = live_answer {
            answers.push(answer);
        }
        for answer in &answers {
            if let Some(reading) = &answer.reading {
                self.record(reading, now);
                self.ensure_subscription(reading, now);
            }
        }
        if !round.keep_logins {
            // A login whose renewal failed is kept out of the requests until
            // its stored copy changes (R8): a new copy means the user signed
            // in again. An expired answer without its message or copy cannot
            // come from `CodexAnswer::failed`; it is reported, not defaulted.
            let mut failed = Vec::new();
            for answer in &answers {
                if answer.state != CodexLoginState::Expired {
                    continue;
                }
                match (answer.copy.clone(), answer.message.clone()) {
                    (Some(copy), Some(message)) => failed.push(FailedCodex {
                        account: answer.account.clone(),
                        copy,
                        message,
                    }),
                    (Some(_), None) => self.report_error(EXPIRED_WITHOUT_MESSAGE),
                    (None, _) => self.report_error(EXPIRED_WITHOUT_COPY),
                }
            }
            self.codex_failed = failed;
            if on {
                self.publish_codex_logins(&answers);
            } else {
                // The card is off: the pop-up has nowhere to show the rows.
                self.codex_logins.clear();
            }
        }
    }

    /// The Codex round's network and store work, before the settings are read
    /// again: nothing in here is recorded or shown.
    fn read_codex_round(&mut self, settings: &Settings) -> CodexRound {
        let paths = match (self.codex_paths)(settings.codex_home.as_deref()) {
            Ok(paths) => paths,
            Err(error) => {
                return CodexRound::unreadable(
                    providers::ProviderError::Setting(error.to_string()).to_string(),
                )
            }
        };
        // The store opens on the first Codex round; an earlier failed open is
        // tried again here, and its error is the card's until an open succeeds.
        if self.store.is_none() {
            match (self.store_opener)() {
                Ok(store) => self.store = Some(store),
                Err(text) => return CodexRound::unreadable(text),
            }
        }
        let store = match self.store.as_ref() {
            Some(store) => store,
            None => return CodexRound::unreadable("the store is not open".to_string()),
        };
        let secrets = store.secrets();
        let (live, card) = match providers::codex::sync_live(&self.app, &paths, secrets) {
            Ok(live) => {
                let reading = providers::codex::read(&paths, &self.api);
                (Some(live.account.clone()), reading)
            }
            // Without the live login the round cannot tell which saved login is
            // live, so it asks none of them (B2): the rows keep their last
            // values and state until the live login can be read again.
            Err(error) => {
                return CodexRound {
                    card: CodexCard::Failed(error.to_string()),
                    live: None,
                    answers: Vec::new(),
                    keep_logins: true,
                }
            }
        };
        let card = match card {
            Ok(reading) => CodexCard::Reading(Box::new(reading)),
            Err(error) => CodexCard::Failed(error.to_string()),
        };
        let subscriptions = match store::load_accounts(&self.app) {
            Ok(file) => file.provider_subscriptions,
            Err(error) => {
                // The list of saved logins cannot be known; the pop-up keeps
                // the one it had.
                self.report_error(&error.to_string());
                return CodexRound {
                    card,
                    live,
                    answers: Vec::new(),
                    keep_logins: true,
                };
            }
        };

        let mut answers = Vec::new();
        for subscription in subscriptions
            .iter()
            .filter(|subscription| subscription.account.org == CODEX_ORG)
        {
            if live.as_ref() == Some(&subscription.account) {
                continue;
            }
            let account = subscription.account.clone();
            let copy = match secrets.get_codex(&providers::codex::secret_id(&account)) {
                Ok(Some(copy)) => copy,
                // No stored copy: this subscription's login is not saved.
                Ok(None) => continue,
                Err(error) => {
                    answers.push(CodexAnswer::failed(
                        account,
                        CodexLoginState::Error,
                        error.to_string(),
                        None,
                    ));
                    continue;
                }
            };
            if let Some(failed) = self
                .codex_failed
                .iter()
                .find(|failed| failed.account == account && failed.copy.same_tokens(&copy))
            {
                answers.push(CodexAnswer::failed(
                    account,
                    CodexLoginState::Expired,
                    failed.message.clone(),
                    Some(copy),
                ));
                continue;
            }
            match providers::codex::read_saved(&self.app, secrets, &account, &self.api) {
                Ok(reading) => answers.push(CodexAnswer {
                    account,
                    state: CodexLoginState::Ok,
                    message: None,
                    usage: reading.usage.clone(),
                    reading: Some(reading),
                    copy: Some(copy),
                    live: false,
                }),
                Err(providers::ProviderError::RenewFailed(message)) => answers.push(
                    CodexAnswer::failed(account, CodexLoginState::Expired, message, Some(copy)),
                ),
                Err(error) => answers.push(CodexAnswer::failed(
                    account,
                    CodexLoginState::Error,
                    error.to_string(),
                    Some(copy),
                )),
            }
        }
        CodexRound {
            card,
            live,
            answers,
            keep_logins: false,
        }
    }

    /// The pop-up's Codex rows (R8): the live login first, then the rest by
    /// abbreviation, from the round's answers and the subscriptions in
    /// `accounts.json`. A failed reading keeps the usage of its last one, like
    /// the card. A list that cannot be built keeps the rows it had.
    fn publish_codex_logins(&mut self, answers: &[CodexAnswer]) {
        let subscriptions = match store::load_accounts(&self.app) {
            Ok(file) => file.provider_subscriptions,
            Err(error) => {
                self.report_error(&error.to_string());
                return;
            }
        };
        let previous = std::mem::take(&mut self.codex_logins);
        let mut views = Vec::new();
        for answer in answers {
            let Some(subscription) = subscriptions
                .iter()
                .find(|subscription| subscription.account == answer.account)
            else {
                self.report_error(&format!(
                    "the saved Codex login {} has no subscription in accounts.json",
                    answer.account.acc
                ));
                continue;
            };
            let usage = answer.usage.clone().or_else(|| {
                previous
                    .iter()
                    .find(|view| view.account == answer.account)
                    .and_then(|view| view.usage.clone())
            });
            views.push(CodexLoginView {
                account: answer.account.clone(),
                abbr: subscription.abbr.clone(),
                label: subscription.label.clone(),
                color: subscription.color.clone(),
                plan: subscription.plan.clone(),
                live: answer.live,
                usage,
                state: answer.state,
                message: answer.message.clone(),
            });
        }
        views
            .sort_by(|first, second| (!first.live, &first.abbr).cmp(&(!second.live, &second.abbr)));
        self.codex_logins = views;
    }

    /// The subscriptions of `provider`'s org, or none after reporting a file
    /// that cannot be read. A read failure is never a silent "no price".
    /// One org can have several (one per Codex login, R8), so nothing here may
    /// assume a single subscription.
    fn subscriptions(&mut self, provider: Provider) -> Vec<ProviderSubscription> {
        match store::load_accounts(&self.app) {
            Ok(file) => file
                .provider_subscriptions
                .into_iter()
                .filter(|subscription| subscription.account.org == provider.name())
                .collect(),
            Err(error) => {
                self.report_error(&error.to_string());
                Vec::new()
            }
        }
    }

    /// Whether any subscription of this provider has a price, so the provider is
    /// polled even with its card off (spec R4).
    fn priced(&mut self, provider: Provider) -> bool {
        self.subscriptions(provider)
            .iter()
            .any(|subscription| subscription.price.is_some())
    }

    /// Make sure a successful reading's account has a subscription through the
    /// one [`ops::ensure_provider_subscription`], which writes `accountAdded`
    /// and `planChanged`. Its warnings are reported here like any provider
    /// error and never stop the poll.
    fn ensure_subscription(&mut self, reading: &ProviderReading, now: DateTime<Utc>) {
        let warnings = {
            let recorder = self.recorder.as_ref().ok();
            ops::ensure_provider_subscription(
                &self.app,
                recorder,
                &reading.account,
                reading.plan.as_deref(),
                now,
            )
        };
        for text in &warnings {
            self.report_error(text);
        }
        self.print_history_warnings();
    }

    /// Record a reading that carries a usage and Ollama's hourly buckets; one
    /// without a usage records no sample. A recording error is reported the one
    /// way the app reports a history write, and never changes the card. An answer
    /// whose buckets cannot be read is reported too, without failing the card; a
    /// usage call that itself failed has no buckets (`hours` is `None`) and is
    /// only the card's spend error, as before O-1. A history that could not be
    /// opened at start is already in the snapshot's error and was notified there,
    /// so this thread records nothing and reports nothing for it.
    fn record(&mut self, reading: &ProviderReading, now: DateTime<Utc>) {
        let mut failures = Vec::new();
        if let Some(usage) = &reading.usage {
            if let Ok(recorder) = &mut self.recorder {
                if let Err(error) = recorder.reading(
                    &reading.account,
                    reading.plan.as_deref(),
                    usage,
                    now,
                    Source::Poll,
                ) {
                    failures.push(ops::history_unwritable(&error));
                }
            }
        }
        if let Some(hours) = &reading.hours {
            match hours {
                Ok(hours) => {
                    if let Ok(recorder) = &mut self.recorder {
                        if let Err(error) = recorder.ollama_hours(hours) {
                            failures.push(ops::history_unwritable(&error));
                        }
                    }
                }
                Err(reason) => failures.push(reason.clone()),
            }
        }
        for text in &failures {
            self.report_error(text);
        }
        self.print_history_warnings();
    }

    /// The recorder's warnings (spec §1.2), printed the one way the engine
    /// prints its own: once per text for the whole run. Core never prints.
    fn print_history_warnings(&mut self) {
        if let Ok(recorder) = &mut self.recorder {
            let warnings = recorder.take_warnings();
            if !warnings.is_empty() {
                super::lock(&self.printed_warnings).print(&warnings);
            }
        }
    }

    /// Report an error text through the injected reporter: in the app that is
    /// the one function that prints and notifies once per text for the whole
    /// run. A test's reporter counts the calls.
    pub(crate) fn report_error(&mut self, text: &str) {
        (self.report)(text);
    }

    fn publish(&mut self) {
        let list = (self.views(), self.codex_logins.clone());
        if list == self.published {
            return;
        }
        self.published = list.clone();
        if let Some(ui) = &self.ui {
            let _ = ui.send(UiEvent::Providers(ProviderLists {
                cards: list.0,
                codex_logins: list.1,
            }));
        }
    }
}

/// A reading's card: its numbers, the moment it was read, and no error.
fn success(provider: Provider, reading: ProviderReading, now: DateTime<Utc>) -> ProviderView {
    let (spend_24h, spend_24h_error) = match reading.spend_24h {
        Some(Ok(spend)) => (Some(spend), None),
        Some(Err(error)) => (None, Some(error)),
        None => (None, None),
    };
    ProviderView {
        provider: provider.name().to_string(),
        title: provider.title().to_string(),
        plan: reading.plan,
        usage: reading.usage,
        credits: reading.credits,
        spend_24h,
        spend_24h_error,
        fetched_at: Some(now),
        error: None,
    }
}

/// A failed reading: every number of the last one and its `fetched_at` stay,
/// and `error` is the provider error's own text.
fn failure(provider: Provider, last: Option<&ProviderView>, error: String) -> ProviderView {
    ProviderView {
        provider: provider.name().to_string(),
        title: provider.title().to_string(),
        plan: last.and_then(|view| view.plan.clone()),
        usage: last.and_then(|view| view.usage.clone()),
        credits: last.and_then(|view| view.credits.clone()),
        spend_24h: last.and_then(|view| view.spend_24h),
        spend_24h_error: last.and_then(|view| view.spend_24h_error.clone()),
        fetched_at: last.and_then(|view| view.fetched_at),
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    use tempfile::tempdir;

    use seatswap_core::history::{AccountRef, Hosts, OllamaHourRecord, Range, Sample};
    use seatswap_core::store::{FileSecrets, Store};

    use crate::gui::ReportedErrors;

    const AUTH: &str = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","last_refresh":"2026-11-14T16:53:03.750022420Z","tokens":{"id_token":"eyJhbGciOi.id.sig","access_token":"eyJhbGciOi.access.sig","refresh_token":"rt_9f2c","account_id":"0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13","login_owner_id":"user-sam","oauth_provenance":{"issuer":"https://auth.openai.com","client_id":"app_4c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f"}}}"#;

    /// The fixture's live Codex account id.
    const LIVE_ACCOUNT_ID: &str = "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13";
    /// A second Codex login's account id and its own access token (R8: every
    /// saved login is asked with its own, never with the live one's).
    const SAVED_ACCOUNT_ID: &str = "7d2b9e4c-1a3f-4c5e-8b6d-9f0a1c2e3d4b";
    const SAVED_AUTH: &str = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","last_refresh":"2026-11-15T09:10:00.000000000Z","tokens":{"id_token":"eyJhbGciOi.saved.id","access_token":"eyJhbGciOi.saved.sig","refresh_token":"rt_51ab","account_id":"7d2b9e4c-1a3f-4c5e-8b6d-9f0a1c2e3d4b","login_owner_id":"user-sam","oauth_provenance":{"issuer":"https://auth.openai.com","client_id":"app_4c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f"}}}"#;

    /// The second login's usage answer: the `pro` plan of its own account.
    const PRO_ANSWER: &str = r#"{"user_id":"user-sam","account_id":"7d2b9e4c-1a3f-4c5e-8b6d-9f0a1c2e3d4b","email":"sam@mail.example","plan_type":"pro",
  "rate_limit":{"allowed":true,"limit_reached":false,
   "primary_window":{"used_percent":3,"limit_window_seconds":18000,"reset_after_seconds":17100,"reset_at":1794693195},
   "secondary_window":{"used_percent":20,"limit_window_seconds":604800,"reset_after_seconds":316800,"reset_at":1795279995}},
  "code_review_rate_limit":null,"additional_rate_limits":null,
  "credits":{"has_credits":false,"unlimited":false,"overage_limit_reached":false,"balance":"0"},
  "spend_control":{"reached":false,"individual_limit":null},"rate_limit_reached_type":null}"#;

    /// The fixture's live login with another access token, as Codex renews it in
    /// place, and a second `auth.json` for another account (a new account id).
    const AUTH_RENEWED: &str = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","last_refresh":"2026-11-15T09:10:00.000000000Z","tokens":{"id_token":"eyJhbGciOi.id.sig","access_token":"eyJhbGciOi.renewed.sig","refresh_token":"rt_9f2c","account_id":"0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13","login_owner_id":"user-sam","oauth_provenance":{"issuer":"https://auth.openai.com","client_id":"app_4c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f"}}}"#;
    const AUTH_OTHER_ACCOUNT: &str = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","last_refresh":"2026-11-15T10:00:00.000000000Z","tokens":{"id_token":"eyJhbGciOi.other.id","access_token":"eyJhbGciOi.other.sig","refresh_token":"rt_77cd","account_id":"7d2b9e4c-1a3f-4c5e-8b6d-9f0a1c2e3d4b","login_owner_id":"user-sam","oauth_provenance":{"issuer":"https://auth.openai.com","client_id":"app_4c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f"}}}"#;

    const PLUS_ANSWER: &str = r#"{"user_id":"user-sam","account_id":"0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13","email":"sam@mail.example","plan_type":"plus",
  "rate_limit":{"allowed":true,"limit_reached":false,
   "primary_window":{"used_percent":12,"limit_window_seconds":18000,"reset_after_seconds":17100,"reset_at":1794693195},
   "secondary_window":{"used_percent":48,"limit_window_seconds":604800,"reset_after_seconds":316800,"reset_at":1795279995}},
  "code_review_rate_limit":null,"additional_rate_limits":null,
  "credits":{"has_credits":false,"unlimited":false,"overage_limit_reached":false,"balance":"0"},
  "spend_control":{"reached":false,"individual_limit":null},"rate_limit_reached_type":null}"#;

    const BALANCE_ZERO: &str = r#"{"included":{"balance_usd":0,"allowance_usd":0}}"#;
    const BALANCE: &str = r#"{"included":{"balance_usd":212.4,"allowance_usd":300,"period":{"until":"2026-12-03T00:00:00Z"}},"purchased":{"balance_usd":0}}"#;
    const USAGE_ANSWER: &str = r#"{"totals":{"request_count":412,"usage_usd":3.18,"input_tokens":5120000,"cached_input_tokens":3900000,"output_tokens":640000}}"#;
    const USAGE_WITH_HOURS: &str = r#"{"totals":{"request_count":17,"usage_usd":0.12,"input_tokens":210000,"cached_input_tokens":160000,"output_tokens":26000},"buckets":[{"from":"2026-11-14T08:00:00Z","until":"2026-11-14T09:00:00Z","request_count":17,"usage_usd":0.12,"input_tokens":210000,"cached_input_tokens":160000,"output_tokens":26000}]}"#;

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("a test date")
    }

    fn settings() -> Settings {
        Settings::default()
    }

    fn codex_on() -> Settings {
        Settings {
            codex_card: true,
            ..Settings::default()
        }
    }

    fn ollama_on(url: &str) -> Settings {
        Settings {
            ollama_card: true,
            ollama_url: url.to_string(),
            ..Settings::default()
        }
    }

    fn write_auth(folder: &Path) {
        std::fs::create_dir_all(folder).expect("the Codex folder");
        std::fs::write(folder.join("auth.json"), AUTH).expect("auth.json");
    }

    fn history(dir: &Path) -> HistoryDir {
        HistoryDir::at(&dir.join("history"), "atlas")
    }

    fn test_app(dir: &Path) -> AppPaths {
        AppPaths::at(&dir.join("data"))
    }

    fn test_store(dir: &Path) -> Store {
        let app = test_app(dir);
        Store::new(app.clone(), Box::new(FileSecrets::new(app.secrets_dir())))
    }

    /// The opener a runtime gets in a test: a store with `FileSecrets` in the
    /// tempdir's secrets folder.
    fn test_store_opener(app: AppPaths) -> StoreOpener {
        Box::new(move || {
            Ok(Store::new(
                app.clone(),
                Box::new(FileSecrets::new(app.secrets_dir())),
            ))
        })
    }

    fn test_runtime(dir: &Path, api: Api, codex_home: &Path) -> ProvidersRuntime {
        let codex_home = codex_home.to_path_buf();
        test_runtime_with(
            dir,
            api,
            Box::new(move |_| Ok(CodexPaths::at(&codex_home))),
            same_settings(),
            Box::new(|_| {}),
        )
    }

    /// A settings lookup that answers the settings the request used: the tests
    /// that do not change anything during the request.
    fn same_settings() -> SettingsLookup {
        Box::new(|used| Ok(used.clone()))
    }

    fn test_runtime_with(
        dir: &Path,
        api: Api,
        codex_paths: CodexPathsLookup,
        settings_lookup: SettingsLookup,
        report: ReportError,
    ) -> ProvidersRuntime {
        test_runtime_with_app(
            dir,
            api,
            codex_paths,
            settings_lookup,
            report,
            test_app(dir),
        )
    }

    fn test_runtime_with_app(
        dir: &Path,
        api: Api,
        codex_paths: CodexPathsLookup,
        settings_lookup: SettingsLookup,
        report: ReportError,
        app: AppPaths,
    ) -> ProvidersRuntime {
        let store_opener = test_store_opener(app.clone());
        ProvidersRuntime::new(
            Ok(history(dir)),
            api,
            None,
            app,
            store_opener,
            codex_paths,
            settings_lookup,
            report,
            Arc::new(Mutex::new(PrintedWarnings::new())),
        )
    }

    /// A Codex lookup that honours the `codexHome` setting, as
    /// `CodexPaths::resolve` does.
    fn explicit_codex_paths() -> CodexPathsLookup {
        Box::new(|explicit| match explicit {
            Some(path) => Ok(CodexPaths::at(path)),
            None => Ok(CodexPaths::at(Path::new("no-explicit-codex-home"))),
        })
    }

    fn codex_api(base: &str) -> Api {
        Api::new().with_codex_url(&format!("{base}/backend-api/wham/usage"))
    }

    /// A Codex URL on a local port that has just closed: a test that expects no
    /// request can never reach chatgpt.com if it is wrong.
    fn offline_api() -> Api {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("a local address"));
        drop(listener);
        Api::new().with_codex_url(&format!("{url}/backend-api/wham/usage"))
    }

    /// A local server that answers with `replies` in order (the last one again
    /// after that) and reports each request's path. It stops when `finish` sets
    /// the flag, or after a moment without a request.
    struct Fake {
        base: String,
        paths: Receiver<String>,
        /// Each request's `Authorization` header, in the same order as `paths`.
        authorizations: Receiver<Option<String>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Fake {
        fn start(replies: Vec<(u16, String)>) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a local server");
            listener
                .set_nonblocking(true)
                .expect("a non-blocking listener");
            let base = format!("http://{}", listener.local_addr().expect("a local address"));
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let (sender, receiver) = mpsc::channel();
            let (auth_sender, auth_receiver) = mpsc::channel();
            let thread = thread::spawn(move || {
                let mut served = 0usize;
                let mut deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if thread_stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false).expect("a blocking stream");
                            let (path, authorization) = read_request(&stream);
                            let _ = sender.send(path);
                            let _ = auth_sender.send(authorization);
                            let (status, body) = replies
                                .get(served)
                                .cloned()
                                .or_else(|| replies.last().cloned())
                                .unwrap_or((200, String::new()));
                            served += 1;
                            write_reply(&mut stream, status, &body);
                            deadline = Instant::now() + Duration::from_millis(300);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return;
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                }
            });
            Fake {
                base,
                paths: receiver,
                authorizations: auth_receiver,
                stop,
                thread: Some(thread),
            }
        }

        fn finish(mut self) -> Vec<String> {
            self.stop_and_join();
            self.paths.try_iter().collect()
        }

        /// Every request's path and `Authorization` header, in order.
        fn finish_requests(mut self) -> (Vec<String>, Vec<Option<String>>) {
            self.stop_and_join();
            (
                self.paths.try_iter().collect(),
                self.authorizations.try_iter().collect(),
            )
        }

        fn stop_and_join(&mut self) {
            thread::sleep(Duration::from_millis(50));
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("the server thread");
            }
        }
    }

    /// One request's path and its `Authorization` header, when it has one.
    fn read_request(stream: &std::net::TcpStream) -> (String, Option<String>) {
        let mut reader = BufReader::new(stream.try_clone().expect("a stream clone"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("the request line");
        let path = line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let mut authorization = None;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).expect("a header") == 0 {
                break;
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_string());
                }
            }
        }
        (path, authorization)
    }

    fn write_reply(stream: &mut std::net::TcpStream, status: u16, body: &str) {
        let text = format!(
            "HTTP/1.1 {status} seatswap-test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(text.as_bytes()).expect("a reply");
    }

    fn codex_login(text: &str) -> CodexLogin {
        serde_json::from_str(text).expect("a Codex login")
    }

    fn account(acc: &str) -> AccountRef {
        AccountRef {
            acc: acc.to_string(),
            org: CODEX_ORG.to_string(),
        }
    }

    fn only_view(views: Vec<ProviderView>) -> ProviderView {
        assert_eq!(views.len(), 1, "one switched-on card is one view");
        views.into_iter().next().expect("one view")
    }

    /// R8: every poll saves the live Codex login, so a login Codex renewed — or
    /// the user just signed in with — is the one seatswap stores, and a new
    /// account id gets its subscription.
    #[test]
    fn the_live_codex_login_is_saved_at_each_poll() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![
            (200, PLUS_ANSWER.to_string()),
            (200, PLUS_ANSWER.to_string()),
            (200, PRO_ANSWER.to_string()),
            (200, PLUS_ANSWER.to_string()),
        ]);
        let api = codex_api(&server.base);
        let mut runtime = test_runtime(dir.path(), api, &codex_home);
        let store = test_store(dir.path());
        let live = account(LIVE_ACCOUNT_ID);

        only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let saved = store
            .secrets()
            .get_codex(&providers::codex::secret_id(&live))
            .expect("the stored copy")
            .expect("the live login is saved");
        assert_eq!(saved, codex_login(AUTH));
        let file = store.load().expect("accounts.json");
        let subscription = file
            .provider_subscription(&live)
            .expect("the live login's subscription");
        assert_eq!(subscription.abbr, "OA");
        assert_eq!(subscription.plan.as_deref(), Some("Plus"));

        // Codex renewed the login in place: the next poll saves the new copy.
        std::fs::write(codex_home.join("auth.json"), AUTH_RENEWED).expect("the renewed auth.json");
        only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:05:00Z")));
        let saved = store
            .secrets()
            .get_codex(&providers::codex::secret_id(&live))
            .expect("the stored copy")
            .expect("the renewed login is saved");
        assert_eq!(saved, codex_login(AUTH_RENEWED));

        // The user signed in with another account: it is saved and gets its own
        // subscription, beside the one the first login has.
        std::fs::write(codex_home.join("auth.json"), AUTH_OTHER_ACCOUNT)
            .expect("the other account's auth.json");
        only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:10:00Z")));
        let other = account(SAVED_ACCOUNT_ID);
        let saved = store
            .secrets()
            .get_codex(&providers::codex::secret_id(&other))
            .expect("the stored copy")
            .expect("the other login is saved");
        assert_eq!(saved, codex_login(AUTH_OTHER_ACCOUNT));
        let file = store.load().expect("accounts.json");
        let subscription = file
            .provider_subscription(&other)
            .expect("the new account id's subscription");
        assert_eq!(subscription.abbr, "O2", "OA is taken by the first login");
        assert_eq!(subscription.plan.as_deref(), Some("Pro"));

        let logins = runtime.codex_logins();
        assert_eq!(logins.len(), 2, "one row per saved login");
        assert_eq!(logins[0].account, other);
        assert!(logins[0].live, "the live login comes first");
        assert_eq!(logins[0].abbr, "O2");
        assert_eq!(logins[0].plan.as_deref(), Some("Pro"));
        assert_eq!(logins[1].account, live);
        assert!(!logins[1].live);
        assert_eq!(logins[1].abbr, "OA");
        assert_eq!(logins[1].state, CodexLoginState::Ok);
        let _ = server.finish();
    }

    /// R8: every saved Codex login is asked with its own stored token, never
    /// with the live one's, and the pop-up lists each with its own plan.
    #[test]
    fn every_saved_codex_login_is_polled_with_its_own_token() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let store = test_store(dir.path());
        let live = account(LIVE_ACCOUNT_ID);
        let other = account(SAVED_ACCOUNT_ID);
        store
            .secrets()
            .set_codex(
                &providers::codex::secret_id(&other),
                &codex_login(SAVED_AUTH),
            )
            .expect("the saved copy");
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

        let server = Fake::start(vec![
            (200, PLUS_ANSWER.to_string()),
            (200, PRO_ANSWER.to_string()),
        ]);
        let mut runtime = test_runtime(dir.path(), codex_api(&server.base), &codex_home);
        let view = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let (paths, authorizations) = server.finish_requests();

        assert_eq!(view.error, None);
        assert_eq!(
            view.plan.as_deref(),
            Some("Plus"),
            "the card is the live login"
        );
        assert_eq!(
            paths,
            vec![
                "/backend-api/wham/usage".to_string(),
                "/backend-api/wham/usage".to_string()
            ],
            "the live login and the saved one are each asked once"
        );
        assert_eq!(
            authorizations,
            vec![
                Some("Bearer eyJhbGciOi.access.sig".to_string()),
                Some("Bearer eyJhbGciOi.saved.sig".to_string())
            ],
            "the saved login is asked with its own token"
        );

        let logins = runtime.codex_logins();
        assert_eq!(logins.len(), 2, "one row per saved login");
        assert_eq!(logins[0].account, live);
        assert!(logins[0].live);
        assert_eq!(logins[0].abbr, "OA");
        assert_eq!(logins[0].plan.as_deref(), Some("Plus"));
        assert_eq!(logins[0].state, CodexLoginState::Ok);
        assert!(logins[0].usage.is_some());
        assert_eq!(logins[1].account, other);
        assert!(!logins[1].live);
        assert_eq!(logins[1].abbr, "OA2");
        assert_eq!(logins[1].plan.as_deref(), Some("Pro"));
        assert_eq!(logins[1].state, CodexLoginState::Ok);
        assert!(logins[1].usage.is_some());
        assert_eq!(logins[1].message, None);

        // A card switched off keeps no rows, and a round after the switch makes
        // no request while no price keeps the provider polled.
        let mut off = codex_on();
        off.codex_card = false;
        let server = Fake::start(vec![]);
        runtime.api = codex_api(&server.base);
        assert!(runtime
            .poll_round(&off, utc("2026-11-14T09:05:00Z"))
            .is_empty());
        let paths = server.finish();
        assert!(
            paths.is_empty(),
            "a switched-off card made a request: {paths:?}"
        );
        assert!(
            runtime.codex_logins().is_empty(),
            "the rows are gone while the card is off"
        );
    }

    /// R8: a login whose renewal failed is `expired` with the error's text and
    /// is not asked again at every poll; a new stored copy — the user signed in
    /// again — is asked at the next round.
    #[test]
    fn a_login_whose_renewal_failed_is_not_asked_again_until_its_copy_changes() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let store = test_store(dir.path());
        let other = account(SAVED_ACCOUNT_ID);
        store
            .secrets()
            .set_codex(
                &providers::codex::secret_id(&other),
                &codex_login(SAVED_AUTH),
            )
            .expect("the saved copy");
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

        // Round one: the live login; then the saved one, whose access token is
        // dead (401) and whose refresh token is dead too (400). Round two: the
        // live login only.
        let server = Fake::start(vec![
            (200, PLUS_ANSWER.to_string()),
            (401, String::new()),
            (400, String::new()),
            (200, PLUS_ANSWER.to_string()),
        ]);
        let api =
            codex_api(&server.base).with_codex_token_url(&format!("{}/oauth/token", server.base));
        let mut runtime = test_runtime(dir.path(), api, &codex_home);
        only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let expired_row = |runtime: &ProvidersRuntime| {
            runtime
                .codex_logins()
                .into_iter()
                .find(|login| login.account == other)
                .expect("the saved login's row")
        };
        let expired = expired_row(&runtime);
        assert_eq!(expired.state, CodexLoginState::Expired);
        assert_eq!(
            expired.message.as_deref(),
            Some("Sign in again with codex login.")
        );
        assert!(expired.usage.is_none());

        runtime.poll_round(&codex_on(), utc("2026-11-14T09:05:00Z"));
        let (paths, _) = server.finish_requests();
        assert_eq!(
            paths.len(),
            4,
            "the dead login was asked once, not at the second poll: {paths:?}"
        );
        assert_eq!(expired_row(&runtime).state, CodexLoginState::Expired);

        // The user signed in again and the new copy is saved: the next round
        // asks it again and it is OK.
        let signed_in_again = SAVED_AUTH.replace("eyJhbGciOi.saved.sig", "eyJhbGciOi.signedin.sig");
        store
            .secrets()
            .set_codex(
                &providers::codex::secret_id(&other),
                &codex_login(&signed_in_again),
            )
            .expect("the new copy");
        let server = Fake::start(vec![
            (200, PLUS_ANSWER.to_string()),
            (200, PRO_ANSWER.to_string()),
        ]);
        runtime.api = codex_api(&server.base);
        runtime.poll_round(&codex_on(), utc("2026-11-14T09:10:00Z"));
        let (paths, authorizations) = server.finish_requests();
        assert_eq!(paths.len(), 2, "the new copy is asked again: {paths:?}");
        assert_eq!(
            authorizations[1],
            Some("Bearer eyJhbGciOi.signedin.sig".to_string()),
            "the new copy's own token"
        );
        let row = expired_row(&runtime);
        assert_eq!(row.state, CodexLoginState::Ok);
        assert_eq!(row.message, None);
        assert!(row.usage.is_some());
    }

    /// B1: a store that could not be opened is not written off for the whole
    /// run. The first round shows its error; the next round opens it again and
    /// reads Codex.
    #[test]
    fn a_store_that_failed_to_open_is_opened_again_at_the_next_round() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let api = codex_api(&server.base);
        let app = test_app(dir.path());
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opener_app = app.clone();
        let opener_attempts = Arc::clone(&attempts);
        let opener: StoreOpener = Box::new(move || {
            let attempt = opener_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if attempt == 0 {
                Err("the store could not be opened: accounts.json is corrupt".to_string())
            } else {
                Ok(Store::new(
                    opener_app.clone(),
                    Box::new(FileSecrets::new(opener_app.secrets_dir())),
                ))
            }
        });
        let folder = codex_home.clone();
        let mut runtime = ProvidersRuntime::new(
            Ok(history(dir.path())),
            api,
            None,
            app,
            opener,
            Box::new(move |_| Ok(CodexPaths::at(&folder))),
            same_settings(),
            Box::new(|_| {}),
            Arc::new(Mutex::new(PrintedWarnings::new())),
        );

        let first = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        assert_eq!(
            first.error.as_deref(),
            Some("the store could not be opened: accounts.json is corrupt")
        );
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the first round tried the open once"
        );

        let second = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:05:00Z")));
        let paths = server.finish();
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the next round opened the store again"
        );
        assert_eq!(second.error, None, "the card reads Codex after the open");
        assert!(second.usage.is_some());
        assert_eq!(
            paths,
            vec!["/backend-api/wham/usage".to_string()],
            "only the round after the open asked Codex"
        );
    }

    /// B2: while the live login cannot be read, the round does not know which
    /// saved login is live, so it asks none of them; their rows keep the last
    /// values and state.
    #[test]
    fn no_saved_login_is_asked_while_the_live_login_is_unknown() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let store = test_store(dir.path());
        let other = account(SAVED_ACCOUNT_ID);
        store
            .secrets()
            .set_codex(
                &providers::codex::secret_id(&other),
                &codex_login(SAVED_AUTH),
            )
            .expect("the saved copy");
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

        let server = Fake::start(vec![
            (200, PLUS_ANSWER.to_string()),
            (200, PRO_ANSWER.to_string()),
        ]);
        let mut runtime = test_runtime(dir.path(), codex_api(&server.base), &codex_home);
        only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let (paths, _) = server.finish_requests();
        assert_eq!(paths.len(), 2, "the live and the saved login were asked");
        let before = runtime.codex_logins();
        assert_eq!(before.len(), 2, "both rows were read");

        // Codex's login is now kept in the keyring: sync_live cannot tell which
        // account is live, so no saved login is asked and none is renewed.
        std::fs::write(
            codex_home.join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":null}"#,
        )
        .expect("the keyring-shaped auth.json");
        let server = Fake::start(vec![]);
        runtime.api = codex_api(&server.base);
        let view = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:05:00Z")));
        let paths = server.finish();

        assert!(paths.is_empty(), "a saved login was asked: {paths:?}");
        assert!(
            view.error
                .as_deref()
                .is_some_and(|text| text.contains("system keyring")),
            "the card does not say why the live login is unknown: {:?}",
            view.error
        );
        assert_eq!(
            runtime.codex_logins(),
            before,
            "the rows keep their last values and state"
        );
    }

    #[test]
    fn a_card_that_is_off_makes_no_request_and_reads_no_file() {
        let dir = tempdir().expect("tempdir");
        // A folder where auth.json is a directory: reading it would fail and
        // still make a card, so an empty list proves the file was not read.
        let codex_home = dir.path().join("codex");
        std::fs::create_dir_all(codex_home.join("auth.json")).expect("the folder");
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let api = codex_api(&server.base);
        let mut runtime = test_runtime(dir.path(), api, &codex_home);
        // The default Ollama URL is this machine's real server; point it at the
        // test server so a broken off-rule is a request the test sees.
        let both_off = Settings {
            ollama_url: server.base.clone(),
            ..settings()
        };

        let views = runtime.poll_round(&both_off, utc("2026-11-14T09:00:00Z"));

        assert!(views.is_empty(), "both cards are off");
        let paths = server.finish();
        assert!(
            paths.is_empty(),
            "a card that is off sent a request: {paths:?}"
        );
    }

    #[test]
    fn a_reading_goes_into_the_snapshot_and_the_history() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let api = codex_api(&server.base);
        let mut runtime = test_runtime(dir.path(), api, &codex_home);

        let view = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let paths = server.finish();

        assert_eq!(view.provider, "codex");
        assert_eq!(view.title, "OpenAI (Codex)");
        assert_eq!(view.plan.as_deref(), Some("Plus"));
        assert!(view.usage.is_some());
        assert_eq!(view.error, None);
        assert_eq!(view.fetched_at, Some(utc("2026-11-14T09:00:00Z")));
        assert_eq!(paths, vec!["/backend-api/wham/usage".to_string()]);

        let read = history(dir.path())
            .read::<Sample>(&Range::ALL, &Hosts::All)
            .expect("the samples");
        assert_eq!(read.records.len(), 1, "one sample line");
        assert_eq!(read.records[0].record.account.org, "codex");
        assert!(
            !read.records[0].record.account.is_claude(),
            "a Codex account is not a Claude account"
        );

        let folder = history(dir.path()).root().join("atlas");
        let line = std::fs::read_dir(folder)
            .expect("the host folder")
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("samples-"))
            .map(|entry| std::fs::read_to_string(entry.path()).expect("the samples file"))
            .expect("a samples file");
        assert!(
            line.contains(r#""org":"codex""#),
            "the line names codex: {line}"
        );
    }

    #[test]
    fn an_error_keeps_the_last_reading() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string()), (500, String::new())]);
        let api = codex_api(&server.base);
        let mut runtime = test_runtime(dir.path(), api, &codex_home);

        let first = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let second = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:10:00Z")));
        let _ = server.finish();

        assert_eq!(second.usage, first.usage, "the numbers stay");
        assert_eq!(second.plan, first.plan);
        assert_eq!(
            second.fetched_at,
            Some(utc("2026-11-14T09:00:00Z")),
            "the reading keeps its time"
        );
        assert_eq!(second.error.as_deref(), Some("server error 500"));
    }

    #[test]
    fn the_error_texts_are_exact() {
        // No Codex login: auth.json does not exist.
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        let mut runtime = test_runtime(dir.path(), offline_api(), &codex_home);
        let view = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let expected = format!(
            "No Codex login found in {}. If Codex keeps its login in the system keyring, seatswap cannot read it.",
            codex_home.join("auth.json").display()
        );
        assert_eq!(view.error.as_deref(), Some(expected.as_str()));

        // A login in the system keyring.
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        std::fs::create_dir_all(&codex_home).expect("the Codex folder");
        std::fs::write(
            codex_home.join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":null}"#,
        )
        .expect("auth.json");
        let mut runtime = test_runtime(dir.path(), offline_api(), &codex_home);
        let view = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        assert_eq!(
            view.error.as_deref(),
            Some(
                "Codex keeps its login in the system keyring. To switch Codex logins, set \
                 cli_auth_credentials_store = \"file\" in Codex's config.toml and sign in again."
            )
        );

        // A 401.
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(401, String::new())]);
        let mut runtime = test_runtime(dir.path(), codex_api(&server.base), &codex_home);
        let view = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let _ = server.finish();
        assert_eq!(
            view.error.as_deref(),
            Some("Codex's login has expired. Run codex once to renew it.")
        );

        // A closed port for Ollama.
        let dir = tempdir().expect("tempdir");
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("a local address"));
        drop(listener);
        let mut runtime = test_runtime(dir.path(), offline_api(), &dir.path().join("codex"));
        let view = only_view(runtime.poll_round(&ollama_on(&url), utc("2026-11-14T09:00:00Z")));
        let expected = format!("Ollama is not running on {url}.");
        assert_eq!(view.error.as_deref(), Some(expected.as_str()));

        // A 500: the error's own text.
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(500, String::new())]);
        let mut runtime = test_runtime(dir.path(), codex_api(&server.base), &codex_home);
        let view = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let _ = server.finish();
        assert_eq!(view.error.as_deref(), Some("server error 500"));
    }

    #[test]
    fn a_card_switched_off_loses_its_view_at_once() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let api = codex_api(&server.base);
        let mut runtime = test_runtime(dir.path(), api, &codex_home);

        let on = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        assert_eq!(on.provider, "codex");
        let views = runtime.settings_saved(&settings(), utc("2026-11-14T09:01:00Z"));

        assert!(views.is_empty(), "the switched-off card lost its view");
        let paths = server.finish();
        assert_eq!(paths.len(), 1, "the switched-off card sent a request");
    }

    /// A save that changes Codex's folder drops the old reading before the new
    /// folder is polled: a failure there shows its error without the old
    /// account's numbers.
    #[test]
    fn a_changed_codex_folder_drops_the_old_numbers() {
        let dir = tempdir().expect("tempdir");
        let folder_a = dir.path().join("codex-a");
        write_auth(&folder_a);
        let folder_b = dir.path().join("codex-b");
        std::fs::create_dir_all(&folder_b).expect("the new Codex folder");
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let mut runtime = test_runtime_with(
            dir.path(),
            codex_api(&server.base),
            explicit_codex_paths(),
            same_settings(),
            Box::new(|_| {}),
        );
        let on_a = Settings {
            codex_card: true,
            codex_home: Some(folder_a),
            ..Settings::default()
        };
        let on_b = Settings {
            codex_card: true,
            codex_home: Some(folder_b.clone()),
            ..Settings::default()
        };

        let first = only_view(runtime.poll_round(&on_a, utc("2026-11-14T09:00:00Z")));
        assert!(first.usage.is_some(), "folder A gave a reading");
        let second = only_view(runtime.settings_saved(&on_b, utc("2026-11-14T09:01:00Z")));
        let _ = server.finish();

        let expected = format!(
            "No Codex login found in {}. If Codex keeps its login in the system keyring, seatswap cannot read it.",
            folder_b.join("auth.json").display()
        );
        assert_eq!(second.error.as_deref(), Some(expected.as_str()));
        assert_eq!(second.usage, None, "the old account's usage stays");
        assert_eq!(second.plan, None, "the old account's plan stays");
        assert_eq!(second.fetched_at, None, "the old reading's time stays");
    }

    #[test]
    fn opening_the_popup_polls_only_after_a_minute() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let api = codex_api(&server.base);
        let mut runtime = test_runtime(dir.path(), api, &codex_home);
        let start = utc("2026-11-14T09:00:00Z");

        runtime.poll_round(&codex_on(), start);
        runtime.opened(&codex_on(), start + chrono::Duration::seconds(30));
        runtime.opened(&codex_on(), start + chrono::Duration::seconds(61));
        let paths = server.finish();

        assert_eq!(
            paths.len(),
            2,
            "the start and the +61 s opening asked; the +30 s one did not: {paths:?}"
        );
    }

    /// A provider whose subscription has a price is polled with its card off:
    /// the price turns its work into money even when the card is hidden. The
    /// reading is recorded, the subscription kept, and no card is shown.
    #[test]
    fn a_provider_is_polled_while_its_price_is_set_with_the_card_off() {
        let dir = tempdir().expect("tempdir");
        let app = test_app(dir.path());
        let store = test_store(dir.path());
        let account = AccountRef {
            acc: providers::OLLAMA_ORG.to_string(),
            org: providers::OLLAMA_ORG.to_string(),
        };
        let mut file = store.load().expect("accounts.json");
        file.ensure_provider_subscription(&account, None)
            .expect("the Ollama subscription");
        let subscription = file
            .provider_subscriptions
            .iter_mut()
            .find(|subscription| subscription.account == account)
            .expect("the Ollama subscription");
        subscription.price = Some(23.0);
        subscription.price_currency = Some("EUR".to_string());
        store.save(&file).expect("accounts.json");

        let server = Fake::start(vec![
            (200, BALANCE.to_string()),
            (200, USAGE_ANSWER.to_string()),
        ]);
        let mut runtime = test_runtime_with_app(
            dir.path(),
            codex_api(&server.base),
            Box::new(|_| Ok(CodexPaths::at(Path::new("unused-codex-home")))),
            same_settings(),
            Box::new(|_| {}),
            app,
        );
        // `ollama_on` is not used: the card stays off.
        let settings = Settings {
            ollama_url: server.base.clone(),
            ..Settings::default()
        };

        let views = runtime.poll_round(&settings, utc("2026-11-14T09:00:00Z"));
        let paths = server.finish();

        assert_eq!(
            paths,
            vec![
                "/api/balance".to_string(),
                "/api/usage?range=24h".to_string()
            ],
            "the priced subscription was not polled with the card off"
        );
        assert!(
            views.is_empty(),
            "a card that is off showed a view: {views:?}"
        );
        let file = test_store(dir.path()).load().expect("accounts.json");
        let subscription = file
            .provider_subscription(&account)
            .expect("the reading kept the subscription");
        assert_eq!(subscription.price, Some(23.0));
        assert_eq!(subscription.price_currency.as_deref(), Some("EUR"));
        let read = history(dir.path())
            .read::<Sample>(&Range::ALL, &Hosts::All)
            .expect("the samples");
        assert_eq!(read.records.len(), 1, "the reading was recorded");
        assert_eq!(read.records[0].record.account.org, "ollama");
    }

    /// A failed poll of a priced subscription whose card is off has no card to
    /// show the error on; it is reported like every other provider error.
    #[test]
    fn a_failed_poll_of_a_priced_card_that_is_off_shows_no_card() {
        let dir = tempdir().expect("tempdir");
        let app = test_app(dir.path());
        let store = test_store(dir.path());
        let account = AccountRef {
            acc: providers::OLLAMA_ORG.to_string(),
            org: providers::OLLAMA_ORG.to_string(),
        };
        let mut file = store.load().expect("accounts.json");
        file.ensure_provider_subscription(&account, None)
            .expect("the Ollama subscription");
        let subscription = file
            .provider_subscriptions
            .iter_mut()
            .find(|subscription| subscription.account == account)
            .expect("the Ollama subscription");
        subscription.price = Some(23.0);
        subscription.price_currency = Some("EUR".to_string());
        store.save(&file).expect("accounts.json");

        let server = Fake::start(vec![(500, String::new())]);
        let (sender, reports) = mpsc::channel();
        let errors = ReportedErrors::new(Box::new(move |text: &str| {
            let _ = sender.send(text.to_string());
        }));
        let report: ReportError = Box::new(move |text: &str| errors.report(text));
        let mut runtime = test_runtime_with_app(
            dir.path(),
            codex_api(&server.base),
            Box::new(|_| Ok(CodexPaths::at(Path::new("unused-codex-home")))),
            same_settings(),
            report,
            app,
        );
        // The card stays off; only the price makes the provider polled.
        let settings = Settings {
            ollama_url: server.base.clone(),
            ..Settings::default()
        };

        let views = runtime.poll_round(&settings, utc("2026-11-14T09:00:00Z"));
        let _ = server.finish();

        assert!(
            views.is_empty(),
            "a card that is off showed an error view: {views:?}"
        );
        let texts: Vec<String> = reports.try_iter().collect();
        assert_eq!(
            texts.len(),
            1,
            "the failure was not reported once: {texts:?}"
        );
        assert!(
            texts[0].contains("server error 500"),
            "the reported text is not the provider's: {}",
            texts[0]
        );
    }

    #[test]
    fn a_reading_without_usage_records_nothing() {
        let dir = tempdir().expect("tempdir");
        let server = Fake::start(vec![
            (200, BALANCE_ZERO.to_string()),
            (200, USAGE_ANSWER.to_string()),
        ]);
        let mut runtime = test_runtime(
            dir.path(),
            codex_api(&server.base),
            &dir.path().join("codex"),
        );

        let view =
            only_view(runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z")));
        let paths = server.finish();

        assert_eq!(view.provider, "ollama");
        assert_eq!(view.usage, None);
        assert_eq!(view.spend_24h, Some(3.18));
        assert_eq!(view.spend_24h_error, None);
        assert_eq!(
            view.credits,
            Some(Credits {
                left: 0.0,
                allowance: Some(0.0),
                until: None,
                purchased: None,
            })
        );
        assert_eq!(
            paths,
            vec![
                "/api/balance".to_string(),
                "/api/usage?range=24h".to_string()
            ]
        );
        let read = history(dir.path())
            .read::<Sample>(&Range::ALL, &Hosts::All)
            .expect("the samples");
        assert!(
            read.records.is_empty(),
            "a reading without a usage wrote a sample line"
        );
    }

    /// A 24 h spend that could not be read is its own view field; the balance
    /// reading itself stays and the card's error stays empty.
    #[test]
    fn a_spend_that_could_not_be_read_has_its_own_view_field() {
        let dir = tempdir().expect("tempdir");
        let server = Fake::start(vec![(200, BALANCE.to_string()), (500, String::new())]);
        let mut runtime = test_runtime(
            dir.path(),
            codex_api(&server.base),
            &dir.path().join("codex"),
        );

        let view =
            only_view(runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z")));
        let paths = server.finish();

        assert_eq!(view.error, None, "the reading itself is Ok");
        assert_eq!(view.spend_24h, None);
        assert_eq!(view.spend_24h_error.as_deref(), Some("server error 500"));
        assert!(view.credits.is_some(), "the balance is still the card's");
        assert_eq!(
            paths,
            vec![
                "/api/balance".to_string(),
                "/api/usage?range=24h".to_string()
            ]
        );
    }

    /// A reading's hourly buckets go into the history once; the same 24 h
    /// answer polled again appends nothing.
    #[test]
    fn a_reading_writes_the_ollama_hours_once() {
        let dir = tempdir().expect("tempdir");
        let server = Fake::start(vec![
            (200, BALANCE.to_string()),
            (200, USAGE_WITH_HOURS.to_string()),
            (200, BALANCE.to_string()),
            (200, USAGE_WITH_HOURS.to_string()),
        ]);
        let mut runtime = test_runtime(
            dir.path(),
            codex_api(&server.base),
            &dir.path().join("codex"),
        );

        let view =
            only_view(runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z")));
        assert_eq!(view.spend_24h, Some(0.12));
        runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:05:00Z"));
        let _ = server.finish();

        let read = history(dir.path())
            .read::<OllamaHourRecord>(&Range::ALL, &Hosts::All)
            .expect("the Ollama hours");
        assert_eq!(
            read.records.len(),
            1,
            "the same bucket was written a second time"
        );
        assert_eq!(
            read.records[0].record,
            OllamaHourRecord {
                hour: utc("2026-11-14T08:00:00Z"),
                requests: 17,
                input: 210000,
                cached_input: 160000,
                output: 26000,
                usd: 0.12,
            }
        );
    }

    /// A bucket that cannot be read is reported, not dropped, and the card
    /// itself stays a successful reading with its balance.
    #[test]
    fn a_bucket_error_is_reported_without_failing_the_card() {
        let dir = tempdir().expect("tempdir");
        let broken = r#"{"totals":{"usage_usd":0.12},"buckets":[{"from":"2026-11-14T08:00:00Z","until":"2026-11-14T10:00:00Z","request_count":17,"usage_usd":0.12,"input_tokens":210000,"cached_input_tokens":160000,"output_tokens":26000}]}"#;
        let server = Fake::start(vec![(200, BALANCE.to_string()), (200, broken.to_string())]);
        let (sender, reports) = mpsc::channel();
        let errors = ReportedErrors::new(Box::new(move |text: &str| {
            let _ = sender.send(text.to_string());
        }));
        let report: ReportError = Box::new(move |text: &str| errors.report(text));
        let mut runtime = test_runtime_with(
            dir.path(),
            codex_api(&server.base),
            Box::new(|_| Ok(CodexPaths::at(Path::new("unused-codex-home")))),
            same_settings(),
            report,
        );

        let view =
            only_view(runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z")));
        let _ = server.finish();

        assert_eq!(view.error, None, "the reading itself is Ok");
        assert!(view.credits.is_some(), "the balance is still the card's");
        let texts: Vec<String> = reports.try_iter().collect();
        assert!(
            texts.iter().any(|text| text.contains("buckets[0]")),
            "the bucket error was not reported: {texts:?}"
        );
    }

    /// PROOF (review O-1, fix 1): the recorder's warnings (an older Ollama answer
    /// named once per hour) are never taken by this runtime, so they never reach
    /// the app's printed warnings and the list stays in the recorder.
    #[test]
    fn proof_an_older_hour_warning_is_not_left_in_the_recorder() {
        let dir = tempdir().expect("tempdir");
        let smaller = USAGE_WITH_HOURS.replace("\"request_count\":17,\"usage_usd\":0.12,\"input_tokens\":210000,\"cached_input_tokens\":160000,\"output_tokens\":26000}]", "\"request_count\":3,\"usage_usd\":0.02,\"input_tokens\":1000,\"cached_input_tokens\":100,\"output_tokens\":200}]");
        assert_ne!(smaller, USAGE_WITH_HOURS);
        let server = Fake::start(vec![
            (200, BALANCE.to_string()),
            (200, USAGE_WITH_HOURS.to_string()),
            (200, BALANCE.to_string()),
            (200, smaller),
        ]);
        let mut runtime = test_runtime(
            dir.path(),
            codex_api(&server.base),
            &dir.path().join("codex"),
        );
        runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z"));
        runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:05:00Z"));
        let _ = server.finish();
        let left = runtime
            .recorder
            .as_mut()
            .expect("a recorder")
            .take_warnings();
        assert!(left.is_empty(), "warnings nobody printed: {left:?}");
        assert_eq!(
            super::super::lock(&runtime.printed_warnings).len(),
            1,
            "the older hour's warning was not printed once"
        );
    }

    /// PROOF (review O-1): a failed 24 h usage call raised no notification
    /// before O-1 (the card showed `spend_24h_error`); it must not now.
    #[test]
    fn proof_a_failed_usage_call_raises_no_notification() {
        let dir = tempdir().expect("tempdir");
        let server = Fake::start(vec![(200, BALANCE.to_string()), (500, String::new())]);
        let (sender, reports) = mpsc::channel();
        let errors = ReportedErrors::new(Box::new(move |text: &str| {
            let _ = sender.send(text.to_string());
        }));
        let report: ReportError = Box::new(move |text: &str| errors.report(text));
        let mut runtime = test_runtime_with(
            dir.path(),
            codex_api(&server.base),
            Box::new(|_| Ok(CodexPaths::at(Path::new("unused-codex-home")))),
            same_settings(),
            report,
        );
        let _ =
            only_view(runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z")));
        let _ = server.finish();
        let texts: Vec<String> = reports.try_iter().collect();
        assert!(texts.is_empty(), "a notification was raised: {texts:?}");
    }

    /// A history folder that cannot be written is reported once for the same
    /// text, however many readings fail the same way.
    #[test]
    fn a_history_error_is_reported_once_for_the_same_text() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        // The history root is a file, so no line can be written.
        let root = dir.path().join("history");
        std::fs::write(&root, b"not a history folder").expect("the root file");
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let (sender, reports) = mpsc::channel();
        let errors = ReportedErrors::new(Box::new(move |text: &str| {
            let _ = sender.send(text.to_string());
        }));
        let report: ReportError = Box::new(move |text: &str| errors.report(text));
        let app = test_app(dir.path());
        let mut runtime = ProvidersRuntime::new(
            Ok(HistoryDir::at(&root, "atlas")),
            codex_api(&server.base),
            None,
            app.clone(),
            test_store_opener(app),
            Box::new(move |_| Ok(CodexPaths::at(&codex_home))),
            same_settings(),
            report,
            Arc::new(Mutex::new(PrintedWarnings::new())),
        );

        runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z"));
        runtime.poll_round(&codex_on(), utc("2026-11-14T09:05:00Z"));
        let _ = server.finish();

        let texts: Vec<String> = reports.try_iter().collect();
        assert_eq!(
            texts.len(),
            1,
            "the same history error was reported again: {texts:?}"
        );
    }

    // ---- the second review round ----

    /// A save is queued behind the running request: the settings read after the
    /// request says the card is off, so that request leaves nothing behind.
    #[test]
    fn a_card_switched_off_during_its_request_leaves_no_trace() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let off = settings();
        let lookup: SettingsLookup = Box::new(move |_| Ok(off.clone()));
        let folder = codex_home.clone();
        let mut runtime = test_runtime_with(
            dir.path(),
            codex_api(&server.base),
            Box::new(move |_| Ok(CodexPaths::at(&folder))),
            lookup,
            Box::new(|_| {}),
        );

        let views = runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z"));
        let paths = server.finish();

        assert!(
            views.is_empty(),
            "the dropped reading made a card: {views:?}"
        );
        assert_eq!(
            paths,
            vec!["/backend-api/wham/usage".to_string()],
            "the request ran before the switch"
        );
        let read = history(dir.path())
            .read::<Sample>(&Range::ALL, &Hosts::All)
            .expect("the samples");
        assert!(
            read.records.is_empty(),
            "the dropped reading wrote a sample line"
        );
    }

    /// A URL changed while its request runs drops the answer the same way.
    #[test]
    fn a_url_changed_during_its_request_leaves_no_trace() {
        let dir = tempdir().expect("tempdir");
        let server = Fake::start(vec![
            (200, BALANCE.to_string()),
            (200, USAGE_ANSWER.to_string()),
        ]);
        let elsewhere = ollama_on("http://127.0.0.1:1");
        let lookup: SettingsLookup = Box::new(move |_| Ok(elsewhere.clone()));
        let mut runtime = test_runtime_with(
            dir.path(),
            codex_api(&server.base),
            Box::new(|_| Ok(CodexPaths::at(Path::new("unused-codex-home")))),
            lookup,
            Box::new(|_| {}),
        );

        let views = runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z"));
        let paths = server.finish();

        assert!(
            views.is_empty(),
            "the dropped reading made a card: {views:?}"
        );
        assert_eq!(
            paths,
            vec![
                "/api/balance".to_string(),
                "/api/usage?range=24h".to_string()
            ],
            "the request ran before the URL changed"
        );
        let read = history(dir.path())
            .read::<Sample>(&Range::ALL, &Hosts::All)
            .expect("the samples");
        assert!(
            read.records.is_empty(),
            "the dropped reading wrote a sample line"
        );
    }

    /// A settings read that fails after the request reports once and records
    /// nothing on a guess.
    #[test]
    fn a_settings_read_that_fails_drops_the_reading_and_reports_it() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let (sender, reports) = mpsc::channel();
        let errors = ReportedErrors::new(Box::new(move |text: &str| {
            let _ = sender.send(text.to_string());
        }));
        let report: ReportError = Box::new(move |text: &str| errors.report(text));
        let lookup: SettingsLookup = Box::new(|_| Err("accounts.json is unreadable".to_string()));
        let folder = codex_home.clone();
        let mut runtime = test_runtime_with(
            dir.path(),
            codex_api(&server.base),
            Box::new(move |_| Ok(CodexPaths::at(&folder))),
            lookup,
            report,
        );

        let first = runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z"));
        let second = runtime.poll_round(&codex_on(), utc("2026-11-14T09:05:00Z"));
        let _ = server.finish();

        assert!(
            first.is_empty(),
            "the dropped reading made a card: {first:?}"
        );
        assert!(
            second.is_empty(),
            "the dropped reading made a card: {second:?}"
        );
        let texts: Vec<String> = reports.try_iter().collect();
        assert_eq!(
            texts,
            vec!["accounts.json is unreadable".to_string()],
            "the same settings error was not reported once: {texts:?}"
        );
        let read = history(dir.path())
            .read::<Sample>(&Range::ALL, &Hosts::All)
            .expect("the samples");
        assert!(
            read.records.is_empty(),
            "the dropped reading wrote a sample line"
        );
    }

    /// A URL changed by hand in `accounts.json` drops the numbers before the
    /// next round polls: the failing new server's error comes with no credits.
    #[test]
    fn a_url_changed_by_hand_drops_the_old_numbers_before_polling() {
        let dir = tempdir().expect("tempdir");
        let server = Fake::start(vec![
            (200, BALANCE.to_string()),
            (200, USAGE_ANSWER.to_string()),
        ]);
        let mut runtime = test_runtime(
            dir.path(),
            codex_api(&server.base),
            &dir.path().join("codex"),
        );

        let first =
            only_view(runtime.poll_round(&ollama_on(&server.base), utc("2026-11-14T09:00:00Z")));
        assert!(first.credits.is_some(), "the first server gave credits");

        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("a local address"));
        drop(listener);
        let second = only_view(runtime.poll_round(&ollama_on(&url), utc("2026-11-14T09:10:00Z")));
        let _ = server.finish();

        let expected = format!("Ollama is not running on {url}.");
        assert_eq!(second.error.as_deref(), Some(expected.as_str()));
        assert_eq!(second.credits, None, "the old server's credits stay");
        assert_eq!(second.usage, None, "the old server's usage stays");
        assert_eq!(second.fetched_at, None, "the old reading's time stays");
    }

    /// The history error at startup is already in the snapshot and was notified
    /// there; the provider thread must stay silent for it.
    #[test]
    fn a_startup_history_error_is_not_reported_again() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let (sender, reports) = mpsc::channel();
        let report: ReportError = Box::new(move |text: &str| {
            let _ = sender.send(text.to_string());
        });
        let folder = codex_home.clone();
        let app = test_app(dir.path());
        let mut runtime = ProvidersRuntime::new(
            Err("the history folder could not be opened".to_string()),
            codex_api(&server.base),
            None,
            app.clone(),
            test_store_opener(app),
            Box::new(move |_| Ok(CodexPaths::at(&folder))),
            same_settings(),
            report,
            Arc::new(Mutex::new(PrintedWarnings::new())),
        );

        let first = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:00:00Z")));
        let second = only_view(runtime.poll_round(&codex_on(), utc("2026-11-14T09:05:00Z")));
        let _ = server.finish();

        let texts: Vec<String> = reports.try_iter().collect();
        assert!(
            texts.is_empty(),
            "the startup history error was reported again: {texts:?}"
        );
        assert!(first.usage.is_some(), "the first view lost its numbers");
        assert!(second.usage.is_some(), "the view lost its numbers");
    }

    // ---- proof tests of the review ----

    /// The plan: the pop-up polls a card "if the last reading is older than
    /// 60 s". A reading that is exactly 60 s old is not older than 60 s, so
    /// the pop-up must not ask again.
    #[test]
    fn opening_the_popup_at_exactly_a_minute_does_not_poll() {
        let dir = tempdir().expect("tempdir");
        let codex_home = dir.path().join("codex");
        write_auth(&codex_home);
        let server = Fake::start(vec![(200, PLUS_ANSWER.to_string())]);
        let api = codex_api(&server.base);
        let mut runtime = test_runtime(dir.path(), api, &codex_home);
        let start = utc("2026-11-14T09:00:00Z");

        runtime.poll_round(&codex_on(), start);
        runtime.opened(&codex_on(), start + chrono::Duration::seconds(60));
        let paths = server.finish();

        assert_eq!(
            paths.len(),
            1,
            "the pop-up asked again after exactly a minute: {paths:?}"
        );
    }
}
