//! The Ollama reader (spec §7.1): the local server's balance and its usage over
//! the last 24 h into a [`ProviderReading`], hourly buckets included. A new plan
//! is a monthly dollar allowance, an older one keeps a session and a weekly
//! percent. Nothing of Ollama is written here and no request generates text; the
//! app may write the buckets this reader returns to the history.

use chrono::{DateTime, Timelike, Utc};
use serde_json::{Map, Value};

use crate::api::{mark_highest_active, Api, ApiError, Limit, LimitKind, Severity, Usage};
use crate::history::{AccountRef, OllamaHourRecord};

use super::{Credits, ProviderError, ProviderReading, OLLAMA_ORG};

/// Read Ollama's cloud balance and the spend of the last 24 hours.
///
/// The configured base URL is checked first: a URL the settings refuse never
/// reaches the server. The balance is what the card needs: a failed or
/// incomplete usage answer is recorded in [`ProviderReading::spend_24h`] as an
/// `Err` with its reason, and the reading is still `Ok`. A usage call that
/// itself failed leaves [`ProviderReading::hours`] `None`: there is no answer
/// to read buckets from, and the card's spend error already shows it. The same
/// answer's `buckets` become [`ProviderReading::hours`] one record per hour; an
/// answer whose buckets cannot be read (a bucket not on a whole hour, not
/// exactly one hour long, or carrying no whole number) is an `Err` whose text
/// names the bucket, never a skipped hour and never a 0.
pub fn read(base_url: &str, api: &Api) -> Result<ProviderReading, ProviderError> {
    crate::store::validate_ollama_url(base_url)
        .map_err(|error| ProviderError::Setting(error.to_string()))?;
    let balance = api.ollama_balance(base_url).map_err(|error| match error {
        ApiError::NoConnection(_) => ProviderError::NotRunning {
            url: base_url.to_string(),
        },
        ApiError::Unauthorized => ProviderError::OllamaRefused {
            url: base_url.to_string(),
            status: 401,
        },
        ApiError::Http { status: 403, .. } => ProviderError::OllamaRefused {
            url: base_url.to_string(),
            status: 403,
        },
        other => ProviderError::Api(other),
    })?;
    let (usage, credits) = balance_reading(&balance)?;
    let usage_answer = api.ollama_usage_24h(base_url);
    let (spend_24h, hours) = match &usage_answer {
        Ok(answer) => {
            let spend = match answer
                .get("totals")
                .and_then(|totals| totals.get("usage_usd"))
                .and_then(amount)
            {
                Some(value) => Ok(value),
                None => Err("the usage answer has no number at totals.usage_usd".to_string()),
            };
            let hours =
                hour_records(answer).map_err(|reason| format!("Ollama's usage answer: {reason}"));
            (Some(spend), Some(hours))
        }
        Err(error) => (Some(Err(error.to_string())), None),
    };
    Ok(ProviderReading {
        account: AccountRef {
            acc: OLLAMA_ORG.to_string(),
            org: OLLAMA_ORG.to_string(),
        },
        plan: None,
        usage,
        credits,
        spend_24h,
        hours,
    })
}

/// The usage answer's `buckets` as one record per hour, in the answer's order.
/// One bucket is one hour; anything else is an error naming the bucket.
fn hour_records(answer: &Value) -> Result<Vec<OllamaHourRecord>, String> {
    let buckets = match answer.get("buckets") {
        Some(Value::Array(buckets)) => buckets,
        None => return Err("the usage answer has no buckets".to_string()),
        Some(_) => return Err("the usage answer's buckets is not an array".to_string()),
    };
    buckets
        .iter()
        .enumerate()
        .map(|(index, bucket)| hour_record(bucket, index))
        .collect()
}

fn hour_record(bucket: &Value, index: usize) -> Result<OllamaHourRecord, String> {
    let object = bucket
        .as_object()
        .ok_or_else(|| format!("the usage answer's buckets[{index}] is not an object"))?;
    let hour = bucket_time(object.get("from"), "from", index)?;
    if hour.minute() != 0 || hour.second() != 0 || hour.nanosecond() != 0 {
        return Err(format!(
            "the usage answer's buckets[{index}] starts at {}, not on a whole hour",
            hour.to_rfc3339()
        ));
    }
    let until = bucket_time(object.get("until"), "until", index)?;
    let span = until - hour;
    if span != chrono::Duration::hours(1) {
        return Err(format!(
            "the usage answer's buckets[{index}] runs from {} to {}, not exactly one hour",
            hour.to_rfc3339(),
            until.to_rfc3339()
        ));
    }
    Ok(OllamaHourRecord {
        hour,
        requests: whole_number(object, "request_count", index)?,
        input: whole_number(object, "input_tokens", index)?,
        cached_input: whole_number(object, "cached_input_tokens", index)?,
        output: whole_number(object, "output_tokens", index)?,
        usd: bucket_usd(object.get("usage_usd"), index)?,
    })
}

/// One of a bucket's bounds as an RFC 3339 time.
fn bucket_time(value: Option<&Value>, field: &str, index: usize) -> Result<DateTime<Utc>, String> {
    match value {
        None | Some(Value::Null) => Err(format!(
            "the usage answer's buckets[{index}].{field} is missing"
        )),
        Some(Value::String(text)) => DateTime::parse_from_rfc3339(text)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|error| {
                format!(
                    "the usage answer's buckets[{index}].{field} is not an RFC 3339 time: {error}"
                )
            }),
        Some(_) => Err(format!(
            "the usage answer's buckets[{index}].{field} is not a string"
        )),
    }
}

/// A whole, non-negative count, or an error naming the bucket's field.
fn whole_number(object: &Map<String, Value>, field: &str, index: usize) -> Result<u64, String> {
    match object.get(field) {
        None | Some(Value::Null) => Err(format!(
            "the usage answer's buckets[{index}].{field} is missing"
        )),
        Some(value) => value.as_u64().ok_or_else(|| {
            format!(
                "the usage answer's buckets[{index}].{field} is not a whole non-negative number"
            )
        }),
    }
}

/// A bucket's dollars: a finite number of at least 0, or an error naming it.
fn bucket_usd(value: Option<&Value>, index: usize) -> Result<f64, String> {
    match value {
        None | Some(Value::Null) => Err(format!(
            "the usage answer's buckets[{index}].usage_usd is missing"
        )),
        Some(value) => match amount(value) {
            Some(usd) if usd.is_finite() && usd >= 0.0 => Ok(usd),
            _ => Err(format!(
                "the usage answer's buckets[{index}].usage_usd is not a finite number of at least 0"
            )),
        },
    }
}

/// A JSON number, whole or fractional; anything else is `None`.
pub fn amount(value: &Value) -> Option<f64> {
    value.as_f64()
}

/// A balance answer into the usage limits and the credits it carries.
fn balance_reading(balance: &Value) -> Result<(Option<Usage>, Option<Credits>), ProviderError> {
    let object = balance
        .as_object()
        .ok_or_else(|| ProviderError::Shape("the Ollama balance answer is not an object".into()))?;
    let included = match object.get("included") {
        Some(Value::Object(included)) => included,
        None => {
            return Err(ProviderError::Shape(
                "the Ollama balance answer has no included".to_string(),
            ))
        }
        Some(_) => {
            return Err(ProviderError::Shape(
                "the Ollama balance answer's included is not an object".to_string(),
            ))
        }
    };
    if included.contains_key("balance_usd") {
        if included.contains_key("session") || included.contains_key("weekly") {
            return Err(ProviderError::Shape(
                "the Ollama balance answer has both credits and session limits".to_string(),
            ));
        }
        new_plan(object, included)
    } else if included.contains_key("session") || included.contains_key("weekly") {
        old_plan(included)
    } else {
        Err(ProviderError::Shape(
            "the Ollama balance answer has neither credits nor session limits".to_string(),
        ))
    }
}

/// A new plan: the included credits and the monthly dollar allowance.
fn new_plan(
    object: &Map<String, Value>,
    included: &Map<String, Value>,
) -> Result<(Option<Usage>, Option<Credits>), ProviderError> {
    let left = required_amount(included, "balance_usd", "included.balance_usd")?;
    if left < 0.0 {
        return Err(ProviderError::Shape(
            "included.balance_usd is negative".to_string(),
        ));
    }
    let allowance = optional_amount(included, "allowance_usd", "included.allowance_usd")?;
    if let Some(allowance) = allowance {
        if allowance < 0.0 {
            return Err(ProviderError::Shape(
                "included.allowance_usd is negative".to_string(),
            ));
        }
        if left > allowance {
            return Err(ProviderError::Shape(format!(
                "included.balance_usd {left} is above included.allowance_usd {allowance}"
            )));
        }
    }
    let until = match included.get("period") {
        None | Some(Value::Null) => None,
        Some(Value::Object(period)) => time(period.get("until"), "included.period.until")?,
        Some(_) => {
            return Err(ProviderError::Shape(
                "included.period is not an object".to_string(),
            ))
        }
    };
    let purchased = match object.get("purchased") {
        None | Some(Value::Null) => None,
        Some(Value::Object(purchased)) => {
            optional_amount(purchased, "balance_usd", "purchased.balance_usd")?
        }
        Some(_) => {
            return Err(ProviderError::Shape(
                "purchased is not an object".to_string(),
            ))
        }
    };

    // Without a known allowance the share used is undefined, so there is no
    // limit to record; the credits are still shown. A zero allowance has no
    // share either (a balance above one is refused above), and dividing by it
    // would be meaningless.
    let usage = match allowance {
        Some(allowance) if allowance != 0.0 => {
            let percent = (allowance - left) / allowance * 100.0;
            Some(Usage {
                limits: vec![Limit {
                    kind: LimitKind::Other("monthly_credits".to_string()),
                    percent,
                    severity: Severity::Normal,
                    resets_at: until,
                    model: None,
                    is_active: true,
                }],
                extra_usage_enabled: false,
                warnings: Vec::new(),
            })
        }
        _ => None,
    };
    Ok((
        usage,
        Some(Credits {
            left,
            allowance,
            until,
            purchased,
        }),
    ))
}

/// An old plan: a session and a weekly limit, as a Claude account has them.
fn old_plan(
    included: &Map<String, Value>,
) -> Result<(Option<Usage>, Option<Credits>), ProviderError> {
    let mut limits = Vec::new();
    for (name, kind) in [
        ("session", LimitKind::Session),
        ("weekly", LimitKind::WeeklyAll),
    ] {
        match included.get(name) {
            None | Some(Value::Null) => {}
            Some(window) => limits.push(percent_limit(window, &format!("included.{name}"), kind)?),
        }
    }
    if limits.is_empty() {
        return Err(ProviderError::Shape(
            "the Ollama balance answer's session and weekly are null".to_string(),
        ));
    }
    mark_highest_active(&mut limits);
    Ok((
        Some(Usage {
            limits,
            extra_usage_enabled: false,
            warnings: Vec::new(),
        }),
        None,
    ))
}

/// An old plan's window: `remaining_percent` becomes the used percent.
fn percent_limit(window: &Value, path: &str, kind: LimitKind) -> Result<Limit, ProviderError> {
    let object = window
        .as_object()
        .ok_or_else(|| ProviderError::Shape(format!("{path} is not an object")))?;
    let remaining = required_amount(
        object,
        "remaining_percent",
        &format!("{path}.remaining_percent"),
    )?;
    if !(0.0..=100.0).contains(&remaining) {
        return Err(ProviderError::Shape(format!(
            "{path}.remaining_percent is outside 0..=100"
        )));
    }
    Ok(Limit {
        kind,
        percent: 100.0 - remaining,
        severity: Severity::Normal,
        resets_at: time(object.get("resets_at"), &format!("{path}.resets_at"))?,
        model: None,
        is_active: false,
    })
}

fn required_amount(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<f64, ProviderError> {
    match object.get(key) {
        None | Some(Value::Null) => Err(ProviderError::Shape(format!("{path} is missing"))),
        Some(value) => {
            amount(value).ok_or_else(|| ProviderError::Shape(format!("{path} is not a number")))
        }
    }
}

fn optional_amount(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<f64>, ProviderError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => amount(value)
            .map(Some)
            .ok_or_else(|| ProviderError::Shape(format!("{path} is not a number"))),
    }
}

/// A reset time given as RFC 3339 text.
fn time(value: Option<&Value>, path: &str) -> Result<Option<DateTime<Utc>>, ProviderError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => DateTime::parse_from_rfc3339(text)
            .map(|time| Some(time.with_timezone(&Utc)))
            .map_err(|error| {
                ProviderError::Shape(format!("{path} is not an RFC 3339 time: {error}"))
            }),
        Some(_) => Err(ProviderError::Shape(format!("{path} is not a string"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use serde_json::json;
    use tempfile::tempdir;
    use tiny_http::{Response, Server, StatusCode};

    use crate::history::{HistoryDir, Recorder};

    const NEW_PLAN_BALANCE: &str = r#"{
      "included": {
        "balance_usd": 212.4,
        "allowance_usd": 300,
        "period": {
          "from": "2026-11-03T00:00:00Z",
          "until": "2026-12-03T00:00:00Z"
        }
      },
      "purchased": {"balance_usd": 0}
    }"#;

    const OLD_PLAN_BALANCE: &str = r#"{
      "included": {
        "session": {"remaining_percent": 88, "resets_at": "2026-11-14T19:30:00Z"},
        "weekly": {"remaining_percent": 52, "resets_at": "2026-11-16T02:00:00Z"}
      }
    }"#;

    const USAGE_ANSWER: &str = r#"{
      "totals": {
        "request_count": 412,
        "usage_usd": 3.18,
        "input_tokens": 5120000,
        "cached_input_tokens": 3900000,
        "output_tokens": 640000
      },
      "buckets": [
        {"from": "2026-11-13T00:00:00Z", "until": "2026-11-13T01:00:00Z",
         "request_count": 17, "usage_usd": 0.12, "input_tokens": 210000,
         "cached_input_tokens": 160000, "output_tokens": 26000}
      ]
    }"#;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().expect("an RFC 3339 time")
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
    }

    impl Captured {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    fn capture(request: &mut tiny_http::Request) -> Captured {
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).unwrap();
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
        }
    }

    /// A fake Ollama that answers the balance and then the 24 h usage.
    fn start_ollama(
        balance: Reply,
        usage: Reply,
    ) -> (String, Receiver<Vec<Captured>>, JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let mut request = server.recv().unwrap();
                let reply = if request.url().contains("/api/balance") {
                    &balance
                } else {
                    &usage
                };
                requests.push(capture(&mut request));
                let response = Response::from_string(reply.body.clone())
                    .with_status_code(StatusCode(reply.status));
                request.respond(response).unwrap();
            }
            sender.send(requests).unwrap();
        });
        (base, receiver, handle)
    }

    /// A fake Ollama that answers one request and stops.
    fn start_one(reply: Reply) -> (String, Receiver<Captured>, JoinHandle<()>) {
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

    fn captured(receiver: &Receiver<Vec<Captured>>, handle: JoinHandle<()>) -> Vec<Captured> {
        let requests = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the fake Ollama received no request");
        handle.join().unwrap();
        requests
    }

    /// A port nothing listens on.
    fn closed_base() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    }

    /// One balance answer on a fake Ollama that serves a single request.
    fn read_one_reply(balance: &str) -> Result<ProviderReading, ProviderError> {
        let (base, receiver, handle) = start_one(Reply::ok(balance));
        let reading = read(&base, &Api::new());
        let _ = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the fake Ollama received no request");
        handle.join().unwrap();
        reading
    }

    #[test]
    fn credits_from_the_new_plan() {
        let (base, receiver, handle) =
            start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(USAGE_ANSWER));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(
            reading.credits,
            Some(Credits {
                left: 212.4,
                allowance: Some(300.0),
                until: Some(at("2026-12-03T00:00:00Z")),
                purchased: Some(0.0),
            })
        );
        let usage = reading.usage.expect("a monthly credits limit");
        assert_eq!(usage.limits.len(), 1);
        let limit = &usage.limits[0];
        assert_eq!(limit.kind, LimitKind::Other("monthly_credits".to_string()));
        assert!((limit.percent - 29.2).abs() < 1e-9, "{}", limit.percent);
        assert_eq!(limit.severity, Severity::Normal);
        assert_eq!(limit.resets_at, Some(at("2026-12-03T00:00:00Z")));
        assert_eq!(limit.model, None);
        assert!(limit.is_active);
        assert!(!usage.extra_usage_enabled);
        assert_eq!(reading.spend_24h, Some(Ok(3.18)));
        assert_eq!(reading.plan, None);
        assert_eq!(
            reading.account,
            AccountRef {
                acc: "ollama".to_string(),
                org: "ollama".to_string(),
            }
        );
    }

    #[test]
    fn buckets_become_hour_records() {
        let (base, receiver, handle) =
            start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(USAGE_ANSWER));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(
            reading.hours,
            Some(Ok(vec![OllamaHourRecord {
                hour: at("2026-11-13T00:00:00Z"),
                requests: 17,
                input: 210000,
                cached_input: 160000,
                output: 26000,
                usd: 0.12,
            }]))
        );
    }

    /// A bucket that runs longer than one hour is not an hour's record: the
    /// reading keeps its balance and spend, and the error names the bucket.
    #[test]
    fn a_bucket_longer_than_an_hour_is_an_error_naming_it() {
        let body = r#"{
          "totals": {"request_count": 17, "usage_usd": 0.12},
          "buckets": [
            {"from": "2026-11-13T00:00:00Z", "until": "2026-11-13T02:00:00Z",
             "request_count": 17, "usage_usd": 0.12, "input_tokens": 210000,
             "cached_input_tokens": 160000, "output_tokens": 26000}
          ]
        }"#;
        let (base, receiver, handle) = start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(body));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(reading.spend_24h, Some(Ok(0.12)));
        assert!(reading.credits.is_some(), "the balance is still read");
        match reading.hours {
            Some(Err(text)) => {
                assert!(text.contains("buckets[0]"), "{text}");
                assert!(text.contains("not exactly one hour"), "{text}");
            }
            other => panic!("expected an error naming the bucket, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_usage_call_reads_no_hours() {
        let (base, receiver, handle) = start_ollama(
            Reply::ok(NEW_PLAN_BALANCE),
            Reply::status(500, "internal error"),
        );
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(reading.spend_24h, Some(Err("server error 500".to_string())));
        assert_eq!(
            reading.hours, None,
            "there is no answer to read buckets from"
        );
    }

    #[test]
    fn a_usage_answer_without_buckets_is_an_error() {
        let (base, receiver, handle) = start_ollama(
            Reply::ok(NEW_PLAN_BALANCE),
            Reply::ok(r#"{"totals":{"usage_usd":0.12}}"#),
        );
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(reading.spend_24h, Some(Ok(0.12)));
        match reading.hours {
            Some(Err(text)) => {
                assert!(text.contains("Ollama's usage answer"), "{text}");
                assert!(text.contains("no buckets"), "{text}");
            }
            other => panic!("expected an error naming the buckets, got {other:?}"),
        }
    }

    #[test]
    fn buckets_that_are_not_an_array_are_an_error() {
        let (base, receiver, handle) = start_ollama(
            Reply::ok(NEW_PLAN_BALANCE),
            Reply::ok(r#"{"totals":{"usage_usd":0.12},"buckets":{}}"#),
        );
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        match reading.hours {
            Some(Err(text)) => {
                assert!(text.contains("Ollama's usage answer"), "{text}");
                assert!(text.contains("not an array"), "{text}");
            }
            other => panic!("expected an error naming the buckets, got {other:?}"),
        }
    }

    #[test]
    fn no_buckets_is_an_empty_list_and_writes_nothing() {
        let (base, receiver, handle) = start_ollama(
            Reply::ok(NEW_PLAN_BALANCE),
            Reply::ok(r#"{"totals":{"usage_usd":0.12},"buckets":[]}"#),
        );
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(reading.hours, Some(Ok(Vec::new())));
        let dir = tempdir().expect("tempdir");
        let mut recorder = Recorder::new(HistoryDir::at(dir.path(), "atlas"));
        assert_eq!(recorder.ollama_hours(&[]).unwrap(), 0);
        assert!(!dir.path().join("atlas/ollama-2026-11.jsonl").exists());
    }

    #[test]
    fn a_fractional_count_is_an_error_naming_the_bucket() {
        let body = r#"{
          "totals": {"usage_usd": 0.12},
          "buckets": [
            {"from": "2026-11-13T00:00:00Z", "until": "2026-11-13T01:00:00Z",
             "request_count": 17.5, "usage_usd": 0.12, "input_tokens": 210000,
             "cached_input_tokens": 160000, "output_tokens": 26000}
          ]
        }"#;
        let (base, receiver, handle) = start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(body));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        match reading.hours {
            Some(Err(text)) => {
                assert!(text.contains("buckets[0]"), "{text}");
                assert!(text.contains("request_count"), "{text}");
            }
            other => panic!("expected an error naming the bucket, got {other:?}"),
        }
    }

    #[test]
    fn a_negative_usage_usd_is_an_error_naming_the_bucket() {
        let body = r#"{
          "totals": {"usage_usd": 0.12},
          "buckets": [
            {"from": "2026-11-13T00:00:00Z", "until": "2026-11-13T01:00:00Z",
             "request_count": 17, "usage_usd": -0.01, "input_tokens": 210000,
             "cached_input_tokens": 160000, "output_tokens": 26000}
          ]
        }"#;
        let (base, receiver, handle) = start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(body));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        match reading.hours {
            Some(Err(text)) => {
                assert!(text.contains("buckets[0]"), "{text}");
                assert!(text.contains("usage_usd"), "{text}");
            }
            other => panic!("expected an error naming the bucket, got {other:?}"),
        }
    }

    #[test]
    fn a_bucket_not_on_a_whole_hour_is_an_error_naming_it() {
        let body = r#"{
          "totals": {"usage_usd": 0.12},
          "buckets": [
            {"from": "2026-11-13T00:30:00Z", "until": "2026-11-13T01:30:00Z",
             "request_count": 17, "usage_usd": 0.12, "input_tokens": 210000,
             "cached_input_tokens": 160000, "output_tokens": 26000}
          ]
        }"#;
        let (base, receiver, handle) = start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(body));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        match reading.hours {
            Some(Err(text)) => {
                assert!(text.contains("buckets[0]"), "{text}");
                assert!(text.contains("not on a whole hour"), "{text}");
            }
            other => panic!("expected an error naming the bucket, got {other:?}"),
        }
    }

    #[test]
    fn amounts_are_numbers_whole_or_fractional() {
        assert_eq!(
            amount(&json!({"allowance_usd": 300})["allowance_usd"]),
            Some(300.0)
        );
        assert_eq!(
            amount(&json!({"balance_usd": 212.4})["balance_usd"]),
            Some(212.4)
        );
        assert_eq!(
            amount(&json!({"balance_usd": "212.4"})["balance_usd"]),
            None
        );

        let balance = r#"{
          "included": {
            "balance_usd": "212.4",
            "allowance_usd": 300,
            "period": {"until": "2026-12-03T00:00:00Z"}
          },
          "purchased": {"balance_usd": 0}
        }"#;
        let (base, receiver, handle) = start_one(Reply::ok(balance));
        match read(&base, &Api::new()) {
            Err(ProviderError::Shape(text)) => {
                assert!(text.contains("included.balance_usd"), "{text}")
            }
            other => panic!("expected Shape, got {other:?}"),
        }
        let _ = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the fake Ollama received no request");
        handle.join().unwrap();
    }

    #[test]
    fn an_old_plan_becomes_two_limits() {
        let (base, receiver, handle) =
            start_ollama(Reply::ok(OLD_PLAN_BALANCE), Reply::ok(USAGE_ANSWER));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(reading.credits, None);
        let usage = reading.usage.expect("two limits");
        assert_eq!(usage.limits.len(), 2);

        let session = usage.session().expect("a session limit");
        assert_eq!(session.percent, 12.0);
        assert_eq!(session.resets_at, Some(at("2026-11-14T19:30:00Z")));
        assert!(!session.is_active);

        let weekly = usage.weekly_all().expect("a weekly limit");
        assert_eq!(weekly.percent, 48.0);
        assert_eq!(weekly.resets_at, Some(at("2026-11-16T02:00:00Z")));
        assert!(weekly.is_active);

        assert_eq!(reading.spend_24h, Some(Ok(3.18)));
    }

    #[test]
    fn a_refused_connection_is_not_running() {
        let base = closed_base();
        match read(&base, &Api::new()) {
            Err(ProviderError::NotRunning { url }) => assert_eq!(url, base),
            other => panic!("expected NotRunning, got {other:?}"),
        }
    }

    #[test]
    fn usage_failing_does_not_fail_the_balance() {
        let (base, receiver, handle) = start_ollama(
            Reply::ok(NEW_PLAN_BALANCE),
            Reply::status(500, "internal error"),
        );
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(reading.spend_24h, Some(Err("server error 500".to_string())));
        assert_eq!(
            reading.credits,
            Some(Credits {
                left: 212.4,
                allowance: Some(300.0),
                until: Some(at("2026-12-03T00:00:00Z")),
                purchased: Some(0.0),
            })
        );
    }

    #[test]
    fn a_usage_answer_without_totals_says_so() {
        let (base, receiver, handle) =
            start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(r#"{"buckets": []}"#));
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(
            reading.spend_24h,
            Some(Err(
                "the usage answer has no number at totals.usage_usd".to_string()
            ))
        );
        assert!(reading.credits.is_some(), "the balance is still read");
    }

    #[test]
    fn a_usage_answer_without_a_number_at_usage_usd_says_so() {
        let (base, receiver, handle) = start_ollama(
            Reply::ok(NEW_PLAN_BALANCE),
            Reply::ok(r#"{"totals":{"usage_usd":"3.18"}}"#),
        );
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(
            reading.spend_24h,
            Some(Err(
                "the usage answer has no number at totals.usage_usd".to_string()
            ))
        );
    }

    #[test]
    fn a_refused_ollama_request_names_no_token() {
        for status in [401, 403] {
            let (base, receiver, handle) = start_one(Reply::status(status, "no"));
            let error = read(&base, &Api::new()).unwrap_err();
            let request = receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the fake Ollama received no request");
            handle.join().unwrap();

            assert_eq!(request.path, "/api/balance");
            match &error {
                ProviderError::OllamaRefused { url, status: got } => {
                    assert_eq!(url, &base);
                    assert_eq!(*got, status);
                }
                other => panic!("expected OllamaRefused for {status}, got {other:?}"),
            }
            assert_eq!(
                error.to_string(),
                format!("Ollama on {base} refused the request (HTTP {status}). Is it signed in?")
            );
        }
    }

    #[test]
    fn only_get_requests_are_sent() {
        let (base, receiver, handle) =
            start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(USAGE_ANSWER));
        let _ = read(&base, &Api::new()).unwrap();
        let requests = captured(&receiver, handle);

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/balance");
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].path, "/api/usage?range=24h");
        for request in &requests {
            assert!(request
                .header("User-Agent")
                .is_some_and(|value| value.starts_with("seatswap/")));
        }
    }

    #[test]
    fn a_trailing_slash_gives_one_slash() {
        let (base, receiver, handle) =
            start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(USAGE_ANSWER));
        let _ = read(&format!("{base}/"), &Api::new()).unwrap();
        let requests = captured(&receiver, handle);

        assert_eq!(requests[0].path, "/api/balance");
        assert_eq!(requests[1].path, "/api/usage?range=24h");
    }

    #[test]
    fn a_base_path_is_kept() {
        let (base, receiver, handle) =
            start_ollama(Reply::ok(NEW_PLAN_BALANCE), Reply::ok(USAGE_ANSWER));
        let _ = read(&format!("{base}/base"), &Api::new()).unwrap();
        let requests = captured(&receiver, handle);

        assert_eq!(requests[0].path, "/base/api/balance");
        assert_eq!(requests[1].path, "/base/api/usage?range=24h");
    }

    // ---- proof tests of the review ----

    /// A fake Ollama that answers every request with `balance` (the 24 h usage
    /// is answered with the fixture) and reports the path of each.
    fn start_recorder(balance: &str) -> (String, Receiver<String>, JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let balance = balance.to_string();
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            while let Ok(Some(request)) = server.recv_timeout(std::time::Duration::from_millis(500))
            {
                let path = request.url().to_string();
                let body = if path.contains("/api/balance") {
                    balance.clone()
                } else {
                    USAGE_ANSWER.to_string()
                };
                let response = Response::from_string(body).with_status_code(StatusCode(200));
                request.respond(response).unwrap();
                if sender.send(path).is_err() {
                    break;
                }
            }
        });
        (base, receiver, handle)
    }

    /// The paths the fake Ollama saw, in the order it saw them.
    fn paths_seen(receiver: &Receiver<String>, handle: JoinHandle<()>) -> Vec<String> {
        let mut paths = Vec::new();
        while let Ok(path) = receiver.recv_timeout(std::time::Duration::from_millis(200)) {
            paths.push(path);
        }
        handle.join().unwrap();
        paths
    }

    /// A new plan whose numbers do not allow a share: a balance above the
    /// allowance, a negative balance, a negative allowance. The reader must
    /// not show a percent outside 0..=100, the range the numbers allow.
    #[test]
    fn a_new_plan_with_impossible_amounts_has_no_made_up_percent() {
        for (balance, allowance) in [("400", "300"), ("-50", "300"), ("212.4", "-300")] {
            let body = format!(
                r#"{{"included":{{"balance_usd":{balance},"allowance_usd":{allowance}}}}}"#
            );
            let (base, _receiver, handle) = start_recorder(&body);
            let reading = read(&base, &Api::new());
            handle.join().unwrap();

            match reading {
                Ok(reading) => {
                    for limit in reading.usage.into_iter().flat_map(|usage| usage.limits) {
                        assert!(
                            (0.0..=100.0).contains(&limit.percent),
                            "a balance of {balance} of an allowance of {allowance} claims {} %",
                            limit.percent
                        );
                    }
                }
                Err(ProviderError::Shape(_)) => {}
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
    }

    /// An old plan whose `remaining_percent` is outside 0..=100: the percent
    /// the reader shows must stay inside the range the numbers allow.
    #[test]
    fn an_old_plan_with_an_impossible_remaining_percent_has_no_made_up_percent() {
        for remaining in ["120", "-5"] {
            let body = format!(
                r#"{{"included":{{"session":{{"remaining_percent":{remaining},"resets_at":"2026-11-14T19:30:00Z"}}}}}}"#
            );
            let (base, _receiver, handle) = start_recorder(&body);
            let reading = read(&base, &Api::new());
            handle.join().unwrap();

            match reading {
                Ok(reading) => {
                    for limit in reading.usage.into_iter().flat_map(|usage| usage.limits) {
                        assert!(
                            (0.0..=100.0).contains(&limit.percent),
                            "a remaining percent of {remaining} claims {} %",
                            limit.percent
                        );
                    }
                }
                Err(ProviderError::Shape(_)) => {}
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
    }

    /// A base URL with two slashes is one the settings refuse; the reader must
    /// report the setting and send nothing.
    #[test]
    fn a_base_with_two_slashes_is_refused() {
        let (base, receiver, handle) = start_recorder(NEW_PLAN_BALANCE);
        let reading = read(&format!("{base}//"), &Api::new());
        let paths = paths_seen(&receiver, handle);

        match reading {
            Err(ProviderError::Setting(text)) => assert!(text.contains("ollamaUrl"), "{text}"),
            other => panic!("expected Setting, got {other:?}"),
        }
        assert!(paths.is_empty(), "the reader asked {paths:?}");
    }

    /// A base URL with a query string is one the settings refuse; the reader
    /// must report the setting and send nothing.
    #[test]
    fn a_base_with_a_query_string_is_refused() {
        let (base, receiver, handle) = start_recorder(NEW_PLAN_BALANCE);
        let reading = read(&format!("{base}?x=1"), &Api::new());
        let paths = paths_seen(&receiver, handle);

        match reading {
            Err(ProviderError::Setting(text)) => assert!(text.contains("ollamaUrl"), "{text}"),
            other => panic!("expected Setting, got {other:?}"),
        }
        assert!(paths.is_empty(), "the reader asked {paths:?}");
    }

    /// An amount outside what the plan allows names its own path, so the card
    /// can say which number is wrong.
    #[test]
    fn impossible_amounts_are_shape_errors_naming_their_path() {
        for (balance, path) in [
            (
                r#"{"included":{"balance_usd":400,"allowance_usd":300}}"#,
                "included.balance_usd",
            ),
            (
                r#"{"included":{"balance_usd":-50,"allowance_usd":300}}"#,
                "included.balance_usd",
            ),
            (
                r#"{"included":{"balance_usd":212.4,"allowance_usd":-300}}"#,
                "included.allowance_usd",
            ),
            (
                r#"{"included":{"session":{"remaining_percent":120,"resets_at":"2026-11-14T19:30:00Z"}}}"#,
                "included.session.remaining_percent",
            ),
            (
                r#"{"included":{"weekly":{"remaining_percent":-5,"resets_at":"2026-11-16T02:00:00Z"}}}"#,
                "included.weekly.remaining_percent",
            ),
        ] {
            match read_one_reply(balance) {
                Err(ProviderError::Shape(text)) => {
                    assert!(text.contains(path), "{balance}: {text}")
                }
                other => panic!("expected Shape for {balance}, got {other:?}"),
            }
        }
    }

    /// A zero allowance leaves the share undefined: the credits are shown and
    /// there is no limit.
    #[test]
    fn a_zero_allowance_shows_the_credits_without_a_limit() {
        let (base, receiver, handle) = start_ollama(
            Reply::ok(r#"{"included":{"balance_usd":0,"allowance_usd":0}}"#),
            Reply::ok(USAGE_ANSWER),
        );
        let reading = read(&base, &Api::new()).unwrap();
        let _ = captured(&receiver, handle);

        assert_eq!(reading.usage, None);
        assert_eq!(
            reading.credits,
            Some(Credits {
                left: 0.0,
                allowance: Some(0.0),
                until: None,
                purchased: None,
            })
        );
    }

    /// An answer that carries both plan shapes at once is neither plan.
    #[test]
    fn both_plan_shapes_at_once_is_a_shape_error() {
        let reading = read_one_reply(
            r#"{"included":{"balance_usd":212.4,"allowance_usd":300,"session":{"remaining_percent":88,"resets_at":"2026-11-14T19:30:00Z"}}}"#,
        );

        match reading {
            Err(ProviderError::Shape(text)) => assert_eq!(
                text,
                "the Ollama balance answer has both credits and session limits"
            ),
            other => panic!("expected Shape, got {other:?}"),
        }
    }
}
