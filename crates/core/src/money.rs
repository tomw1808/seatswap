//! Money (spec §1.4, §1.4.1, §3.6): the API prices of models with their dates,
//! the dollar rate with its dates, and the value of token records. Everything is
//! kept in two files seatswap owns (`prices.json`, `fx.json`); a missing value is
//! `None`, never a number.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::api::{Api, ApiError, Fetched};
use crate::atomic::{self, Access};
use crate::burn::rates::{timelines, Timeline};
use crate::history::{
    same_reset, AccountRef, Event, EventKind, HistoryDir, HistoryError, Hosted, Hosts, Range,
    Sample, TokenRecord, WindowRecord,
};
use crate::locks::{self, LockError};
use crate::paths::AppPaths;
use crate::store::{Account, Settings};
use crate::switcher::APP_LOCK_TIMEOUT;

/// The models.dev providers seatswap reads prices from (spec §1.4.1).
pub const PRICE_PROVIDERS: [&str; 3] = ["anthropic", "openai", "ollama-cloud"];

/// The version of `prices.json` and `fx.json` this build reads and writes.
const MONEY_VERSION: u32 = 1;

const MICRO: f64 = 1_000_000.0;

#[derive(Debug, thiserror::Error)]
pub enum MoneyError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path} has version {version}, but this build reads version 1")]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("models.dev's answer is not the expected model list: {0}")]
    ModelsDev(String),
    #[error("the look-up failed: {0}")]
    Api(#[from] ApiError),
    #[error("cannot take the app lock: {0}")]
    Lock(#[from] LockError),
    #[error("{currency}'s dollar rate {rate} is not a number above 0")]
    BadRate { currency: String, rate: f64 },
    #[error(transparent)]
    History(#[from] HistoryError),
}

/// What one model costs at API prices, in USD per million tokens (spec §1.4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    /// The model has `cost.tiers` (a price that depends on the prompt size);
    /// this step counts its base price.
    pub tiered: bool,
}

impl ModelPrice {
    /// The USD cost of these four token counts at this price.
    fn usd(&self, input: u64, output: u64, cache_write: u64, cache_read: u64) -> f64 {
        (input as f64 * self.input
            + output as f64 * self.output
            + cache_write as f64 * self.cache_write
            + cache_read as f64 * self.cache_read)
            / MICRO
    }

    /// The one price-validity rule (spec §1.4.1): each of the four numbers is
    /// finite and 0 or more. The text names the field.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("input", self.input),
            ("output", self.output),
            ("cache write", self.cache_write),
            ("cache read", self.cache_read),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!(
                    "the {name} price must be a number of 0 or more, got {value}"
                ));
            }
        }
        Ok(())
    }
}

/// One model's price from one moment on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricePoint {
    pub from: DateTime<Utc>,
    pub price: ModelPrice,
}

/// What a look-up changed, for the settings to show (spec §1.4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceChange {
    pub at: DateTime<Utc>,
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

/// The price of one key in effect now: its own price, else its last looked-up
/// point (spec §1.4). `since` is when that point started; an own price has none.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentPrice {
    pub key: String,
    pub price: ModelPrice,
    pub own: bool,
    pub since: Option<DateTime<Utc>>,
}

/// `prices.json`: the looked-up prices with their dates, the last look-up's
/// state, and the prices entered by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prices {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_change: Option<PriceChange>,
    /// Key [`price_key`], points in time order.
    #[serde(default)]
    pub models: BTreeMap<String, Vec<PricePoint>>,
    /// Key [`price_key`], a price entered by hand that wins over the points.
    #[serde(default)]
    pub overrides: BTreeMap<String, ModelPrice>,
    /// The keys whose last looked-up list did not have them. A key is reported
    /// as removed once, not on every later list that still lacks it; an older
    /// `prices.json` loads with this empty.
    #[serde(default)]
    pub missing: BTreeSet<String>,
}

impl Default for Prices {
    fn default() -> Self {
        Prices {
            version: MONEY_VERSION,
            checked_at: None,
            etag: None,
            last_change: None,
            models: BTreeMap::new(),
            overrides: BTreeMap::new(),
            missing: BTreeSet::new(),
        }
    }
}

impl Prices {
    /// `prices.json`; a missing file is empty. A file that does not parse or
    /// names another version is an error naming the path, never replaced.
    pub fn load(app: &AppPaths) -> Result<Prices, MoneyError> {
        let path = app.prices_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Prices::default()),
            Err(source) => return Err(MoneyError::Io { path, source }),
        };
        let prices: Prices = serde_json::from_str(&text).map_err(|source| MoneyError::Json {
            path: path.clone(),
            source,
        })?;
        if prices.version != MONEY_VERSION {
            return Err(MoneyError::UnsupportedVersion {
                path,
                version: prices.version,
            });
        }
        Ok(prices)
    }

    /// Pretty JSON, mode 0600, through [`atomic::write_atomic`].
    pub fn save(&self, app: &AppPaths) -> Result<(), MoneyError> {
        let path = app.prices_file();
        save_json(app, &path, self.version, self)
    }

    /// The price of `key` at `at`: its override, else the last point at or
    /// before `at`, else the first point. `None` when there is no price at all.
    pub fn at(&self, key: &str, at: DateTime<Utc>) -> Option<&ModelPrice> {
        if let Some(price) = self.overrides.get(key) {
            return Some(price);
        }
        let points = self.models.get(key)?;
        let after = points.partition_point(|point| point.from <= at);
        if after == 0 {
            points.first().map(|point| &point.price)
        } else {
            points.get(after - 1).map(|point| &point.price)
        }
    }

    /// The current price of every key: its own price when it has one, else its
    /// last looked-up point, sorted by key. The one place the CLI table, the
    /// price list and the count get "the price of every model" from.
    pub fn current(&self) -> Vec<CurrentPrice> {
        let mut keys: BTreeSet<&String> = self.models.keys().collect();
        keys.extend(self.overrides.keys());
        keys.into_iter()
            .filter_map(|key| {
                if let Some(price) = self.overrides.get(key) {
                    return Some(CurrentPrice {
                        key: key.clone(),
                        price: price.clone(),
                        own: true,
                        since: None,
                    });
                }
                self.models
                    .get(key)
                    .and_then(|points| points.last())
                    .map(|point| CurrentPrice {
                        key: key.clone(),
                        price: point.price.clone(),
                        own: false,
                        since: Some(point.from),
                    })
            })
            .collect()
    }

    /// Take a new list: add a point for every model that is new or whose price
    /// differs from its last point. A model missing from the list keeps its
    /// points and is reported as removed only the first time it is missing. A
    /// model that comes back leaves [`Prices::missing`]. Returns what changed,
    /// or `None` when nothing did; `last_change` keeps its value then.
    pub fn take(
        &mut self,
        list: BTreeMap<String, ModelPrice>,
        at: DateTime<Utc>,
    ) -> Option<PriceChange> {
        let mut added = Vec::new();
        let mut changed = Vec::new();
        for (key, price) in &list {
            self.missing.remove(key);
            match self.models.get_mut(key) {
                Some(points) => {
                    let differs = points
                        .last()
                        .map(|point| point.price != *price)
                        .unwrap_or(true);
                    if differs {
                        insert_point(
                            points,
                            PricePoint {
                                from: at,
                                price: price.clone(),
                            },
                        );
                        changed.push(key.clone());
                    }
                }
                None => {
                    self.models.insert(
                        key.clone(),
                        vec![PricePoint {
                            from: at,
                            price: price.clone(),
                        }],
                    );
                    added.push(key.clone());
                }
            }
        }
        let mut removed = Vec::new();
        for key in self.models.keys() {
            if !list.contains_key(key) && self.missing.insert(key.clone()) {
                removed.push(key.clone());
            }
        }
        if added.is_empty() && changed.is_empty() && removed.is_empty() {
            return None;
        }
        let change = PriceChange {
            at,
            added,
            changed,
            removed,
        };
        self.last_change = Some(change.clone());
        Some(change)
    }
}

/// Insert a point keeping `points` sorted by `from`; a point at the same time
/// replaces the one there. A look-up whose clock moved back then still leaves
/// the list [`Prices::at`] searches in order.
fn insert_point(points: &mut Vec<PricePoint>, point: PricePoint) {
    match points.binary_search_by(|existing| existing.from.cmp(&point.from)) {
        Ok(index) => points[index] = point,
        Err(index) => points.insert(index, point),
    }
}

/// One day's dollar rate: units of the file's currency per 1 USD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FxPoint {
    pub date: NaiveDate,
    pub rate: f64,
}

/// `fx.json`: one currency and its rates with their dates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fx {
    pub version: u32,
    /// The currency these rates are for; empty when nothing was looked up yet.
    pub currency: String,
    /// In date order.
    #[serde(default)]
    pub points: Vec<FxPoint>,
}

impl Default for Fx {
    fn default() -> Self {
        Fx {
            version: MONEY_VERSION,
            currency: String::new(),
            points: Vec::new(),
        }
    }
}

impl Fx {
    /// `fx.json`; a missing file is empty. A file that does not parse or names
    /// another version is an error naming the path.
    pub fn load(app: &AppPaths) -> Result<Fx, MoneyError> {
        let path = app.fx_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Fx::default()),
            Err(source) => return Err(MoneyError::Io { path, source }),
        };
        let fx: Fx = serde_json::from_str(&text).map_err(|source| MoneyError::Json {
            path: path.clone(),
            source,
        })?;
        if fx.version != MONEY_VERSION {
            return Err(MoneyError::UnsupportedVersion {
                path,
                version: fx.version,
            });
        }
        Ok(fx)
    }

    /// Pretty JSON, mode 0600, through [`atomic::write_atomic`].
    pub fn save(&self, app: &AppPaths) -> Result<(), MoneyError> {
        let path = app.fx_file();
        save_json(app, &path, self.version, self)
    }

    /// Replace the point of `date`, or add one, keeping the points in date order.
    pub fn take(&mut self, date: NaiveDate, rate: f64) {
        match self.points.binary_search_by_key(&date, |point| point.date) {
            Ok(index) => self.points[index].rate = rate,
            Err(index) => self.points.insert(index, FxPoint { date, rate }),
        }
    }

    /// The rate at `date`: the point at or before it, else the first point.
    pub fn at(&self, date: NaiveDate) -> Option<f64> {
        self.point_at(date).map(|point| point.rate)
    }

    fn point_at(&self, date: NaiveDate) -> Option<&FxPoint> {
        let after = self.points.partition_point(|point| point.date <= date);
        if after == 0 {
            self.points.first()
        } else {
            self.points.get(after - 1)
        }
    }
}

fn save_json<T: Serialize>(
    app: &AppPaths,
    path: &std::path::Path,
    version: u32,
    value: &T,
) -> Result<(), MoneyError> {
    if version != MONEY_VERSION {
        return Err(MoneyError::UnsupportedVersion {
            path: path.to_path_buf(),
            version,
        });
    }
    atomic::ensure_private_dir(&app.data_dir).map_err(|source| MoneyError::Io {
        path: app.data_dir.clone(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| MoneyError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    atomic::write_atomic(path, &bytes, Access::Private).map_err(|source| MoneyError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// models.dev's list reduced to [`PRICE_PROVIDERS`]: the prices it could read,
/// and the models it left out with the reason (`"<key>: <reason>"`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelsDevList {
    pub prices: BTreeMap<String, ModelPrice>,
    pub left_out: Vec<String>,
}

/// models.dev's `api.json` → the prices of [`PRICE_PROVIDERS`]. A body with none
/// of those providers is an error, not an empty list. A model without
/// `cost.input` and `cost.output` is left out; a missing `cache_read` /
/// `cache_write` is 0.0; a cost that is not a number, negative or not finite
/// leaves that model out of the list and names it in [`ModelsDevList::left_out`],
/// never failing the whole list; `tiered` is set when it has `cost.tiers`.
pub fn prices_from_models_dev(body: &serde_json::Value) -> Result<ModelsDevList, MoneyError> {
    let providers = body
        .as_object()
        .ok_or_else(|| MoneyError::ModelsDev("not a JSON object of providers".to_string()))?;
    let mut prices = BTreeMap::new();
    let mut left_out = Vec::new();
    let mut found = 0;
    for provider in PRICE_PROVIDERS {
        let Some(entry) = providers.get(provider) else {
            continue;
        };
        let models = entry
            .get("models")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| MoneyError::ModelsDev(format!("{provider} has no list of models")))?;
        found += 1;
        for model in models.values() {
            let Some(id) = model.get("id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let key = price_key(provider, id);
            let cost = model.get("cost");
            let number = |name: &str| -> Result<Option<f64>, String> {
                let Some(value) = cost.and_then(|cost| cost.get(name)) else {
                    return Ok(None);
                };
                value
                    .as_f64()
                    .map(Some)
                    .ok_or_else(|| format!("the {name} cost is not a number: {value}"))
            };
            let parse = || -> Result<Option<ModelPrice>, String> {
                let (Some(input), Some(output)) = (number("input")?, number("output")?) else {
                    return Ok(None);
                };
                let price = ModelPrice {
                    input,
                    output,
                    cache_write: number("cache_write")?.unwrap_or(0.0),
                    cache_read: number("cache_read")?.unwrap_or(0.0),
                    tiered: cost
                        .and_then(|cost| cost.get("tiers"))
                        .is_some_and(|tiers| !tiers.is_null()),
                };
                price.validate()?;
                Ok(Some(price))
            };
            match parse() {
                Ok(Some(price)) => {
                    prices.insert(key, price);
                }
                Ok(None) => {}
                Err(reason) => left_out.push(format!("{key}: {reason}")),
            }
        }
    }
    if found == 0 {
        return Err(MoneyError::ModelsDev(format!(
            "the body has none of the providers {}",
            PRICE_PROVIDERS.join(", ")
        )));
    }
    Ok(ModelsDevList { prices, left_out })
}

/// A model's key in [`Prices`]: `"<provider>/<model id>"`.
pub fn price_key(provider: &str, model_id: &str) -> String {
    format!("{provider}/{model_id}")
}

/// Split an own price's key into its provider and model id. A key is
/// `"<provider>/<model id>"`, the provider one of [`PRICE_PROVIDERS`] and the
/// model id not empty; anything else is an error with the text a page shows.
pub fn parse_price_key(key: &str) -> Result<(&str, &str), String> {
    let (provider, model_id) = key.split_once('/').ok_or_else(|| {
        "a model key is provider/model, e.g. anthropic/claude-opus-5-5".to_string()
    })?;
    if !PRICE_PROVIDERS.contains(&provider) {
        return Err(format!(
            "a model key is provider/model with a provider from {}, e.g. anthropic/claude-opus-5-5",
            PRICE_PROVIDERS.join(", ")
        ));
    }
    if model_id.is_empty() {
        return Err(
            "a model key is provider/model with a model id after the provider, e.g. anthropic/claude-opus-5-5"
                .to_string(),
        );
    }
    Ok((provider, model_id))
}

/// The dollar rate used at a date (spec §1.4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rate {
    /// Units of the settings' currency per 1 USD.
    pub value: f64,
    /// The day the looked-up rate is from; `None` for a rate entered by hand
    /// and for USD.
    pub date: Option<NaiveDate>,
    /// Whether the rate comes from the settings (`usdRate`), not a look-up.
    pub own: bool,
}

/// The rate to use at `date`: `settings.usdRate` when it was entered for the
/// settings' currency (own), else a point of `fx` looked up for that same
/// currency. A rate of another currency is never used. Currency "USD" is always
/// 1.0; without a currency there is no rate at all.
pub fn usd_rate(settings: &Settings, fx: &Fx, date: NaiveDate) -> Option<Rate> {
    let currency = settings.currency.as_deref()?;
    if currency == "USD" {
        return Some(Rate {
            value: 1.0,
            date: None,
            own: false,
        });
    }
    if let Some(own) = settings
        .usd_rate
        .as_ref()
        .filter(|own| own.currency == currency)
    {
        return Some(Rate {
            value: own.value,
            date: None,
            own: true,
        });
    }
    if fx.currency != currency {
        return None;
    }
    let point = fx.point_at(date)?;
    Some(Rate {
        value: point.rate,
        date: Some(point.date),
        own: false,
    })
}

/// What one look-up did.
#[derive(Debug, Clone, PartialEq)]
pub enum Looked {
    NotModified,
    Taken {
        change: Option<PriceChange>,
        left_out: Vec<String>,
    },
}

/// One look-up of models.dev's list (spec §1.4.1): load `prices.json` for its
/// ETag, ask models.dev, then take the app lock and load `prices.json` again so
/// an own price written while the request ran is kept. Take the new list and
/// store the new ETag and `checkedAt`. "Not modified" only moves `checkedAt`.
/// On any error nothing is saved.
pub fn look_up_prices(api: &Api, app: &AppPaths, now: DateTime<Utc>) -> Result<Looked, MoneyError> {
    let etag = Prices::load(app)?.etag;
    let fetched = api.models_dev(etag.as_deref())?;
    let _lock = locks::acquire_app_lock(app, APP_LOCK_TIMEOUT)?;
    let mut prices = Prices::load(app)?;
    match fetched {
        Fetched::NotModified => {
            prices.checked_at = Some(now);
            prices.save(app)?;
            Ok(Looked::NotModified)
        }
        Fetched::New { etag, body } => {
            let list = prices_from_models_dev(&body)?;
            let change = prices.take(list.prices, now);
            prices.etag = etag;
            prices.checked_at = Some(now);
            prices.save(app)?;
            Ok(Looked::Taken {
                change,
                left_out: list.left_out,
            })
        }
    }
}

/// One look-up of the dollar rate for `currency`. "USD" has no rate to look up
/// and sends no request. A rate of an earlier currency in `fx.json` is replaced
/// by a new [`Fx`] for `currency`; a rate that is not finite or not above 0 is an
/// error, and nothing is saved. The file is read and written under the app lock.
pub fn look_up_rate(
    api: &Api,
    app: &AppPaths,
    currency: &str,
) -> Result<Option<FxPoint>, MoneyError> {
    if currency == "USD" {
        return Ok(None);
    }
    let (date, rate) = api.usd_rate(currency)?;
    if !(rate.is_finite() && rate > 0.0) {
        return Err(MoneyError::BadRate {
            currency: currency.to_string(),
            rate,
        });
    }
    let _lock = locks::acquire_app_lock(app, APP_LOCK_TIMEOUT)?;
    let mut fx = Fx::load(app)?;
    if fx.currency != currency {
        fx = Fx {
            version: MONEY_VERSION,
            currency: currency.to_string(),
            points: Vec::new(),
        };
    }
    fx.take(date, rate);
    fx.save(app)?;
    Ok(Some(FxPoint { date, rate }))
}

/// What one token record costs at API prices, in USD. `None` when the record's
/// model has no price. A record whose four token counts are all 0 costs nothing,
/// so it has the value 0 even when its model has no price (Claude Code writes
/// such records with model `<synthetic>`). The record's model is priced as an
/// Anthropic model (`anthropic/<model>`), the provider Claude Code's transcripts
/// come from.
pub fn api_usd(record: &TokenRecord, prices: &Prices) -> Option<f64> {
    if record.input == 0 && record.out == 0 && record.cw == 0 && record.cr == 0 {
        return Some(0.0);
    }
    let key = price_key("anthropic", &record.model);
    let price = prices.at(&key, record.t)?;
    Some(price.usd(record.input, record.out, record.cw, record.cr))
}

/// The money value of one percent of one week of an account: monthly price ×
/// 12 / 52 / 100 (spec §1.4).
pub fn percent_value(monthly_price: f64) -> f64 {
    monthly_price * 12.0 / 52.0 / 100.0
}

/// Why a value is missing (spec §1.4: the window says so). `Display` is the text a page shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Missing {
    NoCurrency,
    NoPrice { model: String },
    NoAccount,
    NoAccountPrice,
    PriceInOtherCurrency { currency: String },
    PriceWithoutCurrency,
    NoReading,
    UnpricedInWeek { model: String },
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Missing::NoCurrency => f.write_str("set a currency in Settings → General"),
            Missing::NoPrice { model } => write!(f, "no price for {model}"),
            Missing::NoAccount => f.write_str("no account was in use at that time"),
            Missing::NoAccountPrice => f.write_str("the account has no monthly price"),
            Missing::PriceInOtherCurrency { currency } => write!(
                f,
                "the account's price is in {currency}; enter it again in the current currency"
            ),
            Missing::PriceWithoutCurrency => f.write_str(
                "the account's price was entered before a currency was set; enter it again",
            ),
            Missing::NoReading => f.write_str("no reading of that week's usage"),
            Missing::UnpricedInWeek { model } => {
                write!(
                    f,
                    "that week also has tokens of {model}, which has no price"
                )
            }
        }
    }
}

/// The two values of one token record, in the settings' currency.
#[derive(Debug, Clone, PartialEq)]
pub struct Valued {
    /// The tokens at API prices, times the rate of the record's day. Without a
    /// rate of the settings' currency this is the USD value (spec §1.4).
    pub api: Option<f64>,
    /// The share of the account's subscription that the record's week used.
    pub subscription: Option<f64>,
    /// The account that was live on the record's host at its time.
    pub account: Option<AccountRef>,
    /// The currency `api` is in: the settings' currency with a rate, "USD"
    /// without one, and `None` when `api` is `None`.
    pub api_currency: Option<String>,
    /// Why `api` is `None`; `None` when it has a value.
    pub api_missing: Option<Missing>,
    /// Why `subscription` is `None`; `None` when it has a value.
    pub subscription_missing: Option<Missing>,
}

/// One account's weekly window, with what its records are split by.
struct WeekWindow {
    account: AccountRef,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    /// The finished window's final percent, or a running window's last sample.
    used: Option<f64>,
    /// Whether `used` came from a finished [`WindowRecord`].
    finished: bool,
    last_sample: Option<DateTime<Utc>>,
    /// The sum of [`api_usd`] of every record of this account in this window.
    api_sum: f64,
    /// The first record of this account in this window that has tokens but no
    /// price, in read order: no subscription value of the window can be split.
    unpriced: Option<String>,
}

/// Every account's price changes, in time order: the moment, the monthly price
/// and the currency it was entered in (`None` before a currency was set).
type PriceChanges = BTreeMap<AccountRef, Vec<(DateTime<Utc>, Option<f64>, Option<String>)>>;

/// The value of token records, built once for a range (spec §3.6): the account
/// of every record, its weekly window, and the account's price at its time.
pub struct Valuer {
    timelines: HashMap<String, Timeline>,
    windows: Vec<WeekWindow>,
    price_changes: PriceChanges,
    /// The price as `accounts.json` has it now and the currency it was entered
    /// in, for an account without changes.
    stored_prices: BTreeMap<AccountRef, Option<(f64, Option<String>)>>,
    prices: Prices,
    fx: Fx,
    settings: Settings,
}

impl Valuer {
    /// Reads samples, windows and tokens of `range` widened to whole weekly
    /// windows: a week that holds a point of the range begins at most seven
    /// days before it and ends at most seven days after it. Every event over
    /// all of history is read too, because a record's account and its account's
    /// price are the last `Live`/`Switch`/`PriceChanged` at or before it, which
    /// can be older than the range; a price change after the range still names
    /// the first price, which counts for the past.
    pub fn build(
        history: &HistoryDir,
        range: &Range,
        prices: &Prices,
        fx: &Fx,
        settings: &Settings,
        accounts: &[Account],
    ) -> Result<Valuer, MoneyError> {
        let widened = Range {
            from: range.from - Duration::days(7),
            to: range.to + Duration::days(7),
        };
        let windows = history.finished_windows(&widened, &Hosts::All)?;
        let samples = history.read::<Sample>(&widened, &Hosts::All)?;
        let events = history.read::<Event>(&Range::ALL, &Hosts::All)?;
        let tokens = history.read::<TokenRecord>(&widened, &Hosts::All)?;

        let mut valuer = Valuer {
            timelines: timelines(&events.records),
            windows: Vec::new(),
            price_changes: BTreeMap::new(),
            stored_prices: accounts
                .iter()
                .map(|account| {
                    (
                        AccountRef::of(account),
                        account
                            .price
                            .map(|price| (price, account.price_currency.clone())),
                    )
                })
                .collect(),
            prices: prices.clone(),
            fx: fx.clone(),
            settings: settings.clone(),
        };
        for hosted in &windows {
            valuer.finished_window(&hosted.record);
        }
        for hosted in &samples.records {
            valuer.sample_window(&hosted.record);
        }
        for hosted in &events.records {
            if let EventKind::PriceChanged {
                account,
                amount,
                currency,
            } = &hosted.record.kind
            {
                valuer
                    .price_changes
                    .entry(account.clone())
                    .or_default()
                    .push((hosted.record.t, *amount, currency.clone()));
            }
        }
        for hosted in &tokens.records {
            valuer.add_to_window(hosted, prices);
        }
        Ok(valuer)
    }

    /// The index of the account's window that ends like `end`, adding it when
    /// it is new.
    fn window_index(
        &mut self,
        account: &AccountRef,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> usize {
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| &window.account == account && same_reset(window.end, end))
        {
            return index;
        }
        self.windows.push(WeekWindow {
            account: account.clone(),
            start,
            end,
            used: None,
            finished: false,
            last_sample: None,
            api_sum: 0.0,
            unpriced: None,
        });
        self.windows.len() - 1
    }

    fn finished_window(&mut self, record: &WindowRecord) {
        if record.k != crate::api::LimitKind::WeeklyAll {
            return;
        }
        let end = record.end;
        let start = record.weekly_start();
        let index = self.window_index(&record.account, start, end);
        let window = &mut self.windows[index];
        window.used = Some(record.final_percent);
        window.finished = true;
        window.last_sample = Some(record.last_sample);
    }

    fn sample_window(&mut self, sample: &Sample) {
        for limit in &sample.l {
            if limit.k != crate::api::LimitKind::WeeklyAll {
                continue;
            }
            let Some(end) = limit.r else {
                continue;
            };
            let start = end - Duration::days(7);
            let index = self.window_index(&sample.account, start, end);
            let window = &mut self.windows[index];
            if window.finished {
                continue;
            }
            let newer = window.last_sample.is_none_or(|last| sample.t >= last);
            if newer {
                window.last_sample = Some(sample.t);
                window.used = Some(limit.p);
            }
        }
    }

    fn add_to_window(&mut self, hosted: &Hosted<TokenRecord>, prices: &Prices) {
        let record = &hosted.record;
        let Some(account) = self
            .timelines
            .get(&hosted.host)
            .and_then(|timeline| timeline.at(record.t))
            .cloned()
        else {
            return;
        };
        let Some(index) = self.windows.iter().position(|window| {
            window.account == account && window.start <= record.t && record.t < window.end
        }) else {
            return;
        };
        // A record without tokens adds nothing anywhere and is never "unpriced".
        if record.input == 0 && record.out == 0 && record.cw == 0 && record.cr == 0 {
            return;
        }
        match api_usd(record, prices) {
            Some(usd) => self.windows[index].api_sum += usd,
            None => {
                let window = &mut self.windows[index];
                if window.unpriced.is_none() {
                    window.unpriced = Some(record.model.clone());
                }
            }
        }
    }

    /// The money value of one token record of `host`.
    pub fn value(&self, host: &str, record: &TokenRecord) -> Valued {
        let account = self
            .timelines
            .get(host)
            .and_then(|timeline| timeline.at(record.t))
            .cloned();
        let usd = api_usd(record, &self.prices);
        let rate = usd_rate(&self.settings, &self.fx, record.t.date_naive());
        let (api, api_currency, api_missing) = match (usd, rate) {
            (None, _) => (
                None,
                None,
                Some(Missing::NoPrice {
                    model: record.model.clone(),
                }),
            ),
            (Some(_), None) if self.settings.currency.is_none() => {
                (None, None, Some(Missing::NoCurrency))
            }
            (Some(usd), Some(rate)) => {
                (Some(usd * rate.value), self.settings.currency.clone(), None)
            }
            (Some(usd), None) => (Some(usd), Some("USD".to_string()), None),
        };
        let (subscription, subscription_missing) = match (usd, &account) {
            (None, _) => (
                None,
                Some(Missing::NoPrice {
                    model: record.model.clone(),
                }),
            ),
            (Some(_), None) => (None, Some(Missing::NoAccount)),
            (Some(usd), Some(account)) => match self.subscription(account, record.t, usd) {
                Ok(value) => (Some(value), None),
                Err(missing) => (None, Some(missing)),
            },
        };
        Valued {
            api,
            subscription,
            account,
            api_currency,
            api_missing,
            subscription_missing,
        }
    }

    fn subscription(
        &self,
        account: &AccountRef,
        at: DateTime<Utc>,
        usd: f64,
    ) -> Result<f64, Missing> {
        let window = self
            .windows
            .iter()
            .find(|window| &window.account == account && window.start <= at && at < window.end)
            .ok_or(Missing::NoReading)?;
        let used = window.used.ok_or(Missing::NoReading)?;
        let price = self.price_at(account, at)?;
        if let Some(model) = &window.unpriced {
            return Err(Missing::UnpricedInWeek {
                model: model.clone(),
            });
        }
        // The week has no API value to split (all its prices are 0, or it holds
        // only records without tokens): this record's share is 0.
        if window.api_sum <= 0.0 {
            return Ok(0.0);
        }
        Ok(usd / window.api_sum * used * percent_value(price))
    }

    /// The account's monthly price at `at`: the last [`EventKind::PriceChanged`]
    /// at or before it, before the first one the first one; without any change
    /// the price stored for the account now, in the currency `accounts.json`
    /// records for it. A price is only counted in the currency it was entered in
    /// (spec §1.4): a change in another currency is
    /// [`Missing::PriceInOtherCurrency`], one without a currency
    /// [`Missing::PriceWithoutCurrency`]. Only an account without any stored
    /// price is [`Missing::NoAccountPrice`].
    pub fn price_at(&self, account: &AccountRef, at: DateTime<Utc>) -> Result<f64, Missing> {
        let (amount, currency) = match self.price_changes.get(account) {
            Some(changes) if !changes.is_empty() => {
                let after = changes.partition_point(|(t, _, _)| *t <= at);
                let (_, amount, currency) = if after == 0 {
                    changes.first()
                } else {
                    changes.get(after - 1)
                }
                .ok_or(Missing::NoAccountPrice)?;
                (*amount, currency.as_deref())
            }
            _ => {
                return match self.stored_prices.get(account).and_then(Option::as_ref) {
                    Some((amount, currency)) => counted_price(
                        *amount,
                        currency.as_deref(),
                        self.settings.currency.as_deref(),
                    ),
                    None => Err(Missing::NoAccountPrice),
                };
            }
        };
        let amount = amount.ok_or(Missing::NoAccountPrice)?;
        counted_price(amount, currency, self.settings.currency.as_deref())
    }
}

/// A price counts only in the currency it was entered in (spec §1.4). Without
/// a currency in the settings there is none to count it in.
fn counted_price(
    amount: f64,
    currency: Option<&str>,
    current: Option<&str>,
) -> Result<f64, Missing> {
    if current.is_none() {
        return Err(Missing::NoCurrency);
    }
    match currency {
        Some(currency) if Some(currency) != current => Err(Missing::PriceInOtherCurrency {
            currency: currency.to_string(),
        }),
        Some(_) => Ok(amount),
        None => Err(Missing::PriceWithoutCurrency),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::api::LimitKind;
    use crate::history::{Source, WindowRecord};
    use crate::project::ProjectKey;
    use crate::store::OwnRate;

    const AC_ACC: &str = "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04";
    const AC_ORG: &str = "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20";

    const MODELS_DEV: &str = r#"{
      "anthropic": {"id": "anthropic", "models": {
        "claude-opus-5-5": {"id": "claude-opus-5-5", "cost": {"input": 4, "output": 20, "cache_read": 0.2, "cache_write": 5}},
        "claude-haiku-5-5": {"id": "claude-haiku-5-5", "cost": {"input": 0.1, "output": 0.5, "cache_read": 0.01, "cache_write": 0.125,
          "tiers": [{"input": 0.5, "output": 2.5, "cache_read": 0.05, "cache_write": 0.625, "tier": {"type": "context", "size": 100000}}]}},
        "claude-no-price": {"id": "claude-no-price"}
      }},
      "ollama-cloud": {"id": "ollama-cloud", "models": {"deepseek-v4.1-flash": {"id": "deepseek-v4.1-flash", "cost": {"input": 0.3, "output": 1.2}}}},
      "somebody-else": {"id": "somebody-else", "models": {"x": {"id": "x", "cost": {"input": 1, "output": 1}}}}
    }"#;

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("test date")
    }

    fn date(text: &str) -> NaiveDate {
        text.parse().expect("test date")
    }

    fn ac() -> AccountRef {
        AccountRef {
            acc: AC_ACC.to_string(),
            org: AC_ORG.to_string(),
        }
    }

    fn models_dev() -> serde_json::Value {
        serde_json::from_str(MODELS_DEV).expect("the models.dev fixture")
    }

    fn model_list() -> BTreeMap<String, ModelPrice> {
        prices_from_models_dev(&models_dev())
            .expect("the fixture's prices")
            .prices
    }

    fn priced_prices(at: DateTime<Utc>) -> Prices {
        let mut prices = Prices::default();
        prices.take(model_list(), at);
        prices
    }

    fn settings_eur() -> Settings {
        Settings {
            currency: Some("EUR".to_string()),
            usd_rate: Some(OwnRate {
                currency: "EUR".to_string(),
                value: 1.0,
            }),
            ..Settings::default()
        }
    }

    fn empty_fx() -> Fx {
        Fx::default()
    }

    fn account_with_price(price: Option<f64>) -> Account {
        serde_json::from_value(json!({
            "id": "0b4f6a2c8d1e4f7a9c3b5d7e9f1a2b4c",
            "accountUuid": AC_ACC,
            "organizationUuid": AC_ORG,
            "label": "AC",
            "abbr": "AC",
            "color": "#3b82f6",
            "email": "sam@example.com",
            "orgName": "Acme",
            "plan": "Team 5x",
            "price": price,
            "oauthAccount": {
                "accountUuid": AC_ACC,
                "organizationUuid": AC_ORG,
                "emailAddress": "sam@example.com"
            },
            "addedAt": "2026-09-01T00:00:00Z",
            "loginState": "ok",
            "autoBurn": true
        }))
        .expect("a test account")
    }

    fn token(t: DateTime<Utc>, model: &str, input: u64, out: u64, cw: u64, cr: u64) -> TokenRecord {
        TokenRecord {
            t,
            session: "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34".to_string(),
            cwd: "/home/sam/projects/atlas".to_string(),
            project: ProjectKey::Repo("github.com/acme/atlas".to_string()),
            model: model.to_string(),
            input,
            out,
            cw,
            cr,
            answers: 3,
            agent_answers: 1,
        }
    }

    fn live(at: DateTime<Utc>, account: Option<AccountRef>) -> Event {
        Event {
            t: at,
            kind: EventKind::Live { account },
        }
    }

    fn price_changed(at: DateTime<Utc>, amount: Option<f64>, currency: Option<&str>) -> Event {
        Event {
            t: at,
            kind: EventKind::PriceChanged {
                account: ac(),
                amount,
                currency: currency.map(str::to_string),
            },
        }
    }

    fn weekly_sample(t: DateTime<Utc>, end: DateTime<Utc>, percent: f64) -> Sample {
        Sample {
            t,
            account: ac(),
            src: Source::Poll,
            l: vec![crate::history::LimitSample {
                k: LimitKind::WeeklyAll,
                m: None,
                p: percent,
                r: Some(end),
                b: false,
            }],
        }
    }

    fn weekly_window(start: DateTime<Utc>, end: DateTime<Utc>, final_percent: f64) -> WindowRecord {
        WindowRecord {
            account: ac(),
            k: LimitKind::WeeklyAll,
            m: None,
            start: Some(start),
            end,
            final_percent,
            lost: None,
            last_sample: end - Duration::hours(1),
            plan: None,
            uncertain: false,
        }
    }

    fn range(from: &str, to: &str) -> Range {
        Range {
            from: utc(from),
            to: utc(to),
        }
    }

    #[test]
    fn models_dev_gives_three_providers_only() {
        let prices = model_list();
        let keys: Vec<&String> = prices.keys().collect();
        assert_eq!(
            keys,
            vec![
                "anthropic/claude-haiku-5-5",
                "anthropic/claude-opus-5-5",
                "ollama-cloud/deepseek-v4.1-flash",
            ]
        );

        let opus = &prices["anthropic/claude-opus-5-5"];
        assert_eq!(
            (
                opus.input,
                opus.output,
                opus.cache_write,
                opus.cache_read,
                opus.tiered
            ),
            (4.0, 20.0, 5.0, 0.2, false)
        );
        assert!(prices["anthropic/claude-haiku-5-5"].tiered);
        let ollama = &prices["ollama-cloud/deepseek-v4.1-flash"];
        assert_eq!((ollama.input, ollama.output), (0.3, 1.2));
        assert_eq!(ollama.cache_read, 0.0);
        assert_eq!(ollama.cache_write, 0.0);
    }

    #[test]
    fn a_body_of_another_shape_is_an_error() {
        assert!(matches!(
            prices_from_models_dev(&json!([])),
            Err(MoneyError::ModelsDev(_))
        ));
    }

    #[test]
    fn take_adds_points_only_for_changes() {
        let t1 = utc("2026-11-01T00:00:00Z");
        let t2 = utc("2026-11-02T00:00:00Z");
        let t3 = utc("2026-11-03T00:00:00Z");
        let mut prices = Prices::default();
        let list = model_list();

        let first = prices.take(list.clone(), t1).expect("a new list changes");
        assert_eq!(first.at, t1);
        assert_eq!(first.added.len(), 3);
        assert!(first.changed.is_empty());
        assert!(first.removed.is_empty());
        assert_eq!(prices.models.len(), 3);
        assert_eq!(prices.models["anthropic/claude-opus-5-5"].len(), 1);

        assert_eq!(prices.take(list.clone(), t2), None);
        for points in prices.models.values() {
            assert_eq!(points.len(), 1, "an unchanged list adds no point");
        }

        let mut repriced = list;
        repriced
            .get_mut("anthropic/claude-opus-5-5")
            .expect("opus")
            .output = 18.0;
        let second = prices.take(repriced, t3).expect("a new price changes");
        assert_eq!(second.changed, vec!["anthropic/claude-opus-5-5"]);
        assert!(second.added.is_empty());
        assert_eq!(prices.models["anthropic/claude-opus-5-5"].len(), 2);
    }

    #[test]
    fn a_model_gone_from_the_list_keeps_its_price() {
        let t1 = utc("2026-11-01T00:00:00Z");
        let t2 = utc("2026-11-02T00:00:00Z");
        let mut prices = Prices::default();
        let mut list = model_list();
        prices.take(list.clone(), t1);
        list.remove("anthropic/claude-haiku-5-5");

        let change = prices.take(list, t2).expect("a removed model is a change");
        assert_eq!(change.removed, vec!["anthropic/claude-haiku-5-5"]);
        assert_eq!(
            prices.models["anthropic/claude-haiku-5-5"].len(),
            1,
            "a model missing from the list keeps its point"
        );
    }

    #[test]
    fn at_gives_the_price_of_that_day() {
        let t1 = utc("2026-11-01T00:00:00Z");
        let t2 = utc("2026-11-02T00:00:00Z");
        let t3 = utc("2026-11-03T00:00:00Z");
        let mut prices = Prices::default();
        let mut list = model_list();
        prices.take(list.clone(), t1);

        let key = "anthropic/claude-opus-5-5";
        assert_eq!(prices.at(key, t2).expect("opus").output, 20.0);
        assert_eq!(
            prices.at(key, t1).expect("opus").output,
            20.0,
            "at the first point"
        );

        list.get_mut(key).expect("opus").output = 18.0;
        prices.take(list, t3);
        assert_eq!(prices.at(key, t2).expect("opus").output, 20.0);
        assert_eq!(
            prices
                .at(key, t3 + Duration::seconds(1))
                .expect("opus")
                .output,
            18.0
        );
        assert_eq!(
            prices.at(key, t1 - Duration::days(1)).expect("opus").output,
            20.0,
            "before the first point, the first point"
        );
    }

    #[test]
    fn a_point_taken_with_an_earlier_time_keeps_the_order() {
        let later = utc("2026-11-03T00:00:00Z");
        let earlier = utc("2026-11-02T00:00:00Z");
        let key = "anthropic/claude-opus-5-5";
        let mut prices = Prices::default();
        let mut list = model_list();
        prices.take(list.clone(), later);

        list.get_mut(key).expect("opus").input = 10.0;
        prices.take(list, earlier);

        let points = &prices.models[key];
        assert_eq!(
            points.iter().map(|point| point.from).collect::<Vec<_>>(),
            vec![earlier, later],
            "a point taken with an earlier time is inserted in time order"
        );
        assert_eq!(prices.at(key, earlier).expect("opus").input, 10.0);
        assert_eq!(
            prices.at(key, later).expect("opus").input,
            4.0,
            "the later point still wins at its time"
        );
    }

    #[test]
    fn current_lists_own_prices_first_and_looked_up_ones() {
        let at = utc("2026-11-02T00:00:00Z");
        let mut prices = priced_prices(at);
        let own = "anthropic/claude-haiku-5-5";
        prices.overrides.insert(
            own.to_string(),
            ModelPrice {
                input: 7.0,
                output: 8.0,
                cache_write: 9.0,
                cache_read: 10.0,
                tiered: false,
            },
        );

        let current = prices.current();
        let keys: Vec<&str> = current.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "anthropic/claude-haiku-5-5",
                "anthropic/claude-opus-5-5",
                "ollama-cloud/deepseek-v4.1-flash"
            ],
            "sorted by key"
        );
        let own_row = &current[0];
        assert!(own_row.own);
        assert_eq!(own_row.price.input, 7.0);
        assert_eq!(own_row.since, None, "an own price has no looked-up time");
        let looked_up = &current[1];
        assert!(!looked_up.own);
        assert_eq!(looked_up.price.input, 4.0);
        assert_eq!(looked_up.since, Some(at));
    }

    #[test]
    fn an_override_wins() {
        let t1 = utc("2026-11-01T00:00:00Z");
        let t3 = utc("2026-11-03T00:00:00Z");
        let mut prices = priced_prices(t1);
        let key = "anthropic/claude-opus-5-5";
        prices.overrides.insert(
            key.to_string(),
            ModelPrice {
                input: 1.0,
                output: 99.0,
                cache_write: 0.0,
                cache_read: 0.0,
                tiered: false,
            },
        );
        assert_eq!(prices.at(key, t3).expect("opus").output, 99.0);
        assert_eq!(prices.at(key, t1).expect("opus").output, 99.0);
    }

    #[test]
    fn api_usd_of_a_record() {
        let prices = priced_prices(utc("2026-11-01T00:00:00Z"));
        let record = token(
            utc("2026-11-10T10:00:00Z"),
            "claude-opus-5-5",
            1_000_000,
            2_000_000,
            1_000_000,
            10_000_000,
        );
        // 4 + 40 + 5 + 2 USD.
        assert_eq!(api_usd(&record, &prices), Some(51.0));
    }

    #[test]
    fn no_price_no_value() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let prices = priced_prices(utc("2026-11-01T00:00:00Z"));
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[],
        )
        .expect("a valuer");

        let record = token(
            utc("2026-11-10T10:00:00Z"),
            "claude-unknown",
            1_000_000,
            0,
            0,
            0,
        );
        assert_eq!(api_usd(&record, &prices), None);
        assert_eq!(
            valuer.value("atlas", &record).api_missing,
            Some(Missing::NoPrice {
                model: "claude-unknown".to_string()
            })
        );
    }

    #[test]
    fn the_rate_is_own_then_looked_up_then_missing() {
        let mut settings = Settings {
            currency: Some("EUR".to_string()),
            usd_rate: Some(OwnRate {
                currency: "EUR".to_string(),
                value: 0.95,
            }),
            ..Settings::default()
        };
        assert_eq!(
            usd_rate(&settings, &empty_fx(), date("2026-11-14")),
            Some(Rate {
                value: 0.95,
                date: None,
                own: true,
            })
        );

        settings.usd_rate = None;
        let mut fx = Fx {
            version: 1,
            currency: "EUR".to_string(),
            points: Vec::new(),
        };
        fx.take(date("2026-11-13"), 0.90481);
        assert_eq!(
            usd_rate(&settings, &fx, date("2026-11-14")),
            Some(Rate {
                value: 0.90481,
                date: Some(date("2026-11-13")),
                own: false,
            })
        );

        assert_eq!(
            usd_rate(&settings, &empty_fx(), date("2026-11-14")),
            None,
            "no rate is no rate"
        );

        settings.currency = Some("USD".to_string());
        assert_eq!(
            usd_rate(&settings, &empty_fx(), date("2026-11-14")),
            Some(Rate {
                value: 1.0,
                date: None,
                own: false,
            })
        );
    }

    #[test]
    fn percent_value_of_180_a_month() {
        let value = percent_value(180.0);
        assert!((value - 180.0 * 12.0 / 52.0 / 100.0).abs() < 1e-9);
    }

    /// Two records of one finished week: 30 and 10 USD of tokens, so the 80 %
    /// that week used are split 75 % / 25 %.
    fn two_records() -> (
        HistoryDir,
        tempfile::TempDir,
        TokenRecord,
        TokenRecord,
        Prices,
    ) {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        history
            .append(&live(utc("2026-11-06T00:00:00Z"), Some(ac())))
            .expect("a live event");
        let mut prices = Prices::default();
        prices.overrides.insert(
            "anthropic/claude-opus-5-5".to_string(),
            ModelPrice {
                input: 30.0,
                output: 0.0,
                cache_write: 0.0,
                cache_read: 0.0,
                tiered: false,
            },
        );
        prices.overrides.insert(
            "anthropic/claude-haiku-5-5".to_string(),
            ModelPrice {
                input: 10.0,
                output: 0.0,
                cache_write: 0.0,
                cache_read: 0.0,
                tiered: false,
            },
        );
        let opus = token(
            utc("2026-11-10T10:00:00Z"),
            "claude-opus-5-5",
            1_000_000,
            0,
            0,
            0,
        );
        let haiku = token(
            utc("2026-11-10T11:00:00Z"),
            "claude-haiku-5-5",
            1_000_000,
            0,
            0,
            0,
        );
        history.append(&opus).expect("a token record");
        history.append(&haiku).expect("a token record");
        (history, dir, opus, haiku, prices)
    }

    #[test]
    fn subscription_value_splits_the_used_week_by_api_value() {
        let (history, _dir, opus, haiku, prices) = two_records();
        history
            .append(&price_changed(
                utc("2026-11-06T00:00:00Z"),
                Some(140.0),
                Some("EUR"),
            ))
            .expect("the account's price");
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        let account = account_with_price(Some(140.0));
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account],
        )
        .expect("a valuer");

        let one = valuer.value("atlas", &opus);
        let two = valuer.value("atlas", &haiku);
        assert_eq!(one.account, Some(ac()));
        assert_eq!(one.api, Some(30.0));
        assert_eq!(two.api, Some(10.0));

        let week = 80.0 * percent_value(140.0);
        assert!((one.subscription.expect("a value") - 0.75 * week).abs() < 1e-9);
        assert!((two.subscription.expect("a value") - 0.25 * week).abs() < 1e-9);
        assert!(
            (one.subscription.expect("a value") + two.subscription.expect("a value") - week).abs()
                < 1e-9
        );
    }

    #[test]
    fn a_window_of_two_hosts_uses_the_latest_last_sample() {
        let (history, dir, opus, haiku, prices) = two_records();
        history
            .append(&price_changed(
                utc("2026-11-06T00:00:00Z"),
                Some(140.0),
                Some("EUR"),
            ))
            .expect("the account's price");
        let end = utc("2026-11-14T16:00:00Z");
        let start = end - Duration::days(7);
        let mut from_atlas = weekly_window(start, end, 61.0);
        from_atlas.last_sample = utc("2026-11-14T09:58:00Z");
        history.append(&from_atlas).expect("atlas's window");
        let vega = HistoryDir::at(dir.path(), "vega");
        let mut from_vega = weekly_window(start, end, 63.0);
        from_vega.last_sample = utc("2026-11-14T10:02:00Z");
        vega.append(&from_vega).expect("vega's window");
        fs::write(
            dir.path().join("vega/host.json"),
            "{\"machineId\":\"0b4e7c22-6a1d-4e3f-8c59-2d7a9e1b4f60\"}",
        )
        .expect("the other host's marker");
        let account = account_with_price(Some(140.0));
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account],
        )
        .expect("a valuer");

        // The week's value uses vega's 63 %, not atlas's 61 %: the record with
        // the latest lastSample counts (spec §1.5).
        let week = 63.0 * percent_value(140.0);
        let one = valuer.value("atlas", &opus);
        let two = valuer.value("atlas", &haiku);
        assert!((one.subscription.expect("a value") - 0.75 * week).abs() < 1e-9);
        assert!((two.subscription.expect("a value") - 0.25 * week).abs() < 1e-9);
    }

    #[test]
    fn a_running_week_uses_the_percent_so_far() {
        let (history, _dir, opus, haiku, prices) = two_records();
        let end = utc("2026-11-14T00:00:00Z");
        history
            .append(&price_changed(
                utc("2026-11-06T00:00:00Z"),
                Some(140.0),
                Some("EUR"),
            ))
            .expect("the account's price");
        history
            .append(&weekly_sample(utc("2026-11-13T12:00:00Z"), end, 48.0))
            .expect("a running window's last sample");
        let account = account_with_price(Some(140.0));
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account],
        )
        .expect("a valuer");

        let sum = valuer.value("atlas", &opus).subscription.expect("a value")
            + valuer.value("atlas", &haiku).subscription.expect("a value");
        assert!((sum - 48.0 * percent_value(140.0)).abs() < 1e-9);
    }

    #[test]
    fn an_account_without_a_price_has_no_subscription_value() {
        let (history, _dir, opus, _haiku, prices) = two_records();
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        let account = account_with_price(None);
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account],
        )
        .expect("a valuer");

        let value = valuer.value("atlas", &opus);
        assert_eq!(value.subscription, None);
        assert_eq!(value.subscription_missing, Some(Missing::NoAccountPrice));
        assert_eq!(value.api, Some(30.0));
    }

    #[test]
    fn tokens_before_the_first_live_event_have_no_account() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let prices = priced_prices(utc("2026-11-01T00:00:00Z"));
        let opus = token(
            utc("2026-11-10T10:00:00Z"),
            "claude-opus-5-5",
            1_000_000,
            0,
            0,
            0,
        );
        history.append(&opus).expect("a token record");
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        // One event after the record: before the first event there is no account.
        history
            .append(&live(utc("2026-11-10T12:00:00Z"), Some(ac())))
            .expect("a later live event");
        let account = account_with_price(Some(140.0));
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account],
        )
        .expect("a valuer");

        let value = valuer.value("atlas", &opus);
        assert_eq!(value.account, None);
        assert_eq!(value.subscription, None);
        assert_eq!(value.subscription_missing, Some(Missing::NoAccount));
    }

    #[test]
    fn prices_round_trip_and_refuse_another_version() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let t1 = utc("2026-11-01T00:00:00Z");
        let mut prices = priced_prices(t1);
        prices.checked_at = Some(utc("2026-11-14T09:00:00Z"));
        prices.etag = Some("W/\"abc\"".to_string());
        prices.overrides.insert(
            "anthropic/claude-opus-5-5".to_string(),
            ModelPrice {
                input: 4.0,
                output: 99.0,
                cache_write: 5.0,
                cache_read: 0.2,
                tiered: false,
            },
        );
        prices.save(&app).expect("saved");
        assert_eq!(Prices::load(&app).expect("loaded"), prices);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(app.prices_file())
                .expect("prices.json")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let fresh = AppPaths::at(&dir.path().join("fresh"));
        assert_eq!(
            Prices::load(&fresh).expect("a missing file is empty"),
            Prices::default()
        );

        fs::write(
            app.prices_file(),
            br#"{"version":2,"models":{},"overrides":{}}"#,
        )
        .expect("another version");
        assert!(matches!(
            Prices::load(&app),
            Err(MoneyError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn fx_take_replaces_a_date_and_keeps_order() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let mut fx = Fx::default();
        fx.take(date("2026-11-13"), 0.90);
        fx.take(date("2026-11-11"), 0.91);
        fx.take(date("2026-11-13"), 0.92);
        assert_eq!(
            fx.points.iter().map(|point| point.date).collect::<Vec<_>>(),
            vec![date("2026-11-11"), date("2026-11-13")]
        );
        assert_eq!(fx.at(date("2026-11-13")), Some(0.92));
        assert_eq!(fx.at(date("2026-11-12")), Some(0.91));
        assert_eq!(
            fx.at(date("2026-11-01")),
            Some(0.91),
            "before the first point, the first point"
        );

        let mut stored = fx.clone();
        stored.currency = "EUR".to_string();
        stored.save(&app).expect("saved");
        assert_eq!(Fx::load(&app).expect("loaded"), stored);

        let fresh = AppPaths::at(&dir.path().join("fresh"));
        assert_eq!(
            Fx::load(&fresh).expect("a missing file is empty"),
            Fx::default()
        );

        fs::write(
            app.fx_file(),
            br#"{"version":2,"currency":"EUR","points":[]}"#,
        )
        .expect("another version");
        assert!(matches!(
            Fx::load(&app),
            Err(MoneyError::UnsupportedVersion { .. })
        ));
    }
    #[test]
    fn proof_a_second_identical_list_without_a_model_is_no_change() {
        let t1 = utc("2026-11-01T00:00:00Z");
        let mut prices = Prices::default();
        let mut list = model_list();
        prices.take(list.clone(), t1);
        list.remove("anthropic/claude-haiku-5-5");
        prices
            .take(list.clone(), utc("2026-11-02T00:00:00Z"))
            .expect("the first look-up without haiku changes");
        // An hour later the same list: nothing new happened.
        assert_eq!(prices.take(list, utc("2026-11-02T01:00:00Z")), None);
    }

    #[test]
    fn proof_a_body_with_none_of_the_three_providers_is_an_error() {
        // An error object, or any object of another shape, is not a price list.
        assert!(matches!(
            prices_from_models_dev(&json!({"message": "rate limited"})),
            Err(MoneyError::ModelsDev(_))
        ));
    }

    #[test]
    fn proof_the_first_price_counts_for_a_range_before_every_change() {
        let (history, _dir, opus, haiku, prices) = two_records();
        for (at, amount) in [
            ("2026-11-20T00:00:00Z", 100.0),
            ("2026-12-01T00:00:00Z", 180.0),
        ] {
            history
                .append(&Event {
                    t: utc(at),
                    kind: EventKind::PriceChanged {
                        account: ac(),
                        amount: Some(amount),
                        currency: Some("EUR".to_string()),
                    },
                })
                .expect("a price change");
        }
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        // The account costs 180 now; its first price ever was 100, entered on 20 Nov.
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account_with_price(Some(180.0))],
        )
        .expect("a valuer");
        let sum = valuer.value("atlas", &opus).subscription.expect("a value")
            + valuer.value("atlas", &haiku).subscription.expect("a value");
        assert!(
            (sum - 80.0 * percent_value(100.0)).abs() < 1e-9,
            "the first price counts for the past, got {sum}"
        );
    }

    #[test]
    fn a_negative_cost_leaves_the_model_out_and_names_it() {
        let body = json!({
            "anthropic": {"id": "anthropic", "models": {
                "claude-opus-5-5": {"id": "claude-opus-5-5",
                    "cost": {"input": -4, "output": 20}},
                "claude-haiku-5-5": {"id": "claude-haiku-5-5",
                    "cost": {"input": 0.1, "output": 0.5}}
            }}
        });
        let list = prices_from_models_dev(&body).expect("the rest of the list is read");
        assert!(
            !list.prices.contains_key("anthropic/claude-opus-5-5"),
            "a model with a negative cost is left out"
        );
        assert!(
            list.prices.contains_key("anthropic/claude-haiku-5-5"),
            "the other models are kept"
        );
        assert_eq!(
            list.left_out.len(),
            1,
            "one model is named: {:?}",
            list.left_out
        );
        let entry = &list.left_out[0];
        assert!(
            entry.starts_with("anthropic/claude-opus-5-5: "),
            "the left-out entry names the model, got {entry}"
        );
        assert!(
            entry.contains("input") && entry.contains("-4"),
            "the left-out entry names the field and the value, got {entry}"
        );
    }

    #[test]
    fn a_cost_that_is_not_a_number_leaves_the_model_out_and_names_it() {
        let body = json!({
            "anthropic": {"id": "anthropic", "models": {
                "claude-opus-5-5": {"id": "claude-opus-5-5",
                    "cost": {"input": "four", "output": 20}},
                "claude-haiku-5-5": {"id": "claude-haiku-5-5",
                    "cost": {"input": 0.1, "output": 0.5}}
            }}
        });
        let list = prices_from_models_dev(&body).expect("the rest of the list is read");
        assert!(list.prices.contains_key("anthropic/claude-haiku-5-5"));
        assert_eq!(list.left_out.len(), 1);
        assert!(
            list.left_out[0].starts_with("anthropic/claude-opus-5-5: ")
                && list.left_out[0].contains("input"),
            "got {:?}",
            list.left_out
        );
    }

    #[test]
    fn a_price_must_be_finite_and_zero_or_more() {
        let good = ModelPrice {
            input: 0.0,
            output: 4.0,
            cache_write: 1.5,
            cache_read: 0.2,
            tiered: false,
        };
        assert_eq!(good.validate(), Ok(()));
        for (field, price) in [
            (
                "input",
                ModelPrice {
                    input: -1.0,
                    ..good.clone()
                },
            ),
            (
                "output",
                ModelPrice {
                    output: f64::NAN,
                    ..good.clone()
                },
            ),
            (
                "cache write",
                ModelPrice {
                    cache_write: f64::INFINITY,
                    ..good.clone()
                },
            ),
            (
                "cache read",
                ModelPrice {
                    cache_read: -0.1,
                    ..good.clone()
                },
            ),
        ] {
            let error = price.validate().expect_err("an invalid price");
            assert!(
                error.contains(field),
                "the error names {field}, got {error}"
            );
        }
    }

    #[test]
    fn a_price_in_another_currency_has_no_subscription_value() {
        let (history, _dir, opus, _haiku, prices) = two_records();
        history
            .append(&Event {
                t: utc("2026-11-06T00:00:00Z"),
                kind: EventKind::PriceChanged {
                    account: ac(),
                    amount: Some(140.0),
                    currency: Some("USD".to_string()),
                },
            })
            .expect("a price change in USD");
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");

        let value_of = |history: &HistoryDir| {
            Valuer::build(
                history,
                &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
                &prices,
                &empty_fx(),
                &settings_eur(),
                &[account_with_price(None)],
            )
            .expect("a valuer")
        };
        let usd = value_of(&history).value("atlas", &opus);
        assert_eq!(
            usd.subscription, None,
            "a price in USD is not a subscription in EUR"
        );
        assert_eq!(
            usd.subscription_missing,
            Some(Missing::PriceInOtherCurrency {
                currency: "USD".to_string()
            })
        );
        assert_eq!(usd.api, Some(30.0), "the API value needs no subscription");

        // The same price entered in the current currency does count.
        history
            .append(&Event {
                t: utc("2026-11-06T01:00:00Z"),
                kind: EventKind::PriceChanged {
                    account: ac(),
                    amount: Some(140.0),
                    currency: Some("EUR".to_string()),
                },
            })
            .expect("a price change in EUR");
        let eur = value_of(&history).value("atlas", &opus);
        assert!(
            (eur.subscription.expect("a value") - 0.75 * 80.0 * percent_value(140.0)).abs() < 1e-9
        );
    }

    #[test]
    fn unpriced_tokens_make_the_weeks_value_unknown() {
        let (history, _dir, opus, _haiku, prices) = two_records();
        history
            .append(&price_changed(
                utc("2026-11-06T00:00:00Z"),
                Some(140.0),
                Some("EUR"),
            ))
            .expect("the account's price");
        let unknown = token(
            utc("2026-11-10T12:00:00Z"),
            "claude-unknown",
            1_000_000,
            0,
            0,
            0,
        );
        history
            .append(&unknown)
            .expect("a token record without a price");
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account_with_price(Some(140.0))],
        )
        .expect("a valuer");

        let value = valuer.value("atlas", &opus);
        assert_eq!(
            value.subscription, None,
            "the priced records must not share the unpriced one's tokens"
        );
        assert_eq!(
            value.subscription_missing,
            Some(Missing::UnpricedInWeek {
                model: "claude-unknown".to_string()
            })
        );
        assert_eq!(value.api, Some(30.0), "the API value is still known");
    }

    #[test]
    fn zero_tokens_are_never_unpriced() {
        let (history, _dir, opus, _haiku, prices) = two_records();
        history
            .append(&price_changed(
                utc("2026-11-06T00:00:00Z"),
                Some(140.0),
                Some("EUR"),
            ))
            .expect("the account's price");
        let synthetic = token(utc("2026-11-10T12:00:00Z"), "<synthetic>", 0, 0, 0, 0);
        history.append(&synthetic).expect("a record without tokens");
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account_with_price(Some(140.0))],
        )
        .expect("a valuer");

        let value = valuer.value("atlas", &synthetic);
        assert_eq!(value.api, Some(0.0), "no tokens cost nothing");
        assert_eq!(value.api_missing, None);
        assert_eq!(value.subscription, Some(0.0));

        let opus_value = valuer.value("atlas", &opus);
        assert_eq!(
            opus_value.subscription_missing, None,
            "a record without tokens does not make the week unknown"
        );
        let week = 80.0 * percent_value(140.0);
        assert!((opus_value.subscription.expect("a value") - 0.75 * week).abs() < 1e-9);
    }

    #[test]
    fn an_own_rate_belongs_to_its_currency() {
        let settings = Settings {
            currency: Some("CHF".to_string()),
            usd_rate: Some(OwnRate {
                currency: "EUR".to_string(),
                value: 0.95,
            }),
            ..Settings::default()
        };
        let mut fx = Fx {
            version: 1,
            currency: "CHF".to_string(),
            points: Vec::new(),
        };
        fx.take(date("2026-11-13"), 0.90);
        assert_eq!(
            usd_rate(&settings, &fx, date("2026-11-14")),
            Some(Rate {
                value: 0.90,
                date: Some(date("2026-11-13")),
                own: false,
            }),
            "the own rate for EUR is not used for CHF"
        );
        assert_eq!(
            usd_rate(&settings, &empty_fx(), date("2026-11-14")),
            None,
            "without a CHF rate there is no CHF rate, not the EUR one"
        );
    }

    #[test]
    fn a_rate_of_another_currency_is_not_used() {
        let settings = Settings {
            currency: Some("CHF".to_string()),
            ..Settings::default()
        };
        let mut fx = Fx {
            version: 1,
            currency: "EUR".to_string(),
            points: Vec::new(),
        };
        fx.take(date("2026-11-13"), 0.95);
        assert_eq!(
            usd_rate(&settings, &fx, date("2026-11-14")),
            None,
            "an EUR point is not a CHF rate"
        );
    }

    #[test]
    fn a_price_without_a_currency_does_not_count() {
        let value_for = |with_change: bool| {
            let (history, _dir, opus, _haiku, prices) = two_records();
            if with_change {
                history
                    .append(&price_changed(
                        utc("2026-11-06T00:00:00Z"),
                        Some(140.0),
                        None,
                    ))
                    .expect("a price change before a currency was set");
            }
            history
                .append(&weekly_window(
                    utc("2026-11-07T00:00:00Z"),
                    utc("2026-11-14T00:00:00Z"),
                    80.0,
                ))
                .expect("a finished window");
            Valuer::build(
                &history,
                &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
                &prices,
                &empty_fx(),
                &settings_eur(),
                &[account_with_price(Some(140.0))],
            )
            .expect("a valuer")
            .value("atlas", &opus)
        };

        let change = value_for(true);
        assert_eq!(change.subscription, None);
        assert_eq!(
            change.subscription_missing,
            Some(Missing::PriceWithoutCurrency)
        );

        let stored = value_for(false);
        assert_eq!(stored.subscription, None);
        assert_eq!(
            stored.subscription_missing,
            Some(Missing::PriceWithoutCurrency),
            "a stored price names no currency"
        );
    }

    #[test]
    fn a_stored_price_counts_in_its_own_currency() {
        let (history, _dir, opus, _haiku, prices) = two_records();
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        let mut account = account_with_price(Some(140.0));
        account.price_currency = Some("EUR".to_string());

        let value_in = |currency: &str| {
            let settings = Settings {
                currency: Some(currency.to_string()),
                usd_rate: Some(OwnRate {
                    currency: currency.to_string(),
                    value: 1.0,
                }),
                ..Settings::default()
            };
            Valuer::build(
                &history,
                &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
                &prices,
                &empty_fx(),
                &settings,
                std::slice::from_ref(&account),
            )
            .expect("a valuer")
            .value("atlas", &opus)
        };

        let eur = value_in("EUR");
        assert!(
            eur.subscription.is_some(),
            "a stored price in the settings' currency counts: {:?}",
            eur.subscription_missing
        );

        let chf = value_in("CHF");
        assert_eq!(chf.subscription, None);
        assert_eq!(
            chf.subscription_missing,
            Some(Missing::PriceInOtherCurrency {
                currency: "EUR".to_string()
            }),
            "a stored price keeps the currency it was entered in"
        );
    }

    #[test]
    fn proof_without_a_currency_the_value_says_the_currency_is_missing() {
        let (history, _dir, opus, _haiku, prices) = two_records();
        history
            .append(&weekly_window(
                utc("2026-11-07T00:00:00Z"),
                utc("2026-11-14T00:00:00Z"),
                80.0,
            ))
            .expect("a finished window");
        let mut account = account_with_price(Some(140.0));
        account.price_currency = Some("EUR".to_string());
        let settings = Settings {
            currency: None,
            ..Settings::default()
        };

        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings,
            &[account],
        )
        .expect("a valuer");

        let value = valuer.value("atlas", &opus);
        assert_eq!(
            value.api_missing,
            Some(Missing::NoCurrency),
            "without a currency the API value says the currency is missing"
        );
        assert_eq!(
            value.subscription_missing,
            Some(Missing::NoCurrency),
            "without a currency there is no current currency to enter the price in"
        );
    }

    #[test]
    fn a_week_without_a_reading_says_so() {
        let (history, _dir, opus, _haiku, prices) = two_records();
        history
            .append(&price_changed(
                utc("2026-11-06T00:00:00Z"),
                Some(140.0),
                Some("EUR"),
            ))
            .expect("the account's price");
        let valuer = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings_eur(),
            &[account_with_price(Some(140.0))],
        )
        .expect("a valuer");

        let value = valuer.value("atlas", &opus);
        assert_eq!(value.subscription, None);
        assert_eq!(value.subscription_missing, Some(Missing::NoReading));
    }

    #[test]
    fn every_missing_value_has_the_text_a_page_shows() {
        assert_eq!(
            Missing::NoCurrency.to_string(),
            "set a currency in Settings → General"
        );
        assert_eq!(
            Missing::NoPrice {
                model: "claude-unknown".to_string()
            }
            .to_string(),
            "no price for claude-unknown"
        );
        assert_eq!(
            Missing::NoAccount.to_string(),
            "no account was in use at that time"
        );
        assert_eq!(
            Missing::NoAccountPrice.to_string(),
            "the account has no monthly price"
        );
        assert_eq!(
            Missing::PriceInOtherCurrency {
                currency: "USD".to_string()
            }
            .to_string(),
            "the account's price is in USD; enter it again in the current currency"
        );
        assert_eq!(
            Missing::PriceWithoutCurrency.to_string(),
            "the account's price was entered before a currency was set; enter it again"
        );
        assert_eq!(
            Missing::NoReading.to_string(),
            "no reading of that week's usage"
        );
        assert_eq!(
            Missing::UnpricedInWeek {
                model: "claude-unknown".to_string()
            }
            .to_string(),
            "that week also has tokens of claude-unknown, which has no price"
        );
    }

    #[test]
    fn a_missing_rate_is_named() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let prices = priced_prices(utc("2026-11-01T00:00:00Z"));
        let opus = token(
            utc("2026-11-10T10:00:00Z"),
            "claude-opus-5-5",
            1_000_000,
            0,
            0,
            0,
        );
        let value_of = |settings: &Settings| {
            Valuer::build(
                &history,
                &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
                &prices,
                &empty_fx(),
                settings,
                &[],
            )
            .expect("a valuer")
            .value("atlas", &opus)
        };

        let no_currency = value_of(&Settings::default());
        assert_eq!(
            no_currency.api, None,
            "without a currency there is no amount"
        );
        assert_eq!(
            no_currency.api_missing,
            Some(Missing::NoCurrency),
            "a missing currency is named"
        );
        assert_eq!(
            no_currency.api_currency, None,
            "a missing amount has no currency"
        );

        let with_rate = value_of(&settings_eur());
        assert_eq!(with_rate.api, Some(4.0));
        assert_eq!(with_rate.api_missing, None);
        assert_eq!(with_rate.api_currency.as_deref(), Some("EUR"));
    }

    #[test]
    fn without_a_rate_the_api_value_is_in_usd() {
        let dir = tempdir().expect("tempdir");
        let history = HistoryDir::at(dir.path(), "atlas");
        let prices = priced_prices(utc("2026-11-01T00:00:00Z"));
        let opus = token(
            utc("2026-11-10T10:00:00Z"),
            "claude-opus-5-5",
            1_000_000,
            0,
            0,
            0,
        );
        let settings = Settings {
            currency: Some("CHF".to_string()),
            ..Settings::default()
        };
        let value = Valuer::build(
            &history,
            &range("2026-11-10T00:00:00Z", "2026-11-11T00:00:00Z"),
            &prices,
            &empty_fx(),
            &settings,
            &[],
        )
        .expect("a valuer")
        .value("atlas", &opus);

        assert_eq!(value.api, Some(4.0), "the USD value is shown");
        assert_eq!(value.api_currency.as_deref(), Some("USD"));
        assert_eq!(value.api_missing, None);
    }

    // -----------------------------------------------------------------------
    // a local HTTP server for the two look-ups (never the internet)

    #[derive(Debug)]
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

    struct Reply {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Reply {
        fn ok(body: impl Into<String>) -> Reply {
            Reply {
                status: 200,
                headers: Vec::new(),
                body: body.into(),
            }
        }

        fn status(status: u16) -> Reply {
            Reply {
                status,
                headers: Vec::new(),
                body: String::new(),
            }
        }

        fn header(mut self, name: &str, value: &str) -> Reply {
            self.headers.push((name.to_string(), value.to_string()));
            self
        }
    }

    fn start_server(
        reply: Reply,
    ) -> (
        String,
        std::sync::mpsc::Receiver<Captured>,
        std::thread::JoinHandle<()>,
    ) {
        use std::sync::mpsc;
        use std::thread;

        use tiny_http::{Header, Response, Server, StatusCode};

        let server = Server::http("127.0.0.1:0").expect("a local server");
        let address = server.server_addr().to_ip().expect("a local address");
        let base = format!("http://{address}");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut request = server.recv().expect("one request");
            let mut body = String::new();
            request
                .as_reader()
                .read_to_string(&mut body)
                .expect("a body");
            let captured = Captured {
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
            };
            let mut response =
                Response::from_string(reply.body).with_status_code(StatusCode(reply.status));
            for (name, value) in reply.headers {
                response = response
                    .with_header(Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap());
            }
            request.respond(response).expect("a reply");
            sender.send(captured).expect("the capture");
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

    fn captured(
        receiver: &std::sync::mpsc::Receiver<Captured>,
        handle: std::thread::JoinHandle<()>,
    ) -> Captured {
        let request = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the fake server received no request");
        handle.join().expect("the server thread");
        request
    }

    #[test]
    fn look_up_prices_stores_the_etag_and_the_points() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let now = utc("2026-11-14T09:00:00Z");
        let (base, receiver, handle) =
            start_server(Reply::ok(MODELS_DEV).header("ETag", "W/\"new\""));

        let looked = look_up_prices(&lookup_api(&base), &app, now).expect("the look-up");
        assert!(matches!(
            looked,
            Looked::Taken {
                change: Some(_),
                ..
            }
        ));
        let request = captured(&receiver, handle);
        assert_eq!(request.path, "/api.json");
        assert_eq!(request.header("If-None-Match"), None);

        let stored = Prices::load(&app).expect("prices.json");
        assert_eq!(stored.etag.as_deref(), Some("W/\"new\""));
        assert_eq!(stored.checked_at, Some(now));
        assert_eq!(stored.models.len(), 3);
        assert_eq!(stored.models["anthropic/claude-opus-5-5"].len(), 1);
    }

    #[test]
    fn look_up_prices_saves_nothing_on_a_failure() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let stored_at = utc("2026-11-13T09:00:00Z");
        let mut prices = priced_prices(stored_at);
        prices.checked_at = Some(stored_at);
        prices.etag = Some("W/\"old\"".to_string());
        prices.save(&app).expect("the stored prices");

        let (base, receiver, handle) = start_server(Reply::status(500));
        let error = look_up_prices(&lookup_api(&base), &app, utc("2026-11-14T09:00:00Z"))
            .expect_err("a failed look-up");
        assert!(matches!(error, MoneyError::Api(_)), "got {error:?}");
        let request = captured(&receiver, handle);
        assert_eq!(request.header("If-None-Match"), Some("W/\"old\""));

        let after = Prices::load(&app).expect("prices.json");
        assert_eq!(after.checked_at, Some(stored_at), "checkedAt stays");
        assert_eq!(after.etag.as_deref(), Some("W/\"old\""));
        assert_eq!(after.models, prices.models, "the stored prices stay");
    }

    #[test]
    fn look_up_rate_with_usd_sends_nothing() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        // A port with nothing listening: a request would be a network error.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = listener.local_addr().expect("the address");
        drop(listener);
        let api = lookup_api(&format!("http://{address}"));

        assert!(look_up_rate(&api, &app, "USD")
            .expect("USD needs no request")
            .is_none());
        assert!(!app.fx_file().exists(), "USD writes no fx.json");
    }

    #[test]
    fn look_up_rate_replaces_an_fx_of_another_currency() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let old_rate = 0.8;
        let mut fx = Fx {
            currency: "GBP".to_string(),
            ..Fx::default()
        };
        fx.take(date("2026-11-10"), old_rate);
        fx.save(&app).expect("the old fx.json");

        let body = r#"{"base":"USD","date":"2026-11-13","rates":{"EUR":0.90481}}"#;
        let (base, receiver, handle) = start_server(Reply::ok(body));
        let point = look_up_rate(&lookup_api(&base), &app, "EUR")
            .expect("the rate")
            .expect("a point");
        assert_eq!(point.date, date("2026-11-13"));
        assert_eq!(point.rate, 0.90481);
        let request = captured(&receiver, handle);
        assert_eq!(request.path, "/v1/latest?base=USD&symbols=EUR");

        let stored = Fx::load(&app).expect("fx.json");
        assert_eq!(stored.currency, "EUR");
        assert_eq!(
            stored.points,
            vec![FxPoint {
                date: date("2026-11-13"),
                rate: 0.90481,
            }],
            "the GBP point is gone"
        );
    }

    #[test]
    fn look_up_rate_refuses_a_rate_that_is_not_above_zero() {
        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let body = r#"{"base":"USD","date":"2026-11-13","rates":{"EUR":0}}"#;
        let (base, receiver, handle) = start_server(Reply::ok(body));
        let error = look_up_rate(&lookup_api(&base), &app, "EUR").expect_err("a bad rate");
        assert!(
            error.to_string().contains("EUR") && error.to_string().contains('0'),
            "the error names the rate, got {error}"
        );
        let _ = captured(&receiver, handle);
        assert!(!app.fx_file().exists(), "a bad rate is not saved");
    }

    /// A look-up and the window's own price must not overwrite each other: the
    /// look-up loads `prices.json`, waits for the server and saves the file it
    /// loaded. An own price written in between is lost.
    #[test]
    fn proof_a_refresh_does_not_lose_an_own_price_written_while_it_runs() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        use tiny_http::{Response, Server, StatusCode};

        let dir = tempdir().expect("tempdir");
        let app = AppPaths::at(&dir.path().join("data"));
        let prices = priced_prices(utc("2026-11-13T09:00:00Z"));
        prices.save(&app).expect("the stored prices");

        // A server that answers only when the test says so.
        let server = Server::http("127.0.0.1:0").expect("a local server");
        let address = server.server_addr().to_ip().expect("a local address");
        let base = format!("http://{address}");
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let server_thread = thread::spawn(move || {
            let mut request = server.recv().expect("one request");
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let _ = arrived_tx.send(());
            let _ = go_rx.recv();
            let _ = request
                .respond(Response::from_string(MODELS_DEV).with_status_code(StatusCode(200)));
        });

        // The look-up runs in its own thread: it has read `prices.json` already.
        let (done_tx, done_rx) = mpsc::channel();
        let app_for_thread = app.clone();
        let base_for_thread = base.clone();
        let worker = thread::spawn(move || {
            let result = look_up_prices(
                &lookup_api(&base_for_thread),
                &app_for_thread,
                utc("2026-11-14T09:00:00Z"),
            );
            let _ = done_tx.send(result.is_ok());
        });

        arrived_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the fake server received no request");
        // The window's thread writes an own price while the look-up waits.
        let mut during = Prices::load(&app).expect("prices.json");
        during.overrides.insert(
            "anthropic/claude-opus-5-5".to_string(),
            ModelPrice {
                input: 99.0,
                output: 99.0,
                cache_write: 0.0,
                cache_read: 0.0,
                tiered: false,
            },
        );
        during.save(&app).expect("the own price");
        let _ = go_tx.send(());
        assert!(
            done_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the look-up did not finish"),
            "the look-up failed"
        );
        let _ = worker.join();
        let _ = server_thread.join();

        let after = Prices::load(&app).expect("prices.json");
        assert!(
            after.overrides.contains_key("anthropic/claude-opus-5-5"),
            "the look-up overwrote the own price: {:?}",
            after.overrides
        );
    }
}
