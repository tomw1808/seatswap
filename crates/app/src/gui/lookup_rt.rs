//! The look-up thread (spec §1.4.1): the API prices from models.dev every hour
//! and the dollar rate from Frankfurter once a day, both behind `lookUpPrices`.
//!
//! The thread is the only writer of `prices.json` in the app; the commands read
//! its [`MoneyView`] and send it over a channel. Every change of the view is a
//! [`UiEvent::Money`] for the window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::Serialize;

use seatswap_core::api::Api;
use seatswap_core::locks;
use seatswap_core::money::{self, Fx, ModelPrice, MoneyError, PriceChange, Prices, Rate};
use seatswap_core::paths::AppPaths;
use seatswap_core::store::{self, Settings};
use seatswap_core::switcher::APP_LOCK_TIMEOUT;

use super::worker::UiEvent;

/// models.dev is checked every hour (spec §1.4.1).
const PRICE_EVERY: Duration = Duration::from_secs(60 * 60);

/// What the settings page shows about money. A missing value stays `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MoneyView {
    pub currency: Option<String>,
    pub rate: Option<Rate>,
    pub look_up: bool,
    pub models_with_price: u32,
    pub prices_checked_at: Option<DateTime<Utc>>,
    pub last_change: Option<PriceChange>,
    pub last_error: Option<String>,
}

/// One model's effective price as the price list shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PriceRow {
    pub key: String,
    pub price: ModelPrice,
    pub own: bool,
    pub tiered: bool,
    pub since: Option<DateTime<Utc>>,
}

/// The commands the window sends to the look-up thread.
enum LookupCommand {
    Now {
        reply: Sender<Result<MoneyView, String>>,
    },
    SetOverride {
        key: String,
        price: Option<ModelPrice>,
        reply: Sender<Result<(), String>>,
    },
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

    /// Set (`Some`) or remove (`None`) one model's own price. The thread writes
    /// `prices.json`; no command writes it beside the thread.
    pub(crate) fn set_override(
        &self,
        key: String,
        price: Option<ModelPrice>,
    ) -> Result<(), String> {
        let (reply, received) = mpsc::channel();
        self.commands
            .send(LookupCommand::SetOverride { key, price, reply })
            .map_err(|_| super::thread_stopped_message())?;
        received
            .recv()
            .map_err(|_| super::thread_stopped_message())?
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
    let mut next = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let wait = next.saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(LookupCommand::Now { reply }) => {
                let view = one_round(&mut runtime, &app, Utc::now());
                let _ = reply.send(Ok(view));
                next = Instant::now() + PRICE_EVERY;
            }
            Ok(LookupCommand::SetOverride { key, price, reply }) => {
                let _ = reply.send(runtime.set_override(key, price));
            }
            Ok(LookupCommand::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                one_round(&mut runtime, &app, Utc::now());
                next = Instant::now() + PRICE_EVERY;
            }
        }
    }
}

fn one_round(runtime: &mut LookupRuntime, app: &AppPaths, now: DateTime<Utc>) -> MoneyView {
    match store::load_accounts(app) {
        Ok(file) => runtime.round(&file.settings, now),
        Err(error) => runtime.settings_failed(error.to_string()),
    }
}

/// One round over the files and the two look-ups. Tests drive it directly with
/// settings and a fixed time, so no test waits for the thread's timer.
pub(crate) struct LookupRuntime {
    app: AppPaths,
    api: Api,
    ui: Option<Sender<UiEvent>>,
    view: Arc<Mutex<MoneyView>>,
    settings: Option<Settings>,
    /// The local day of the last Frankfurter request and the currency it was
    /// for: a weekend has no new rate, so "older than today" alone would ask
    /// every hour (lead decision 3).
    last_rate_day: Option<(NaiveDate, String)>,
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
            settings: None,
            last_rate_day: None,
        }
    }

    pub(crate) fn view(&self) -> MoneyView {
        lock_view(&self.view).clone()
    }

    /// One round with settings that were read already. It sends models.dev a
    /// request only when `lookUpPrices` is on; with it off the view still shows
    /// what is stored.
    pub(crate) fn round(&mut self, settings: &Settings, now: DateTime<Utc>) -> MoneyView {
        let mut error: Option<String> = None;
        let today = local_day(now);
        if settings.look_up_prices {
            if let Err(failure) = money::look_up_prices(&self.api, &self.app, now) {
                error = combine(error, format!("API prices: {failure}"));
            }
            if let Some(currency) = settings.currency.clone() {
                if currency != "USD" {
                    let fx = match Fx::load(&self.app) {
                        Ok(fx) => Some(fx),
                        Err(failure) => {
                            error = combine(error, format!("the dollar rate: {failure}"));
                            None
                        }
                    };
                    if let Some(fx) = fx {
                        let newest = if fx.currency == currency {
                            fx.points.last().map(|point| point.date)
                        } else {
                            None
                        };
                        let stale = newest.is_none_or(|date| date < today);
                        let asked = self
                            .last_rate_day
                            .as_ref()
                            .is_some_and(|(day, code)| *day == today && *code == currency);
                        if stale && !asked {
                            match money::look_up_rate(&self.api, &self.app, &currency) {
                                // Only an answer that arrived marks the day: a
                                // failure is asked again next round.
                                Ok(_) => {
                                    self.last_rate_day = Some((today, currency.clone()));
                                }
                                Err(failure) => {
                                    error = combine(error, format!("the dollar rate: {failure}"));
                                }
                            }
                        }
                    }
                }
            }
        }
        self.settings = Some(settings.clone());
        self.build_view(settings, error, today)
    }

    /// accounts.json could not be read: remember that and send no request.
    pub(crate) fn settings_failed(&mut self, error: String) -> MoneyView {
        let mut view = self.view();
        view.look_up = false;
        view.last_error = Some(format!("cannot read accounts.json: {error}"));
        self.store(view)
    }

    fn set_override(&mut self, key: String, price: Option<ModelPrice>) -> Result<(), String> {
        if let Some(price) = &price {
            validate_price(price)?;
        }
        // The same app lock the look-up takes: an own price written while a
        // look-up runs is never lost, and a look-up never overwrites this save.
        let _lock = locks::acquire_app_lock(&self.app, APP_LOCK_TIMEOUT)
            .map_err(|error| format!("cannot take the app lock: {error}"))?;
        let mut prices = Prices::load(&self.app).map_err(|error| error.to_string())?;
        match price {
            Some(price) => {
                prices.overrides.insert(key, price);
            }
            None => {
                prices.overrides.remove(&key);
            }
        }
        prices.save(&self.app).map_err(|error| error.to_string())?;
        if let Some(settings) = self.settings.clone() {
            self.build_view(&settings, None, local_day(Utc::now()));
        }
        Ok(())
    }

    fn build_view(
        &mut self,
        settings: &Settings,
        error: Option<String>,
        today: NaiveDate,
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
        let fx_for = match &currency {
            Some(code) if *code == fx.currency => fx,
            _ => Fx::default(),
        };
        let rate = money::usd_rate(settings, &fx_for, today);
        let models_with_price = prices.current().len() as u32;
        let view = MoneyView {
            currency,
            rate,
            look_up: settings.look_up_prices,
            models_with_price,
            prices_checked_at: prices.checked_at,
            last_change: prices.last_change,
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
        look_up: false,
        models_with_price: prices.current().len() as u32,
        prices_checked_at: prices.checked_at,
        last_change: prices.last_change,
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

fn validate_price(price: &ModelPrice) -> Result<(), String> {
    for (name, value) in [
        ("input", price.input),
        ("output", price.output),
        ("cache write", price.cache_write),
        ("cache read", price.cache_read),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "the {name} price must be a number of 0 or more, got {value}"
            ));
        }
    }
    Ok(())
}

/// Every model with a price: the looked-up one or the one entered by hand.
/// `since` is when the looked-up price started; an own price has none.
pub(crate) fn price_rows(app: &AppPaths) -> Result<Vec<PriceRow>, MoneyError> {
    Ok(Prices::load(app)?
        .current()
        .into_iter()
        .map(|current| PriceRow {
            key: current.key,
            price: current.price.clone(),
            own: current.own,
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
    fn price_rows_show_an_override_and_the_looked_up_price() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let at = utc("2026-11-13T09:00:00Z");
        let mut prices = stored_prices(at);
        prices
            .overrides
            .insert("openai/gpt-6".to_string(), model_price(3.0));
        prices.save(&app).expect("prices.json");

        let rows = price_rows(&app).expect("the rows");
        assert_eq!(rows.len(), 2);
        let own = rows.iter().find(|row| row.key == "openai/gpt-6").unwrap();
        assert!(own.own);
        assert_eq!(own.price.input, 3.0);
        assert_eq!(own.since, None);
        let looked = rows
            .iter()
            .find(|row| row.key == "anthropic/claude-opus-5-5")
            .unwrap();
        assert!(!looked.own);
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
                own: false,
            }),
            look_up: true,
            models_with_price: 3,
            prices_checked_at: Some(utc("2026-11-14T09:00:00Z")),
            last_change: Some(PriceChange {
                at: utc("2026-11-14T09:00:00Z"),
                added: vec!["openai/gpt-6".to_string()],
                changed: Vec::new(),
                removed: Vec::new(),
            }),
            last_error: None,
        };
        let json = serde_json::to_value(view).expect("the event's JSON");
        assert_eq!(json["modelsWithPrice"], json!(3));
        assert_eq!(json["rate"]["value"], json!(0.9048));
        assert_eq!(json["rate"]["date"], json!("2026-11-13"));
        assert_eq!(json["pricesCheckedAt"], json!("2026-11-14T09:00:00Z"));
    }

    const MODELS_DEV_BODY: &str = r#"{
      "anthropic": {"id": "anthropic", "models": {
        "claude-opus-5-5": {"id": "claude-opus-5-5", "cost": {"input": 4, "output": 20}}
      }}
    }"#;

    /// A server that answers every request and reports the path of each one.
    fn path_server(
        prices_status: u16,
        rate_status: u16,
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
                            (rate_status, "")
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

    /// A rate request that failed taught nothing: the next round of the same
    /// day must ask again.
    #[test]
    fn proof_a_failed_rate_is_asked_again_in_the_same_day() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let (base, paths, _handle) = path_server(200, 500);
        let mut runtime = runtime(app, lookup_api(&base));
        let settings = Settings {
            currency: Some("EUR".to_string()),
            look_up_prices: true,
            ..Settings::default()
        };

        let first = runtime.round(&settings, utc("2026-11-14T12:00:00Z"));
        assert!(first.last_error.is_some(), "the failed rate is remembered");
        // An hour later, the same local day.
        runtime.round(&settings, utc("2026-11-14T13:00:00Z"));

        let asked: Vec<String> = paths.try_iter().collect();
        let rate_requests = asked
            .iter()
            .filter(|path| path.starts_with("/v1/latest"))
            .count();
        assert!(
            rate_requests >= 2,
            "the rate was asked once and never again after the failure: {asked:?}"
        );
    }
}
