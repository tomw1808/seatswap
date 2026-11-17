//! The look-up thread (spec §1.4.1): the API prices from models.dev every hour
//! and the dollar rate from Frankfurter on its own schedule, both behind
//! `lookUpPrices`. The rate is looked at every 30 minutes, so the 16:00–18:00
//! retry runs and a failed ask is asked again; only an answer marks the day.
//!
//! The thread is the only writer of `prices.json` in the app; the commands read
//! its [`MoneyView`] and send it over a channel. Every change of the view is a
//! [`UiEvent::Money`] for the window.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::Serialize;

use seatswap_core::api::Api;
use seatswap_core::money::{self, Fx, ModelPrice, MoneyError, PriceChange, Prices, Rate};
use seatswap_core::paths::AppPaths;
use seatswap_core::store::{self, Settings};

use super::worker::UiEvent;

/// models.dev is checked every hour (spec §1.4.1).
const PRICE_EVERY: Duration = Duration::from_secs(60 * 60);

/// How often the thread looks at the rate: the 16:00–18:00 retry asks every 30
/// minutes (spec §1.4), and a failed ask is asked again at the next look.
const RATE_EVERY: Duration = Duration::from_secs(30 * 60);

/// The sources the settings page names beside the rate and the price list
/// (spec §1.4, §1.4.1).
const RATE_SOURCE: &str = "European Central Bank via Frankfurter";
const PRICES_SOURCE: &str = "models.dev";

/// What the settings page shows about money. A missing value stays `None`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoneyView {
    pub currency: Option<String>,
    pub rate: Option<Rate>,
    /// When the looked-up point in effect was fetched; `None` for a point of
    /// an old `fx.json` (the page says "fetched: not recorded").
    pub rate_fetched_at: Option<DateTime<Utc>>,
    pub rate_source: &'static str,
    pub prices_source: &'static str,
    pub look_up: bool,
    pub models_with_price: u32,
    pub prices_checked_at: Option<DateTime<Utc>>,
    pub last_change: Option<PriceChange>,
    /// The models of the last look-up's list that were left out, each
    /// `"<key>: <reason>"`; kept until a later list names others.
    pub left_out: Vec<String>,
    /// The rate look-up's failure, for the rate row (spec §1.4). The price
    /// look-up's errors and the file errors stay in `last_error`.
    pub rate_error: Option<String>,
    pub last_error: Option<String>,
}

impl Default for MoneyView {
    fn default() -> Self {
        MoneyView {
            currency: None,
            rate: None,
            rate_fetched_at: None,
            rate_source: RATE_SOURCE,
            prices_source: PRICES_SOURCE,
            look_up: false,
            models_with_price: 0,
            prices_checked_at: None,
            last_change: None,
            left_out: Vec::new(),
            rate_error: None,
            last_error: None,
        }
    }
}

/// One model's effective price as the price list shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PriceRow {
    pub key: String,
    pub price: ModelPrice,
    pub tiered: bool,
    pub since: Option<DateTime<Utc>>,
}

/// The commands the window sends to the look-up thread.
enum LookupCommand {
    Now {
        reply: Sender<Result<MoneyView, String>>,
    },
    /// A session met a model without a price (the sessions thread sends it).
    PriceWanted(String),
    /// The settings were saved or a shared setting arrived from another host:
    /// the currency may be new, so the rate part runs at once.
    SettingsSaved,
    Stop,
}

/// What the commands share with the thread.
#[derive(Clone)]
pub(crate) struct LookupClient {
    view: Arc<Mutex<MoneyView>>,
    commands: Sender<LookupCommand>,
}

/// The shared view's lock. A worker thread that panicked while holding it must
/// not turn the view into "nothing is stored": the app's own pattern is used.
fn lock_view(view: &Mutex<MoneyView>) -> MutexGuard<'_, MoneyView> {
    view.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl LookupClient {
    pub(crate) fn view(&self) -> Result<MoneyView, String> {
        Ok(lock_view(&self.view).clone())
    }

    /// One round now, over the same code the timer uses.
    pub(crate) fn now(&self) -> Result<MoneyView, String> {
        let (reply, received) = mpsc::channel();
        self.commands
            .send(LookupCommand::Now { reply })
            .map_err(|_| super::thread_stopped_message())?;
        received
            .recv()
            .map_err(|_| super::thread_stopped_message())?
    }

    /// A session met `model` without a price: ask models.dev at once. The
    /// thread answers at most once per model per hour; the sessions thread
    /// calls it for every listed model without a price.
    pub(crate) fn want_price(&self, model: &str) {
        let _ = self
            .commands
            .send(LookupCommand::PriceWanted(model.to_string()));
    }

    /// The settings changed (saved here or shared from another host): ask the
    /// rate at once when it is due, instead of at the next rate wake.
    pub(crate) fn settings_saved(&self) -> Result<(), String> {
        super::send_to_worker(self.commands.send(LookupCommand::SettingsSaved))
    }
}

/// The handle the tray keeps. Dropping it stops and joins the thread.
pub(crate) struct LookupHandle {
    client: LookupClient,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LookupHandle {
    pub(crate) fn client(&self) -> LookupClient {
        self.client.clone()
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.client.commands.send(LookupCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LookupHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start the look-up thread. It runs whatever `lookUpPrices` says: the setting
/// is read again every round, so a change counts without a restart.
pub(crate) fn spawn(app: AppPaths, api: Api, ui: Sender<UiEvent>) -> Result<LookupHandle, String> {
    // Readable before the thread's first instruction: a window that asks for
    // the view right after start already sees what is stored.
    let view = Arc::new(Mutex::new(stored_view(&app)));
    let (commands, received) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_view = Arc::clone(&view);
    let thread = thread::Builder::new()
        .name("seatswap-lookup".to_string())
        .spawn(move || run(app, api, ui, thread_view, thread_stop, received))
        .map_err(|error| format!("cannot start the look-up thread: {error}"))?;
    Ok(LookupHandle {
        client: LookupClient { view, commands },
        stop,
        thread: Some(thread),
    })
}

fn run(
    app: AppPaths,
    api: Api,
    ui: Sender<UiEvent>,
    view: Arc<Mutex<MoneyView>>,
    stop: Arc<AtomicBool>,
    commands: Receiver<LookupCommand>,
) {
    let mut runtime = LookupRuntime::new(app.clone(), api, Some(ui), view);
    // The first round is at the start, before the first hour has passed.
    let mut next_prices = Instant::now();
    let mut next_rate = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let wait = next_prices
            .min(next_rate)
            .saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(LookupCommand::Now { reply }) => {
                let view = one_round(&mut runtime, &app, Utc::now());
                let _ = reply.send(Ok(view));
                next_prices = Instant::now() + PRICE_EVERY;
                next_rate = Instant::now() + RATE_EVERY;
            }
            Ok(LookupCommand::PriceWanted(model)) => {
                runtime.price_wanted(&model, Utc::now());
            }
            Ok(LookupCommand::SettingsSaved) => {
                one_rate_round(&mut runtime, &app, Utc::now());
                next_rate = Instant::now() + RATE_EVERY;
            }
            Ok(LookupCommand::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let due = due_jobs(now, next_prices, next_rate);
                if due.prices {
                    // models.dev's round looks at the rate too, so the rate's
                    // own deadline moves with it.
                    one_round(&mut runtime, &app, Utc::now());
                    next_prices = now + PRICE_EVERY;
                    next_rate = now + RATE_EVERY;
                } else if due.rate {
                    // A wake for the rate runs only the rate part.
                    one_rate_round(&mut runtime, &app, Utc::now());
                    next_rate = now + RATE_EVERY;
                }
            }
        }
    }
}

/// Which jobs a timer wake runs: the one whose deadline passed. Both deadlines
/// are checked, so a rate wake sends models.dev no request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DueJobs {
    prices: bool,
    rate: bool,
}

fn due_jobs(now: Instant, next_prices: Instant, next_rate: Instant) -> DueJobs {
    DueJobs {
        prices: now >= next_prices,
        rate: now >= next_rate,
    }
}

fn one_round(runtime: &mut LookupRuntime, app: &AppPaths, now: DateTime<Utc>) -> MoneyView {
    match store::load_accounts(app) {
        Ok(file) => runtime.round(&file.settings, now),
        Err(error) => runtime.settings_failed(error.to_string()),
    }
}

/// [`one_round`]'s rate part alone: a wake for the rate asks Frankfurter when
/// the schedule says so and never models.dev.
fn one_rate_round(
    runtime: &mut LookupRuntime,
    app: &AppPaths,
    now: DateTime<Utc>,
) -> Option<MoneyView> {
    match store::load_accounts(app) {
        Ok(file) => runtime.rate_round(&file.settings, now),
        Err(error) => Some(runtime.settings_failed(error.to_string())),
    }
}

/// One round over the files and the two look-ups. Tests drive it directly with
/// settings and a fixed time, so no test waits for the thread's timer.
pub(crate) struct LookupRuntime {
    app: AppPaths,
    api: Api,
    ui: Option<Sender<UiEvent>>,
    view: Arc<Mutex<MoneyView>>,
    /// The last ask Frankfurter answered, with the currency it answered for:
    /// [`money::rate_due`] decides from it. A failed ask leaves it as it was.
    last_rate_answered: Option<(DateTime<Utc>, String)>,
    /// The last time each model's price was asked for, by the model name the
    /// session gives: at most once an hour per model.
    wanted: HashMap<String, DateTime<Utc>>,
    /// Why the last models.dev request failed; `None` after an answer. It stays
    /// in the view until the next models.dev request replaces it, so a rate
    /// wake in between does not hide it.
    prices_error: Option<String>,
}

impl LookupRuntime {
    pub(crate) fn new(
        app: AppPaths,
        api: Api,
        ui: Option<Sender<UiEvent>>,
        view: Arc<Mutex<MoneyView>>,
    ) -> LookupRuntime {
        // The window reads the view before the first round is done: fill it from
        // the stored files at once, without a request, so the page does not say
        // "0 models with a price" while the files hold some.
        *lock_view(&view) = stored_view(&app);
        LookupRuntime {
            app,
            api,
            ui,
            view,
            last_rate_answered: None,
            wanted: HashMap::new(),
            prices_error: None,
        }
    }

    pub(crate) fn view(&self) -> MoneyView {
        lock_view(&self.view).clone()
    }

    /// One round with settings that were read already. It sends models.dev a
    /// request only when `lookUpPrices` is on; with it off the view still shows
    /// what is stored.
    pub(crate) fn round(&mut self, settings: &Settings, now: DateTime<Utc>) -> MoneyView {
        let left_out = if settings.look_up_prices {
            self.ask_prices(now)
        } else {
            // No request: an old failure is no longer the page's business.
            self.prices_error = None;
            None
        };
        let rate_error = self.ask_rate(settings, now).err();
        let error = self.prices_error.clone();
        self.build_view(settings, error, rate_error, local_day(now), left_out)
    }

    /// A wake for the rate only: it asks Frankfurter when [`money::rate_due`]
    /// says so and sends models.dev no request. `None` when the schedule had
    /// nothing to do, so the view is not rebuilt for nothing.
    pub(crate) fn rate_round(
        &mut self,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Option<MoneyView> {
        let rate_error = match self.ask_rate(settings, now) {
            Ok(false) => return None,
            Ok(true) => None,
            Err(failure) => Some(failure),
        };
        let error = self.prices_error.clone();
        Some(self.build_view(settings, error, rate_error, local_day(now), None))
    }

    /// The rate part of a round: the one place the rate is asked for. `Ok(true)`
    /// when it asked, `Ok(false)` when nothing was due; the error text is what
    /// the rate row shows. Only an answer records the ask, so a failed request
    /// makes the next rate wake ask again (spec §1.4: due once per Frankfurt
    /// day as today). Until a first answer since the app started (for this
    /// currency) it is always due: every start, and every new currency, asks.
    fn ask_rate(&mut self, settings: &Settings, now: DateTime<Utc>) -> Result<bool, String> {
        if !settings.look_up_prices {
            return Ok(false);
        }
        let Some(currency) = settings.currency.clone() else {
            return Ok(false);
        };
        if currency == "USD" {
            return Ok(false);
        }
        let fx = Fx::load(&self.app).map_err(|failure| format!("the dollar rate: {failure}"))?;
        let stored = if fx.currency == currency {
            fx.points.last().map(|point| point.date)
        } else {
            None
        };
        // The last answered ask belongs to a currency (fix round, point 3):
        // for another currency it counts as no ask.
        let last_ask = self
            .last_rate_answered
            .as_ref()
            .filter(|(_, answered)| answered == &currency)
            .map(|(at, _)| *at);
        let due = last_ask.is_none()
            || money::rate_due(now, stored, last_ask)
                .map_err(|failure| format!("the dollar rate: {failure}"))?;
        if !due {
            return Ok(false);
        }
        match money::look_up_rate(&self.api, &self.app, &currency, now) {
            Ok(_) => {
                self.last_rate_answered = Some((now, currency));
                Ok(true)
            }
            Err(failure) => Err(format!("the dollar rate: {failure}")),
        }
    }

    /// accounts.json could not be read: remember that and send no request.
    pub(crate) fn settings_failed(&mut self, error: String) -> MoneyView {
        let mut view = self.view();
        view.look_up = false;
        view.last_error = Some(format!("cannot read accounts.json: {error}"));
        self.store(view)
    }

    /// A model without a price was met: ask models.dev at once, at most once
    /// per model per hour. The settings are read the way the round reads them,
    /// so a look-up switched off after the last round sends no request; the map
    /// is keyed by the model name the session gives. Returns whether it asked;
    /// the clock is passed in so tests never sleep.
    fn price_wanted(&mut self, model: &str, now: DateTime<Utc>) -> bool {
        let settings = match store::load_accounts(&self.app) {
            Ok(file) => file.settings,
            Err(error) => {
                self.settings_failed(error.to_string());
                return false;
            }
        };
        if !settings.look_up_prices {
            return false;
        }
        if self.wanted.get(model).is_some_and(|last| {
            now.signed_duration_since(*last).num_seconds() < PRICE_EVERY.as_secs() as i64
        }) {
            return false;
        }
        self.wanted.insert(model.to_string(), now);
        let left_out = self.ask_prices(now);
        let rate_error = self.view().rate_error;
        let error = self.prices_error.clone();
        self.build_view(&settings, error, rate_error, local_day(now), left_out);
        true
    }

    /// One models.dev request: the one place the prices are asked for. Returns
    /// the left-out models of a new list; records why it failed, or clears that
    /// after an answer.
    fn ask_prices(&mut self, now: DateTime<Utc>) -> Option<Vec<String>> {
        match money::look_up_prices(&self.api, &self.app, now) {
            Ok(money::Looked::Taken { left_out, .. }) => {
                self.prices_error = None;
                Some(left_out)
            }
            Ok(money::Looked::NotModified) => {
                self.prices_error = None;
                None
            }
            Err(failure) => {
                self.prices_error = Some(format!("API prices: {failure}"));
                None
            }
        }
    }

    fn build_view(
        &mut self,
        settings: &Settings,
        error: Option<String>,
        rate_error: Option<String>,
        today: NaiveDate,
        left_out: Option<Vec<String>>,
    ) -> MoneyView {
        let mut error = error;
        let prices = match Prices::load(&self.app) {
            Ok(prices) => prices,
            Err(failure) => {
                error = combine(error, format!("prices.json: {failure}"));
                Prices::default()
            }
        };
        let fx = match Fx::load(&self.app) {
            Ok(fx) => fx,
            Err(failure) => {
                error = combine(error, format!("fx.json: {failure}"));
                Fx::default()
            }
        };
        let currency = settings.currency.clone();
        let rate = money::usd_rate(settings, &fx, today);
        let rate_fetched_at = match rate {
            Some(Rate { date: Some(_), .. }) => fx.fetched_at(today),
            _ => None,
        };
        let models_with_price = prices.current().len() as u32;
        let left_out = left_out.unwrap_or_else(|| self.view().left_out);
        let view = MoneyView {
            currency,
            rate,
            rate_fetched_at,
            rate_source: RATE_SOURCE,
            prices_source: PRICES_SOURCE,
            look_up: settings.look_up_prices,
            models_with_price,
            prices_checked_at: prices.checked_at,
            last_change: prices.last_change,
            left_out,
            rate_error,
            last_error: error,
        };
        self.store(view)
    }

    fn store(&mut self, view: MoneyView) -> MoneyView {
        let changed = {
            let mut current = lock_view(&self.view);
            let changed = *current != view;
            *current = view.clone();
            changed
        };
        if changed {
            if let Some(ui) = &self.ui {
                let _ = ui.send(UiEvent::Money(view.clone()));
            }
        }
        view
    }
}

/// The view from what is stored, before the first round: no request is sent and
/// nothing is invented. The currency and the switch are known only after the
/// settings are read, so they stay missing here.
fn stored_view(app: &AppPaths) -> MoneyView {
    let mut error = None;
    let prices = match Prices::load(app) {
        Ok(prices) => prices,
        Err(failure) => {
            error = combine(error, format!("prices.json: {failure}"));
            Prices::default()
        }
    };
    if let Err(failure) = Fx::load(app) {
        error = combine(error, format!("fx.json: {failure}"));
    }
    MoneyView {
        currency: None,
        rate: None,
        rate_fetched_at: None,
        rate_source: RATE_SOURCE,
        prices_source: PRICES_SOURCE,
        look_up: false,
        models_with_price: prices.current().len() as u32,
        prices_checked_at: prices.checked_at,
        last_change: prices.last_change,
        left_out: Vec::new(),
        rate_error: None,
        last_error: error,
    }
}

fn local_day(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&Local).date_naive()
}

fn combine(first: Option<String>, second: String) -> Option<String> {
    Some(match first {
        Some(first) => format!("{first}; {second}"),
        None => second,
    })
}

/// Every model with a looked-up price. `since` is when that price started.
pub(crate) fn price_rows(app: &AppPaths) -> Result<Vec<PriceRow>, MoneyError> {
    Ok(Prices::load(app)?
        .current()
        .into_iter()
        .map(|current| PriceRow {
            key: current.key,
            price: current.price.clone(),
            tiered: current.price.tiered,
            since: current.since,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    use serde_json::json;
    use tempfile::tempdir;

    struct Captured {
        path: String,
        headers: Vec<(String, String)>,
    }

    impl Captured {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("a test date")
    }

    fn model_price(input: f64) -> ModelPrice {
        ModelPrice {
            input,
            output: input * 5.0,
            cache_write: input * 1.25,
            cache_read: input * 0.05,
            tiered: false,
        }
    }

    fn stored_prices(at: DateTime<Utc>) -> Prices {
        let mut prices = Prices::default();
        let models = [
            ("anthropic/claude-opus-5-5", model_price(4.0)),
            ("openai/gpt-6", model_price(2.0)),
        ]
        .into_iter()
        .map(|(key, price)| (key.to_string(), price))
        .collect();
        prices.take(models, at);
        prices.checked_at = Some(at);
        prices.etag = Some("W/\"old\"".to_string());
        prices
    }

    /// The request line and the headers, read from the local test connection.
    fn read_request(stream: &std::net::TcpStream) -> Captured {
        // On Windows an accepted stream inherits the listener's non-blocking
        // mode (two servers here poll their listener); a read would then fail
        // with WouldBlock while the client is still sending.
        stream
            .set_nonblocking(false)
            .expect("the accepted stream blocks");
        let mut reader = BufReader::new(stream.try_clone().expect("a stream clone"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("the request line");
        let path = line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let mut headers = Vec::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).expect("a header") == 0 {
                break;
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        Captured { path, headers }
    }

    /// A status, a few headers and a body on the local test connection.
    fn write_reply(
        mut stream: std::net::TcpStream,
        status: u16,
        headers: Vec<(&str, &str)>,
        body: &str,
    ) {
        let mut text = format!("HTTP/1.1 {status} seatswap-test\r\n");
        for (name, value) in headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        text.push_str(body);
        stream.write_all(text.as_bytes()).expect("a reply");
    }

    fn start_server(
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        body: &'static str,
    ) -> (String, Receiver<Captured>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local server");
        let base = format!("http://{}", listener.local_addr().expect("a local address"));
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one request");
            let captured = read_request(&stream);
            write_reply(stream, status, headers, body);
            let _ = sender.send(captured);
        });
        (base, receiver, handle)
    }

    /// A server that answers when spoken to and reports whether it was.
    fn quiet_server() -> (String, Receiver<Captured>, thread::JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local server");
        let base = format!("http://{}", listener.local_addr().expect("a local address"));
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(300);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let captured = read_request(&stream);
                        write_reply(stream, 200, Vec::new(), "");
                        let _ = sender.send(captured);
                        return false;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return true;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return true,
                }
            }
        });
        (base, receiver, handle)
    }

    fn lookup_api(base: &str) -> Api {
        Api::with_urls(
            &format!("{base}/api/oauth/usage"),
            &format!("{base}/v1/oauth/token"),
        )
        .with_lookup_urls(&format!("{base}/api.json"), &format!("{base}/v1/latest"))
    }

    fn runtime(app: AppPaths, api: Api) -> LookupRuntime {
        LookupRuntime::new(app, api, None, Arc::new(Mutex::new(MoneyView::default())))
    }

    /// A runtime whose accounts.json holds `settings`, without a round:
    /// `price_wanted` reads the settings from the file the way the round does.
    fn runtime_with(app: AppPaths, base: &str, settings: Settings) -> LookupRuntime {
        write_settings(&app, &settings);
        runtime(app, lookup_api(base))
    }

    fn no_rate_settings() -> Settings {
        Settings {
            currency: None,
            look_up_prices: true,
            ..Settings::default()
        }
    }

    #[test]
    fn a_failed_lookup_keeps_the_stored_prices() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let stored_at = utc("2026-11-13T09:00:00Z");
        let prices = stored_prices(stored_at);
        prices.save(&app).expect("the stored prices");

        let (base, receiver, handle) = start_server(500, Vec::new(), "");
        let mut runtime = runtime(app.clone(), lookup_api(&base));
        let view = runtime.round(&no_rate_settings(), utc("2026-11-14T09:00:00Z"));
        let request = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the fake server received no request");
        handle.join().expect("the server thread");

        assert_eq!(request.path, "/api.json");
        assert_eq!(request.header("If-None-Match"), Some("W/\"old\""));
        assert!(view.last_error.is_some(), "the failure is remembered");
        assert_eq!(view.prices_checked_at, Some(stored_at));
        assert_eq!(view.models_with_price, 2);

        let after = Prices::load(&app).expect("prices.json");
        assert_eq!(after.checked_at, Some(stored_at), "checkedAt stays");
        assert_eq!(after.etag.as_deref(), Some("W/\"old\""));
    }

    #[test]
    fn a_body_of_another_shape_keeps_the_stored_prices() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let stored_at = utc("2026-11-13T09:00:00Z");
        let prices = stored_prices(stored_at);
        prices.save(&app).expect("the stored prices");
        let before = std::fs::read(app.prices_file()).expect("prices.json");

        let (base, receiver, handle) = start_server(200, Vec::new(), r#"{"x": 1}"#);
        let mut runtime = runtime(app.clone(), lookup_api(&base));
        let view = runtime.round(&no_rate_settings(), utc("2026-11-14T09:00:00Z"));
        let request = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the fake server received no request");
        handle.join().expect("the server thread");

        assert_eq!(request.path, "/api.json");
        assert!(
            view.last_error
                .as_deref()
                .is_some_and(|error| error.contains("models.dev")),
            "the view names models.dev, got {:?}",
            view.last_error
        );
        let after = std::fs::read(app.prices_file()).expect("prices.json");
        assert_eq!(
            after, before,
            "a body of another shape leaves prices.json byte-for-byte the same"
        );
        assert_eq!(view.models_with_price, 2);
    }

    #[test]
    fn not_modified_only_moves_checked_at() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let stored_at = utc("2026-11-13T09:00:00Z");
        let prices = stored_prices(stored_at);
        prices.save(&app).expect("the stored prices");
        let now = utc("2026-11-14T10:00:00Z");

        let (base, receiver, handle) = start_server(304, vec![("ETag", "W/\"old\"")], "");
        let mut runtime = runtime(app.clone(), lookup_api(&base));
        let view = runtime.round(&no_rate_settings(), now);
        let request = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the fake server received no request");
        handle.join().expect("the server thread");

        assert_eq!(request.header("If-None-Match"), Some("W/\"old\""));
        assert_eq!(view.prices_checked_at, Some(now));
        assert_eq!(view.last_error, None);
        let after = Prices::load(&app).expect("prices.json");
        assert_eq!(after.checked_at, Some(now));
        assert_eq!(after.etag.as_deref(), Some("W/\"old\""));
        assert_eq!(after.models.len(), 2);
    }

    #[test]
    fn off_means_no_request() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let stored_at = utc("2026-11-13T09:00:00Z");
        let prices = stored_prices(stored_at);
        prices.save(&app).expect("the stored prices");

        let (base, receiver, handle) = quiet_server();
        let mut runtime = runtime(app.clone(), lookup_api(&base));
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: false,
            ..Settings::default()
        };
        let view = runtime.round(&settings, utc("2026-11-14T09:00:00Z"));
        let quiet = handle.join().expect("the server thread");

        assert!(quiet, "a look-up was switched off but a request arrived");
        assert!(receiver.try_recv().is_err(), "the server saw a request");
        assert!(!view.look_up);
        assert_eq!(view.prices_checked_at, Some(stored_at));
        assert_eq!(view.models_with_price, 2);
    }

    #[test]
    fn price_rows_show_the_looked_up_price() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let at = utc("2026-11-13T09:00:00Z");
        stored_prices(at).save(&app).expect("prices.json");

        let rows = price_rows(&app).expect("the rows");
        assert_eq!(rows.len(), 2);
        let looked = rows
            .iter()
            .find(|row| row.key == "anthropic/claude-opus-5-5")
            .expect("opus");
        assert_eq!(looked.price.input, 4.0);
        assert_eq!(looked.since, Some(at));
    }

    #[test]
    fn the_view_serialises_the_fields_the_window_reads() {
        let view = MoneyView {
            currency: Some("EUR".to_string()),
            rate: Some(Rate {
                value: 0.9048,
                date: Some(NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()),
            }),
            rate_fetched_at: Some(utc("2026-11-13T10:10:00Z")),
            rate_source: RATE_SOURCE,
            prices_source: PRICES_SOURCE,
            look_up: true,
            models_with_price: 3,
            prices_checked_at: Some(utc("2026-11-14T09:00:00Z")),
            last_change: Some(PriceChange {
                at: utc("2026-11-14T09:00:00Z"),
                added: vec!["openai/gpt-6".to_string()],
                changed: Vec::new(),
                removed: Vec::new(),
            }),
            left_out: vec!["anthropic/claude-x: the input cost is not a number".to_string()],
            rate_error: Some("the dollar rate: the look-up failed".to_string()),
            last_error: None,
        };
        let json = serde_json::to_value(view).expect("the event's JSON");
        // Nothing reads a provider list from the view since the "add a model"
        // control is gone (review left-over; fix point 5).
        assert!(
            json.get("providers").is_none(),
            "the view carries no providers"
        );
        assert_eq!(json["modelsWithPrice"], json!(3));
        assert_eq!(json["rate"]["value"], json!(0.9048));
        assert_eq!(json["rate"]["date"], json!("2026-11-13"));
        assert_eq!(json["rateFetchedAt"], json!("2026-11-13T10:10:00Z"));
        assert_eq!(
            json["rateSource"],
            json!("European Central Bank via Frankfurter")
        );
        assert_eq!(json["pricesSource"], json!("models.dev"));
        assert_eq!(json["pricesCheckedAt"], json!("2026-11-14T09:00:00Z"));
        assert_eq!(
            json["leftOut"],
            json!(["anthropic/claude-x: the input cost is not a number"])
        );
        assert_eq!(
            json["rateError"],
            json!("the dollar rate: the look-up failed")
        );
    }

    #[test]
    fn left_out_is_kept_until_a_later_list_names_others() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        stored_prices(utc("2026-11-13T09:00:00Z"))
            .save(&app)
            .expect("the stored prices");
        let mut runtime = runtime(app, lookup_api("http://127.0.0.1:1"));
        let settings = no_rate_settings();
        let today = NaiveDate::from_ymd_opt(2026, 11, 14).expect("a test date");
        let names = vec!["anthropic/claude-x: the input price is negative".to_string()];

        let taken = runtime.build_view(&settings, None, None, today, Some(names.clone()));
        assert_eq!(taken.left_out, names);

        let not_modified = runtime.build_view(&settings, None, None, today, None);
        assert_eq!(not_modified.left_out, names, "NotModified keeps the names");

        let error = runtime.build_view(
            &settings,
            Some("API prices: models.dev's answer failed".to_string()),
            Some("the dollar rate: the look-up failed".to_string()),
            today,
            None,
        );
        assert_eq!(error.left_out, names, "an error keeps the names");

        let cleared = runtime.build_view(&settings, None, None, today, Some(Vec::new()));
        assert!(
            cleared.left_out.is_empty(),
            "a later list without left-out models clears them"
        );
    }

    const MODELS_DEV_BODY: &str = r#"{
      "anthropic": {"id": "anthropic", "models": {
        "claude-opus-5-5": {"id": "claude-opus-5-5", "cost": {"input": 4, "output": 20}}
      }}
    }"#;

    /// Frankfurter's answer for both currencies the tests ask for: Friday
    /// 13 Nov 2026.
    const RATE_BODY: &str =
        r#"{"base":"USD","date":"2026-11-13","rates":{"EUR":0.90481,"CHF":0.93}}"#;

    /// A server that answers every request and reports the path of each one.
    fn path_server(
        prices_status: u16,
        rate_status: u16,
        rate_body: &'static str,
    ) -> (String, Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local server");
        let base = format!("http://{}", listener.local_addr().expect("a local address"));
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let captured = read_request(&stream);
                        let path = captured.path;
                        let (status, answer) = if path.starts_with("/api.json") {
                            (prices_status, MODELS_DEV_BODY)
                        } else {
                            (rate_status, rate_body)
                        };
                        // Report the path before the answer, so a test that
                        // waits for the round sees every request it made.
                        if sender.send(path).is_err() {
                            return;
                        }
                        write_reply(stream, status, Vec::new(), answer);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        });
        (base, receiver, handle)
    }

    fn rate_asks(paths: &Receiver<String>) -> usize {
        paths
            .try_iter()
            .filter(|path| path.starts_with("/v1/latest"))
            .count()
    }

    /// accounts.json holding `settings`, the way the round and `price_wanted`
    /// read them.
    fn write_settings(app: &AppPaths, settings: &Settings) {
        std::fs::create_dir_all(&app.data_dir).expect("the data folder");
        let file = store::AccountsFile {
            settings: settings.clone(),
            ..store::AccountsFile::default()
        };
        std::fs::write(
            app.accounts_file(),
            serde_json::to_vec(&file).expect("accounts JSON"),
        )
        .expect("accounts.json");
    }

    #[test]
    fn an_unknown_model_asks_models_dev_once_an_hour() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 200, "");
        let settings = Settings {
            look_up_prices: true,
            ..Settings::default()
        };
        let mut runtime = runtime_with(app, &base, settings);
        let start = utc("2026-11-13T09:00:00Z");

        assert!(runtime.price_wanted("claude-x-9", start), "the first ask");
        assert_eq!(
            paths
                .recv_timeout(Duration::from_secs(10))
                .expect("the fake server received no request"),
            "/api.json"
        );
        assert!(
            runtime.wanted.contains_key("claude-x-9"),
            "the once-an-hour map is keyed by the model name the session gives"
        );
        assert!(
            !runtime.wanted.contains_key("anthropic/claude-x-9"),
            "not by price_key(\"anthropic\", …): the provider is not known here"
        );
        assert!(
            !runtime.price_wanted("claude-x-9", start + chrono::Duration::minutes(59)),
            "not twice within an hour"
        );
        assert!(paths.try_recv().is_err(), "the second call sent no request");
        assert!(
            runtime.price_wanted("claude-x-9", start + chrono::Duration::hours(1)),
            "an hour later the same model is asked again"
        );
        assert_eq!(
            paths
                .recv_timeout(Duration::from_secs(10))
                .expect("the fake server received no request"),
            "/api.json"
        );
    }

    /// With the look-ups off neither host is contacted (spec §11.5); a model
    /// met then falls back to the stored list, once the hourly round has it.
    #[test]
    fn no_model_asks_models_dev_with_look_ups_off() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 200, "");
        let settings = Settings {
            look_up_prices: false,
            ..Settings::default()
        };
        let mut runtime = runtime_with(app, &base, settings);

        assert!(
            !runtime.price_wanted("claude-x-9", utc("2026-11-13T09:00:00Z")),
            "no request is made with the look-ups off"
        );
        assert!(paths.try_recv().is_err(), "the fake server got no request");
    }

    /// The settings that count are the ones in effect now, not the copy the
    /// last round read: look-ups switched off since then send no request.
    #[test]
    fn look_ups_off_after_the_last_round_sends_no_request() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 200, "");
        let on = Settings {
            look_up_prices: true,
            ..Settings::default()
        };
        let mut runtime = runtime_with(app.clone(), &base, on.clone());
        runtime.round(&on, utc("2026-11-13T09:00:00Z"));
        paths
            .recv_timeout(Duration::from_secs(10))
            .expect("the round asked models.dev");

        write_settings(
            &app,
            &Settings {
                look_up_prices: false,
                ..Settings::default()
            },
        );
        assert!(
            !runtime.price_wanted("claude-x-9", utc("2026-11-13T09:01:00Z")),
            "the last round's copy still had the look-ups on"
        );
        assert!(
            paths.try_recv().is_err(),
            "a request arrived with the look-ups off"
        );
    }

    /// The window reads the view before the thread's first round is done: it
    /// must not claim that there is no price at all.
    #[test]
    fn proof_the_view_before_the_first_round_does_not_say_zero_models() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let prices = stored_prices(utc("2026-11-13T09:00:00Z"));
        prices.save(&app).expect("the stored prices");

        let runtime = runtime(app, lookup_api("http://127.0.0.1:1"));
        assert_eq!(
            runtime.view().models_with_price,
            2,
            "the view starts empty and the page would read '0 models with a price'"
        );
    }

    /// The thread has a second deadline: a wake runs the job whose deadline
    /// passed, and a rate wake is no models.dev round.
    #[test]
    fn a_wake_runs_the_job_whose_deadline_passed() {
        let now = Instant::now();
        let half_hour = now + RATE_EVERY;
        let hour = now + PRICE_EVERY;
        let three_hours = now + Duration::from_secs(3 * 60 * 60);
        let just_after = |deadline: Instant| deadline + Duration::from_secs(1);
        assert_eq!(
            due_jobs(just_after(half_hour), hour, half_hour),
            DueJobs {
                prices: false,
                rate: true
            },
            "the rate's deadline passed, models.dev's did not"
        );
        assert_eq!(
            due_jobs(just_after(hour), hour, three_hours),
            DueJobs {
                prices: true,
                rate: false
            },
            "models.dev's deadline passed, the rate's did not"
        );
        assert_eq!(
            due_jobs(just_after(hour), hour, hour),
            DueJobs {
                prices: true,
                rate: true
            },
            "both passed"
        );
        assert_eq!(
            due_jobs(now, hour, half_hour),
            DueJobs {
                prices: false,
                rate: false
            },
            "waiting, not due"
        );
    }

    /// The 16:00–18:00 retry runs every 30 minutes: a Monday whose 16:00 answer
    /// carries Friday's date is asked again at 16:30 and 17:00, not at 18:00,
    /// and the rate's own wakes send models.dev no request.
    #[test]
    fn the_afternoon_retry_asks_the_rate_without_models_dev() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 200, RATE_BODY);
        let mut runtime = runtime(app.clone(), lookup_api(&base));
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        // Mon 16 Nov 2026: 15:00 UTC is 16:00 in Frankfurt. The round asks the
        // price list and the rate; the answer carries Friday 13 Nov.
        runtime.round(&settings, utc("2026-11-16T15:00:00Z"));
        let stored = Fx::load(&app).expect("fx.json");
        assert_eq!(
            stored.points[0].fetched_at,
            Some(utc("2026-11-16T15:00:00Z")),
            "the look-up is stamped with the round's clock, not the wall clock"
        );
        let first: Vec<String> = paths.try_iter().collect();
        assert!(
            first.iter().any(|path| path == "/api.json"),
            "the round asked models.dev: {first:?}"
        );
        assert_eq!(
            first
                .iter()
                .filter(|path| path.starts_with("/v1/latest"))
                .count(),
            1,
            "the round asked the rate once: {first:?}"
        );

        // 16:30 and 17:00 in Frankfurt: the rate alone is asked again.
        for at in ["2026-11-16T15:30:00Z", "2026-11-16T16:00:00Z"] {
            let view = runtime.rate_round(&settings, utc(at));
            assert!(view.is_some(), "the rate wake at {at} did nothing");
            let asked: Vec<String> = paths.try_iter().collect();
            assert_eq!(
                asked,
                vec!["/v1/latest?base=USD&symbols=EUR".to_string()],
                "the rate wake at {at} asked models.dev too"
            );
        }

        // 18:00 in Frankfurt: the window has ended, nothing is due.
        assert!(
            runtime
                .rate_round(&settings, utc("2026-11-16T17:00:00Z"))
                .is_none(),
            "18:00 is outside the retry's window"
        );
        assert!(
            paths.try_recv().is_err(),
            "the 18:00 wake asked for something"
        );
    }

    /// An answered ask whose answer is older than today is asked again in the
    /// afternoon: the 16:00–18:00 retry of the schedule.
    #[test]
    fn proof_an_answered_stale_rate_is_asked_again_from_four() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 200, RATE_BODY);
        let mut runtime = runtime(app, lookup_api(&base));
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        // Mon 16 Nov 2026, 09:00 in Frankfurt: the first ask, answered with
        // Friday 13 Nov.
        let first = runtime.round(&settings, utc("2026-11-16T08:00:00Z"));
        assert_eq!(first.rate_error, None, "the rate was answered");
        assert_eq!(rate_asks(&paths), 1);
        // 10:00: the answer's date is still older than today, but the day's
        // ask was answered, so nothing before the afternoon.
        runtime.round(&settings, utc("2026-11-16T09:00:00Z"));
        assert_eq!(rate_asks(&paths), 0, "one ask before the afternoon");
        // 16:31 in Frankfurt (15:31 UTC): the afternoon retry asks again.
        runtime.round(&settings, utc("2026-11-16T15:31:00Z"));
        assert_eq!(rate_asks(&paths), 1, "the afternoon did not ask again");
    }

    /// Every start asks the rate once, even when `fx.json` already holds a rate
    /// with today's date (another start the same day, or a copy from another
    /// machine); after that answer the day's schedule applies again.
    #[test]
    fn every_start_asks_the_rate_once() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let mut fx = Fx {
            currency: "EUR".to_string(),
            ..Fx::default()
        };
        fx.take_fetched(
            NaiveDate::from_ymd_opt(2026, 11, 14).expect("a date"),
            0.9,
            Some(utc("2026-11-14T08:00:00Z")),
        );
        fx.save(&app).expect("fx.json");
        let (base, paths, _handle) = path_server(200, 200, RATE_BODY);
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        // Sat 14 Nov 2026, 11:00 in Frankfurt: today's rate is stored, but this
        // is the first round since the start.
        let mut runtime = runtime(app, lookup_api(&base));
        assert!(
            runtime
                .rate_round(&settings, utc("2026-11-14T10:00:00Z"))
                .is_some(),
            "the start's first rate wake did nothing"
        );
        assert_eq!(rate_asks(&paths), 1, "the start did not ask the rate");
        assert!(
            runtime
                .rate_round(&settings, utc("2026-11-14T10:30:00Z"))
                .is_none(),
            "the second wake of the day asked again"
        );
        assert_eq!(rate_asks(&paths), 0);
    }

    /// The rate asked for today was answered for EUR; a new currency has no
    /// answer and no stored rate, so it must be asked for at once, not after
    /// the next Frankfurt day.
    #[test]
    fn proof_a_currency_change_the_same_day_asks_for_the_new_rate() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 200, RATE_BODY);
        let mut runtime = runtime(app, lookup_api(&base));
        let settings = |currency: &str| Settings {
            currency: Some(currency.to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        // Sat 14 Nov 2026, 11:00 in Frankfurt: a weekend, outside the afternoon.
        // The EUR ask is answered, so the day counts as asked for EUR.
        let first = runtime.round(&settings("EUR"), utc("2026-11-14T10:00:00Z"));
        assert_eq!(first.rate_error, None, "the EUR ask was answered");
        assert_eq!(rate_asks(&paths), 1);

        runtime.round(&settings("CHF"), utc("2026-11-14T10:05:00Z"));
        assert_eq!(
            rate_asks(&paths),
            1,
            "the new currency CHF has no stored rate and was not asked for"
        );
    }

    /// Only an answer marks the day: after the 09:00 ask failed, the round at
    /// 11:00 asks again ("due once per Frankfurt day as today").
    #[test]
    fn proof_a_failed_rate_is_asked_again_in_the_morning() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 500, "");
        let mut runtime = runtime(app, lookup_api(&base));
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        // Fri 13 Nov 2026, 09:00 and 11:00 in Frankfurt.
        let first = runtime.round(&settings, utc("2026-11-13T08:00:00Z"));
        assert!(
            first.rate_error.is_some(),
            "the failed rate is remembered at the rate"
        );
        runtime.round(&settings, utc("2026-11-13T10:00:00Z"));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            rate_asks(&paths),
            2,
            "the failed ask of 09:00 was not repeated at 11:00"
        );
    }

    /// The rate wake rebuilds the view; the price look-up's error of the hourly
    /// round is still the last word about models.dev and must stay on the page.
    #[test]
    fn proof_a_rate_wake_keeps_the_price_look_ups_error() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, _paths, _handle) = path_server(500, 500, "");
        let mut runtime = runtime(app, lookup_api(&base));
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        let hourly = runtime.round(&settings, utc("2026-11-16T08:00:00Z"));
        assert!(
            hourly.last_error.is_some(),
            "models.dev failed in the round"
        );
        let wake = runtime
            .rate_round(&settings, utc("2026-11-16T08:30:00Z"))
            .expect("the failed rate is asked again");
        assert!(wake.rate_error.is_some(), "the rate failed again");
        assert!(
            wake.last_error.is_some(),
            "the rate wake wiped the models.dev error from the view"
        );
    }

    /// The rate error says no rate arrived: it stays on the view until an ask
    /// is answered, also after the afternoon retry's window (fix point 5,
    /// review finding 15). A failed ask does not count as an answer, so the
    /// next round asks again and the reason stays.
    #[test]
    fn proof_a_rate_error_stays_after_the_afternoon_window() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 500, "");
        let mut runtime = runtime(app, lookup_api(&base));
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        // Fri 13 Nov 2026, 09:00 in Frankfurt: the rate ask fails.
        let first = runtime.round(&settings, utc("2026-11-13T08:00:00Z"));
        assert!(first.rate_error.is_some(), "the failed rate is remembered");
        // 19:00 in Frankfurt: the afternoon retry's window is over, but the
        // rate never arrived, so the page keeps its reason.
        let later = runtime.round(&settings, utc("2026-11-13T18:00:00Z"));
        assert!(
            later.rate_error.is_some(),
            "the rate never arrived; its error must stay after 18:00"
        );
        assert_eq!(rate_asks(&paths), 2, "a failed rate is asked again");
    }
}
