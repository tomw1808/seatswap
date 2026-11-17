//! The provider thread (step-d.md Task 4): Codex's and Ollama's read-only cards,
//! polled every five minutes while their switch is on.
//!
//! The thread reads `accounts.json` at every round, so a switch, a Codex folder
//! or an Ollama URL counts without a restart. A card that is off is never read
//! and never asked. Every change of the list is one [`UiEvent::Providers`] for
//! the main thread, Codex first; the list holds only the cards that are on.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

use seatswap_core::api::{Api, Usage};
use seatswap_core::history::{EventKind, HistoryDir, Recorder, Source};
use seatswap_core::paths::{AppPaths, CodexPaths, PathsError};
use seatswap_core::providers::{self, Credits, ProviderReading, CODEX_ORG, OLLAMA_ORG};
use seatswap_core::store::{self, ProviderSubscription, Settings};

use seatswap_core::switcher;

use crate::ops;

use super::worker::{PrintedWarnings, UiEvent};
use super::ReportedErrors;

/// The provider cards are polled every five minutes (step-d.md Task 4).
pub(crate) const PROVIDER_POLL_SECS: u64 = 300;

/// An open pop-up polls a provider only when its last attempt is older than
/// this.
const PROVIDER_OPEN_SECS: i64 = 60;

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

    fn view<'a>(
        self,
        codex: &'a Option<ProviderView>,
        ollama: &'a Option<ProviderView>,
    ) -> Option<&'a ProviderView> {
        match self {
            Provider::Codex => codex.as_ref(),
            Provider::Ollama => ollama.as_ref(),
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

/// The commands the window and the settings save send to the provider thread.
enum ProvidersCommand {
    /// The pop-up opened.
    Opened,
    /// A settings save wrote `accounts.json`.
    SettingsSaved,
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
    let mut runtime = ProvidersRuntime::new(
        history,
        api,
        Some(ui),
        app.clone(),
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
    /// `accounts.json` there. The provider thread never needs a secret.
    app: AppPaths,
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
    codex_tried: Option<DateTime<Utc>>,
    ollama_tried: Option<DateTime<Utc>>,
    /// The list the UI received last; a send happens only when it changes.
    published: Vec<ProviderView>,
}

impl ProvidersRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        history: Result<HistoryDir, String>,
        api: Api,
        ui: Option<Sender<UiEvent>>,
        app: AppPaths,
        codex_paths: CodexPathsLookup,
        settings_lookup: SettingsLookup,
        report: ReportError,
        printed_warnings: Arc<Mutex<PrintedWarnings>>,
    ) -> ProvidersRuntime {
        ProvidersRuntime {
            api,
            ui,
            app,
            codex_paths,
            settings_lookup,
            recorder: history.map(Recorder::new),
            report,
            printed_warnings,
            settings: None,
            codex: None,
            ollama: None,
            codex_tried: None,
            ollama_tried: None,
            published: Vec::new(),
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

    /// Drop the view of a card that is now off, and of one whose folder or URL
    /// changed since `previous`: the old numbers belong to another account or
    /// server. Returns the cards whose folder or URL changed.
    fn settle(&mut self, settings: &Settings, previous: Option<&Settings>) -> Vec<Provider> {
        let mut changed = Vec::new();
        for provider in [Provider::Codex, Provider::Ollama] {
            if !provider.is_on(settings) {
                *provider.view_mut(&mut self.codex, &mut self.ollama) = None;
                *provider.tried_mut(&mut self.codex_tried, &mut self.ollama_tried) = None;
            } else if previous.is_some_and(|previous| provider.setting_changed(previous, settings))
            {
                *provider.view_mut(&mut self.codex, &mut self.ollama) = None;
                changed.push(provider);
            }
        }
        changed
    }

    fn poll(&mut self, due: &[Provider], settings: &Settings, now: DateTime<Utc>) {
        for provider in due {
            let result = match provider {
                Provider::Codex => self.read_codex(settings),
                Provider::Ollama => providers::ollama::read(&settings.ollama_url, &self.api),
            };
            match provider {
                Provider::Codex => self.codex_tried = Some(now),
                Provider::Ollama => self.ollama_tried = Some(now),
            }
            // A save that switches this card off or moves it is a queued
            // command, so the request has ended before that save ran. Read the
            // settings again: an answer for a card that is now off and unpriced,
            // or that sits elsewhere, leaves no trace. A failed read is reported
            // and drops the answer too; nothing is recorded on a guess. A card
            // that is off but whose subscription has a price is still recorded;
            // it has no view (spec §7.9).
            let current = match (self.settings_lookup)(settings) {
                Ok(current) => current,
                Err(text) => {
                    self.report_error(&text);
                    continue;
                }
            };
            let on = provider.is_on(&current);
            let keep =
                (on || self.priced(*provider)) && !provider.setting_changed(settings, &current);
            if !keep {
                continue;
            }
            let view = match result {
                Ok(reading) => {
                    self.record(&reading, now);
                    self.ensure_subscription(*provider, &reading, now);
                    if on {
                        Some(success(*provider, reading, now))
                    } else {
                        None
                    }
                }
                Err(error) => {
                    let text = error.to_string();
                    if on {
                        Some(failure(*provider, self.view(*provider), text))
                    } else {
                        // The card is off, so there is no view to show the
                        // error; report it the way every provider error is.
                        self.report_error(&text);
                        None
                    }
                }
            };
            *provider.view_mut(&mut self.codex, &mut self.ollama) = view;
        }
    }

    /// The subscription of `provider`, or `None` after reporting a file that
    /// cannot be read. A read failure is never a silent "no price".
    fn subscription(&mut self, provider: Provider) -> Option<ProviderSubscription> {
        match store::load_accounts(&self.app) {
            Ok(file) => file
                .provider_subscriptions
                .into_iter()
                .find(|subscription| subscription.account.org == provider.name()),
            Err(error) => {
                self.report_error(&error.to_string());
                None
            }
        }
    }

    /// Whether this provider is polled for its price with the card off.
    fn priced(&mut self, provider: Provider) -> bool {
        self.subscription(provider)
            .is_some_and(|subscription| subscription.price.is_some())
    }

    /// Make sure a successful reading's account has a subscription, and write
    /// the events that reports: `accountAdded` when it was created, and
    /// `planChanged` when the provider named another plan. A failure here is
    /// reported like any provider error and never stops the poll.
    fn ensure_subscription(
        &mut self,
        provider: Provider,
        reading: &ProviderReading,
        now: DateTime<Utc>,
    ) {
        match switcher::ensure_provider_subscription(
            &self.app,
            &reading.account,
            reading.plan.as_deref(),
        ) {
            Ok(ensured) => {
                let mut failures = Vec::new();
                if let Ok(recorder) = &mut self.recorder {
                    if ensured.created {
                        if let Err(error) = recorder.event(
                            now,
                            EventKind::AccountAdded {
                                account: reading.account.clone(),
                                plan: reading.plan.clone(),
                            },
                        ) {
                            failures.push(ops::history_unwritable(&error));
                        }
                    }
                    if let Some(change) = &ensured.plan_change {
                        if let Err(error) = ops::record_plan_change(recorder, change, now) {
                            failures.push(ops::history_unwritable(&error));
                        }
                    }
                }
                for text in &failures {
                    self.report_error(text);
                }
                self.print_history_warnings();
            }
            Err(error) => self.report_error(&format!(
                "the {} subscription is not saved: {error}",
                provider.title()
            )),
        }
    }

    fn read_codex(&self, settings: &Settings) -> Result<ProviderReading, providers::ProviderError> {
        let paths = (self.codex_paths)(settings.codex_home.as_deref())
            .map_err(|error| providers::ProviderError::Setting(error.to_string()))?;
        providers::codex::read(&paths, &self.api)
    }

    fn view(&self, provider: Provider) -> Option<&ProviderView> {
        provider.view(&self.codex, &self.ollama)
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
        let list = self.views();
        if list == self.published {
            return;
        }
        self.published = list.clone();
        if let Some(ui) = &self.ui {
            let _ = ui.send(UiEvent::Providers(list));
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

    const AUTH: &str = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","last_refresh":"2026-11-14T16:53:03.750022420Z","tokens":{"id_token":"eyJhbGciOi.id.sig","access_token":"eyJhbGciOi.access.sig","refresh_token":"rt_9f2c","account_id":"0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13","login_owner_id":"user-sam","oauth_provenance":"cli"}}"#;

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
        ProvidersRuntime::new(
            Ok(history(dir)),
            api,
            None,
            app,
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
                            let path = read_request(&stream);
                            let _ = sender.send(path);
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
                stop,
                thread: Some(thread),
            }
        }

        fn finish(mut self) -> Vec<String> {
            thread::sleep(Duration::from_millis(50));
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("the server thread");
            }
            self.paths.try_iter().collect()
        }
    }

    fn read_request(stream: &std::net::TcpStream) -> String {
        let mut reader = BufReader::new(stream.try_clone().expect("a stream clone"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("the request line");
        let path = line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).expect("a header") == 0 {
                break;
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
        }
        path
    }

    fn write_reply(stream: &mut std::net::TcpStream, status: u16, body: &str) {
        let text = format!(
            "HTTP/1.1 {status} seatswap-test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(text.as_bytes()).expect("a reply");
    }

    fn only_view(views: Vec<ProviderView>) -> ProviderView {
        assert_eq!(views.len(), 1, "one switched-on card is one view");
        views.into_iter().next().expect("one view")
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
                "Codex keeps its login in the system keyring. seatswap reads the login file only."
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
        let mut runtime = ProvidersRuntime::new(
            Ok(HistoryDir::at(&root, "atlas")),
            codex_api(&server.base),
            None,
            test_app(dir.path()),
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
        let mut runtime = ProvidersRuntime::new(
            Err("the history folder could not be opened".to_string()),
            codex_api(&server.base),
            None,
            test_app(dir.path()),
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
