//! The Codex reader (spec §7.2): `auth.json` and Codex's usage answer into a
//! [`ProviderReading`]. Codex's login file is only ever read; no token is
//! refreshed, stored or printed.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::api::{mark_highest_active, Api, ApiError, Limit, LimitKind, Severity, Usage};
use crate::history::AccountRef;
use crate::paths::CodexPaths;

use super::{ProviderError, ProviderReading, CODEX_ORG};

/// Read Codex's login and ask its usage endpoint for the account's limits.
pub fn read(paths: &CodexPaths, api: &Api) -> Result<ProviderReading, ProviderError> {
    let bytes = match std::fs::read(&paths.auth_file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ProviderError::NoLogin {
                path: paths.auth_file.clone(),
            })
        }
        Err(source) => {
            return Err(ProviderError::Io {
                path: paths.auth_file.clone(),
                source,
            })
        }
    };
    let auth: Value = serde_json::from_slice(&bytes)
        .map_err(|error| ProviderError::Shape(format!("auth.json is not JSON: {error}")))?;
    let object = auth
        .as_object()
        .ok_or_else(|| ProviderError::Shape("auth.json is not a JSON object".to_string()))?;
    let tokens = match object.get("tokens") {
        None | Some(Value::Null) => {
            return Err(
                if object.get("auth_mode").and_then(Value::as_str) == Some("chatgpt") {
                    ProviderError::KeyringLogin
                } else {
                    ProviderError::NoLogin {
                        path: paths.auth_file.clone(),
                    }
                },
            )
        }
        Some(Value::Object(tokens)) => tokens,
        Some(_) => {
            return Err(ProviderError::Shape(
                "auth.json's tokens is not an object".to_string(),
            ))
        }
    };
    let access_token = string_field(tokens, "access_token")?;
    let account_id = string_field(tokens, "account_id")?;

    let answer = api
        .codex_usage(access_token, account_id)
        .map_err(|error| match error {
            ApiError::Unauthorized => ProviderError::LoginExpired,
            other => ProviderError::Api(other),
        })?;
    let (usage, plan, account) = usage_from(&answer)?;
    Ok(ProviderReading {
        account,
        plan,
        usage: Some(usage),
        credits: None,
        spend_24h: None,
    })
}

/// A string token field, named by its key only; its value never enters an error.
fn string_field<'a>(tokens: &'a Map<String, Value>, name: &str) -> Result<&'a str, ProviderError> {
    match tokens.get(name).and_then(Value::as_str) {
        None => Err(ProviderError::Shape(format!(
            "auth.json's tokens.{name} is missing or not a string"
        ))),
        Some("") => Err(ProviderError::Shape(format!(
            "auth.json's tokens.{name} is empty"
        ))),
        Some(value) => Ok(value),
    }
}

/// The usage answer's windows as [`Limit`]s, with the plan and the account.
///
/// `rate_limit`'s own windows give [`LimitKind::Session`] (18000 s) and
/// [`LimitKind::WeeklyAll`] (604800 s). An entry of `additional_rate_limits`
/// never gives the account's session: 604800 s is
/// [`LimitKind::WeeklyScoped`], every other length keeps its own label
/// (`Other("5 h limit")` and so on), and the entry's `limit_name` is the
/// limit's model.
pub fn usage_from(answer: &Value) -> Result<(Usage, Option<String>, AccountRef), ProviderError> {
    let object = answer
        .as_object()
        .ok_or_else(|| ProviderError::Shape("the Codex answer is not a JSON object".to_string()))?;

    let mut limits = Vec::new();
    match object.get("rate_limit") {
        Some(Value::Null) => {}
        Some(Value::Object(rate_limit)) => {
            push_windows(rate_limit, "rate_limit", false, None, &mut limits)?;
        }
        None => {
            return Err(ProviderError::Shape(
                "the Codex answer has no rate_limit".to_string(),
            ))
        }
        Some(_) => {
            return Err(ProviderError::Shape(
                "the Codex answer's rate_limit is not an object".to_string(),
            ))
        }
    }
    match object.get("additional_rate_limits") {
        None | Some(Value::Null) => {}
        Some(Value::Array(entries)) => {
            for (index, entry) in entries.iter().enumerate() {
                let path = format!("additional_rate_limits[{index}]");
                let entry = entry
                    .as_object()
                    .ok_or_else(|| ProviderError::Shape(format!("{path} is not a JSON object")))?;
                match entry.get("rate_limit") {
                    Some(Value::Null) => continue,
                    Some(Value::Object(rate_limit)) => {
                        let limit_name = entry
                            .get("limit_name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ProviderError::Shape(format!(
                                    "{path}.limit_name is missing or not a string"
                                ))
                            })?
                            .to_string();
                        push_windows(
                            rate_limit,
                            &format!("{path}.rate_limit"),
                            true,
                            Some(limit_name),
                            &mut limits,
                        )?;
                    }
                    _ => {
                        return Err(ProviderError::Shape(format!(
                            "{path}.rate_limit is missing or not an object"
                        )))
                    }
                }
            }
        }
        Some(_) => {
            return Err(ProviderError::Shape(
                "the Codex answer's additional_rate_limits is not an array".to_string(),
            ))
        }
    }

    let account_id = object
        .get("account_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProviderError::Shape(
                "the Codex answer's account_id is missing or not a string".to_string(),
            )
        })?;
    if account_id.is_empty() {
        return Err(ProviderError::Shape(
            "the Codex answer's account_id is empty".to_string(),
        ));
    }
    let plan = plan_type(object.get("plan_type"))?;

    mark_highest_active(&mut limits);

    Ok((
        Usage {
            limits,
            extra_usage_enabled: false,
        },
        plan,
        AccountRef {
            acc: account_id.to_string(),
            org: CODEX_ORG.to_string(),
        },
    ))
}

fn push_windows(
    rate_limit: &Map<String, Value>,
    path: &str,
    additional: bool,
    model: Option<String>,
    limits: &mut Vec<Limit>,
) -> Result<(), ProviderError> {
    for name in ["primary_window", "secondary_window"] {
        let window_path = format!("{path}.{name}");
        match rate_limit.get(name) {
            None | Some(Value::Null) => {}
            Some(window) => limits.push(limit_from_window(
                window,
                &window_path,
                additional,
                model.clone(),
            )?),
        }
    }
    Ok(())
}

fn limit_from_window(
    window: &Value,
    path: &str,
    additional: bool,
    model: Option<String>,
) -> Result<Limit, ProviderError> {
    let object = window
        .as_object()
        .ok_or_else(|| ProviderError::Shape(format!("{path} is not a JSON object")))?;
    // Codex sends no severity; as for Claude's fallback windows, the load level
    // comes from the percent alone.
    let percent = object
        .get("used_percent")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            ProviderError::Shape(format!("{path}.used_percent is missing or not a number"))
        })?;
    if percent < 0.0 {
        return Err(ProviderError::Shape(format!(
            "{path}.used_percent is negative"
        )));
    }
    let seconds = match object.get("limit_window_seconds") {
        Some(value @ Value::Number(_)) => value.as_i64().ok_or_else(|| {
            ProviderError::Shape(format!("{path}.limit_window_seconds is not a whole number"))
        })?,
        _ => {
            return Err(ProviderError::Shape(format!(
                "{path}.limit_window_seconds is missing or not a number"
            )))
        }
    };
    if seconds <= 0 {
        return Err(ProviderError::Shape(format!(
            "{path}.limit_window_seconds is zero or negative"
        )));
    }
    let resets_at =
        match object.get("reset_at") {
            None | Some(Value::Null) => None,
            Some(Value::Number(_)) => {
                let seconds = object
                    .get("reset_at")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| {
                        ProviderError::Shape(format!("{path}.reset_at is not a whole number"))
                    })?;
                Some(DateTime::<Utc>::from_timestamp(seconds, 0).ok_or_else(|| {
                    ProviderError::Shape(format!("{path}.reset_at is out of range"))
                })?)
            }
            Some(_) => {
                return Err(ProviderError::Shape(format!(
                    "{path}.reset_at is not a number"
                )))
            }
        };
    Ok(Limit {
        kind: window_kind(seconds, additional),
        percent,
        severity: Severity::Normal,
        resets_at,
        model,
        is_active: false,
    })
}

/// Codex's window lengths: `rate_limit`'s own 5 h and week have names; an
/// additional limit's week is scoped to its model, and every other length keeps
/// its own, in whole hours, else whole minutes, else seconds.
fn window_kind(seconds: i64, additional: bool) -> LimitKind {
    match (seconds, additional) {
        (18_000, false) => LimitKind::Session,
        (604_800, false) => LimitKind::WeeklyAll,
        (604_800, true) => LimitKind::WeeklyScoped,
        _ if seconds % 3_600 == 0 => LimitKind::Other(format!("{} h limit", seconds / 3_600)),
        _ if seconds % 60 == 0 => LimitKind::Other(format!("{} min limit", seconds / 60)),
        _ => LimitKind::Other(format!("{seconds} s limit")),
    }
}

fn plan_type(value: Option<&Value>) -> Result<Option<String>, ProviderError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let mut chars = text.chars();
            let Some(first) = chars.next() else {
                return Ok(None);
            };
            Ok(Some(first.to_uppercase().chain(chars).collect()))
        }
        Some(_) => Err(ProviderError::Shape(
            "the Codex answer's plan_type is not a string".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use serde_json::json;
    use tiny_http::{Response, Server, StatusCode};

    const ACCOUNT_ID: &str = "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13";
    const ACCESS_TOKEN: &str = "eyJhbGciOi.eyJzdWIiOiJ1c2VyLXNhbSIsImVtYWlsIjoic2FtQG1haWwuZXhhbXBsZSIsImV4cCI6MTc5NDY0MDAwMH0.7f3a91c2e4b6d8f0a2c4e6b8d0f2a4c6e8b0d2f4a6c8e0b2d4f6a8c0e2b4d6f8";
    const REFRESH_TOKEN: &str = "rt_9f2c5d7e1b3a4c6d8e0f2a4b6c8d0e2f";
    const ID_TOKEN: &str =
        "eyJhbGciOi.eyJzdWIiOiJ1c2VyLXNhbSJ9.1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";

    const USAGE_ANSWER: &str = r#"{
      "user_id": "user-sam",
      "account_id": "0f3a6c2e-7b1d-4e9a-8c5f-2d4b6a8e0c13",
      "email": "sam@mail.example",
      "plan_type": "plus",
      "rate_limit": {
        "allowed": true,
        "limit_reached": false,
        "primary_window": {
          "used_percent": 12,
          "limit_window_seconds": 18000,
          "reset_after_seconds": 17100,
          "reset_at": 1794693195
        },
        "secondary_window": {
          "used_percent": 48,
          "limit_window_seconds": 604800,
          "reset_after_seconds": 316800,
          "reset_at": 1795279995
        }
      },
      "code_review_rate_limit": null,
      "additional_rate_limits": null,
      "credits": {
        "has_credits": false,
        "unlimited": false,
        "overage_limit_reached": false,
        "balance": "0"
      },
      "spend_control": {"reached": false, "individual_limit": null},
      "rate_limit_reached_type": null
    }"#;

    fn answer() -> Value {
        serde_json::from_str(USAGE_ANSWER).expect("the fixture answer is JSON")
    }

    fn auth_json() -> Value {
        json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "last_refresh": "2026-11-14T16:53:03.750022420Z",
            "tokens": {
                "id_token": ID_TOKEN,
                "access_token": ACCESS_TOKEN,
                "refresh_token": REFRESH_TOKEN,
                "account_id": ACCOUNT_ID,
                "login_owner_id": "user-sam",
                "oauth_provenance": "cli"
            }
        })
    }

    fn write_auth(paths: &CodexPaths) {
        std::fs::write(&paths.auth_file, auth_json().to_string()).expect("write auth.json");
    }

    /// The fixture `auth.json` after one edit, in a tempdir of its own.
    fn with_auth(edit: impl FnOnce(&mut Value)) -> (tempfile::TempDir, CodexPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        let mut auth = auth_json();
        edit(&mut auth);
        std::fs::write(&paths.auth_file, auth.to_string()).unwrap();
        (tmp, paths)
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

    fn start_server(reply: Reply) -> (String, Receiver<Captured>, JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let request = server.recv().unwrap();
            let captured = Captured {
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
            };
            let response =
                Response::from_string(reply.body).with_status_code(StatusCode(reply.status));
            request.respond(response).unwrap();
            sender.send(captured).unwrap();
        });
        (base, receiver, handle)
    }

    fn api_for(base: &str) -> Api {
        Api::new().with_codex_url(&format!("{base}/backend-api/wham/usage"))
    }

    /// A server that reports whether anything reached it within a short window.
    fn no_request_server() -> (String, Receiver<bool>, JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base = format!("http://{address}");
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let got = server
                .recv_timeout(std::time::Duration::from_millis(500))
                .unwrap()
                .is_some();
            sender.send(got).unwrap();
        });
        (base, receiver, handle)
    }

    fn saw_no_request(receiver: &Receiver<bool>, handle: JoinHandle<()>) {
        let got = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the counting server did not report");
        handle.join().unwrap();
        assert!(!got, "a request was sent");
    }

    fn closed_api() -> Api {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        Api::new().with_codex_url(&format!("http://{address}/backend-api/wham/usage"))
    }

    fn captured(receiver: &Receiver<Captured>, handle: JoinHandle<()>) -> Captured {
        let request = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the fake server received no request");
        handle.join().unwrap();
        request
    }

    #[test]
    fn the_plus_answer_becomes_two_limits() {
        let (usage, plan, account) = usage_from(&answer()).unwrap();

        assert_eq!(plan.as_deref(), Some("Plus"));
        assert_eq!(
            account,
            AccountRef {
                acc: ACCOUNT_ID.to_string(),
                org: "codex".to_string(),
            }
        );
        assert_eq!(usage.limits.len(), 2);

        let session = usage.session().expect("a session limit");
        assert_eq!(session.kind, LimitKind::Session);
        assert_eq!(session.percent, 12.0);
        assert_eq!(
            session.resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_794_693_195, 0).unwrap())
        );
        assert_eq!(session.severity, Severity::Normal);
        assert!(!session.is_active);

        let weekly = usage.weekly_all().expect("a weekly limit");
        assert_eq!(weekly.percent, 48.0);
        assert_eq!(
            weekly.resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_795_279_995, 0).unwrap())
        );
        assert!(weekly.is_active);
        assert!(!usage.extra_usage_enabled);
    }

    #[test]
    fn two_limits_at_the_same_percent_mark_the_first() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]["used_percent"] = json!(48);

        let (usage, _, _) = usage_from(&answer).unwrap();

        assert_eq!(usage.limits.len(), 2);
        assert!(
            usage.session().expect("a session limit").is_active,
            "the first of equals is the active one"
        );
        assert!(!usage.weekly_all().expect("a weekly limit").is_active);
        assert_eq!(
            usage.binding().map(|limit| &limit.kind),
            Some(&LimitKind::Session)
        );
    }

    #[test]
    fn a_window_of_another_length_keeps_its_length() {
        let mut answer = answer();
        answer["rate_limit"]["secondary_window"]["limit_window_seconds"] = json!(86_400);
        let (usage, _, _) = usage_from(&answer).unwrap();

        let weekly = usage
            .limits
            .iter()
            .find(|limit| limit.kind == LimitKind::Other("24 h limit".to_string()))
            .expect("a 24 h limit");
        assert_eq!(weekly.percent, 48.0);
        assert_eq!(
            weekly.resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_795_279_995, 0).unwrap())
        );
    }

    #[test]
    fn an_additional_limit_carries_its_name() {
        let mut answer = answer();
        answer["additional_rate_limits"] = json!([{
            "limit_name": "gpt-6-astra",
            "metered_feature": "codex",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 30,
                    "limit_window_seconds": 604800,
                    "reset_at": 1795279995
                }
            }
        }]);
        let (usage, _, _) = usage_from(&answer).unwrap();

        let scoped: Vec<_> = usage.weekly_scoped().collect();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].kind, LimitKind::WeeklyScoped);
        assert_eq!(scoped[0].model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(scoped[0].percent, 30.0);
        assert_eq!(
            scoped[0].resets_at,
            Some(DateTime::<Utc>::from_timestamp(1_795_279_995, 0).unwrap())
        );
    }

    #[test]
    fn a_null_window_is_left_out() {
        let mut answer = answer();
        answer["rate_limit"]["secondary_window"] = Value::Null;
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert_eq!(usage.limits.len(), 1);
        assert!(usage.session().is_some());
        assert!(usage.weekly_all().is_none());
    }

    #[test]
    fn a_window_of_whole_minutes_says_minutes() {
        let mut answer = answer();
        answer["rate_limit"]["secondary_window"]["limit_window_seconds"] = json!(5_400);
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert!(usage
            .limits
            .iter()
            .any(|limit| limit.kind == LimitKind::Other("90 min limit".to_string())));
    }

    #[test]
    fn a_missing_used_percent_is_a_shape_error() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]
            .as_object_mut()
            .unwrap()
            .remove("used_percent");

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => {
                assert!(
                    text.contains("rate_limit.primary_window.used_percent"),
                    "{text}"
                );
            }
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn no_auth_file_is_no_login() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());

        match read(&paths, &closed_api()) {
            Err(ProviderError::NoLogin { path }) => assert_eq!(path, paths.auth_file),
            other => panic!("expected NoLogin, got {other:?}"),
        }
    }

    #[test]
    fn an_auth_file_without_tokens_is_a_keyring_login() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        std::fs::write(&paths.auth_file, r#"{"auth_mode":"chatgpt","tokens":null}"#).unwrap();

        match read(&paths, &closed_api()) {
            Err(ProviderError::KeyringLogin) => {}
            other => panic!("expected KeyringLogin, got {other:?}"),
        }
    }

    #[test]
    fn a_401_is_an_expired_login() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        write_auth(&paths);
        let (base, receiver, handle) = start_server(Reply::status(401, "unauthorized"));

        let error = read(&paths, &api_for(&base)).unwrap_err();
        let _ = captured(&receiver, handle);

        assert!(
            matches!(error, ProviderError::LoginExpired),
            "got {error:?}"
        );
    }

    #[test]
    fn the_request_carries_both_headers() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        write_auth(&paths);
        let (base, receiver, handle) = start_server(Reply::ok(USAGE_ANSWER));

        let reading = read(&paths, &api_for(&base)).unwrap();
        let request = captured(&receiver, handle);

        assert_eq!(reading.plan.as_deref(), Some("Plus"));
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/backend-api/wham/usage");
        let authorization = format!("Bearer {ACCESS_TOKEN}");
        assert_eq!(
            request.header("Authorization"),
            Some(authorization.as_str())
        );
        assert_eq!(request.header("chatgpt-account-id"), Some(ACCOUNT_ID));
        assert!(request
            .header("User-Agent")
            .is_some_and(|value| value.starts_with("seatswap/")));
    }

    #[test]
    fn the_token_never_appears_in_an_error() {
        let mut errors = Vec::new();

        let (_expired_dir, expired) = with_auth(|_| {});
        let (base, receiver, handle) = start_server(Reply::status(401, "unauthorized"));
        errors.push(read(&expired, &api_for(&base)).unwrap_err());
        let _ = captured(&receiver, handle);

        let (_server_dir, server) = with_auth(|_| {});
        let (base, receiver, handle) = start_server(Reply::status(500, "down"));
        errors.push(read(&server, &api_for(&base)).unwrap_err());
        let _ = captured(&receiver, handle);

        let (_shape_dir, shape) = with_auth(|_| {});
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]
            .as_object_mut()
            .unwrap()
            .remove("used_percent");
        let (base, receiver, handle) = start_server(Reply::ok(answer.to_string()));
        errors.push(read(&shape, &api_for(&base)).unwrap_err());
        let _ = captured(&receiver, handle);

        let (_bad_header_dir, bad_header) = with_auth(|auth| {
            auth["tokens"]["access_token"] = json!(format!("{ACCESS_TOKEN}\u{e9}"));
        });
        errors.push(read(&bad_header, &closed_api()).unwrap_err());

        assert_eq!(errors.len(), 4);
        for error in &errors {
            for text in [error.to_string(), format!("{error:?}")] {
                assert!(!text.contains(ACCESS_TOKEN), "{text}");
                assert!(!text.contains(REFRESH_TOKEN), "{text}");
                assert!(!text.contains(ID_TOKEN), "{text}");
            }
        }
    }

    #[test]
    fn an_unreadable_auth_file_is_an_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        std::fs::create_dir(&paths.auth_file).unwrap();

        match read(&paths, &closed_api()) {
            Err(ProviderError::Io { path, .. }) => assert_eq!(path, paths.auth_file),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn a_bad_header_character_does_not_leak_the_token() {
        let (_dir, paths) = with_auth(|auth| {
            auth["tokens"]["access_token"] = json!(format!("{ACCESS_TOKEN}\u{e9}"));
        });

        let error = read(&paths, &closed_api()).unwrap_err();
        assert!(
            matches!(error, ProviderError::Api(ApiError::Network(_))),
            "got {error:?}"
        );
        let text = error.to_string();
        let debug = format!("{error:?}");
        assert!(!text.contains(ACCESS_TOKEN), "{text}");
        assert!(!debug.contains(ACCESS_TOKEN), "{debug}");
    }

    #[test]
    fn an_empty_access_token_is_a_shape_error() {
        let (_dir, paths) = with_auth(|auth| auth["tokens"]["access_token"] = json!(""));
        let (base, receiver, handle) = no_request_server();

        match read(&paths, &api_for(&base)) {
            Err(ProviderError::Shape(text)) => assert!(text.contains("access_token"), "{text}"),
            other => panic!("expected Shape, got {other:?}"),
        }
        saw_no_request(&receiver, handle);
    }

    #[test]
    fn an_empty_account_id_is_a_shape_error() {
        let (_dir, paths) = with_auth(|auth| auth["tokens"]["account_id"] = json!(""));
        let (base, receiver, handle) = no_request_server();

        match read(&paths, &api_for(&base)) {
            Err(ProviderError::Shape(text)) => assert!(text.contains("account_id"), "{text}"),
            other => panic!("expected Shape, got {other:?}"),
        }
        saw_no_request(&receiver, handle);
    }

    #[test]
    fn an_empty_answer_account_id_is_a_shape_error() {
        let mut answer = answer();
        answer["account_id"] = json!("");

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => assert!(text.contains("account_id"), "{text}"),
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_or_negative_window_is_a_shape_error() {
        for seconds in [0, -3_600] {
            let mut answer = answer();
            answer["rate_limit"]["primary_window"]["limit_window_seconds"] = json!(seconds);

            match usage_from(&answer) {
                Err(ProviderError::Shape(text)) => assert!(
                    text.contains("rate_limit.primary_window.limit_window_seconds"),
                    "{text}"
                ),
                other => panic!("expected Shape for {seconds}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_negative_percent_is_a_shape_error() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]["used_percent"] = json!(-5);

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => {
                assert!(
                    text.contains("rate_limit.primary_window.used_percent"),
                    "{text}"
                )
            }
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn a_fractional_reset_at_says_whole_number() {
        let mut answer = answer();
        answer["rate_limit"]["primary_window"]["reset_at"] = json!(1_794_693_195.5);

        match usage_from(&answer) {
            Err(ProviderError::Shape(text)) => {
                assert!(text.contains("whole number"), "{text}");
            }
            other => panic!("expected Shape, got {other:?}"),
        }
    }

    #[test]
    fn an_additional_5h_limit_is_not_the_account_session() {
        let mut answer = answer();
        answer["rate_limit"] = Value::Null;
        answer["additional_rate_limits"] = json!([{
            "limit_name": "gpt-6-astra",
            "metered_feature": "codex",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 30,
                    "limit_window_seconds": 18000,
                    "reset_at": 1794693195
                }
            }
        }]);
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert!(usage.session().is_none());
        assert!(usage.weekly_all().is_none());
        assert_eq!(usage.limits.len(), 1);
        assert_eq!(
            usage.limits[0].kind,
            LimitKind::Other("5 h limit".to_string())
        );
        assert_eq!(usage.limits[0].model.as_deref(), Some("gpt-6-astra"));
    }

    #[test]
    fn an_additional_entry_with_a_null_rate_limit_is_left_out() {
        let mut answer = answer();
        answer["additional_rate_limits"] = json!([{
            "limit_name": "gpt-6-astra",
            "metered_feature": "codex",
            "rate_limit": null
        }]);
        let (usage, _, _) = usage_from(&answer).unwrap();

        assert_eq!(usage.limits.len(), 2);
        assert!(usage.session().is_some());
        assert!(usage.weekly_all().is_some());
    }

    #[test]
    fn an_empty_plan_is_none() {
        let mut answer = answer();
        answer["plan_type"] = json!("");
        let (_, plan, _) = usage_from(&answer).unwrap();

        assert_eq!(plan, None);
    }

    #[test]
    fn the_error_texts_are_the_cards_words() {
        assert_eq!(
            ProviderError::NoLogin {
                path: std::path::PathBuf::from("/tmp/codex/auth.json"),
            }
            .to_string(),
            "No Codex login found in /tmp/codex/auth.json. If Codex keeps its login in the system keyring, seatswap cannot read it."
        );
        assert_eq!(
            ProviderError::KeyringLogin.to_string(),
            "Codex keeps its login in the system keyring. seatswap reads the login file only."
        );
        assert_eq!(
            ProviderError::LoginExpired.to_string(),
            "Codex's login has expired. Run codex once to renew it."
        );
        assert_eq!(
            ProviderError::NotRunning {
                url: "http://127.0.0.1:11434".to_string(),
            }
            .to_string(),
            "Ollama is not running on http://127.0.0.1:11434."
        );
    }

    #[test]
    fn auth_json_is_not_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CodexPaths::at(tmp.path());
        write_auth(&paths);
        let before = std::fs::read(&paths.auth_file).unwrap();
        let before_mtime = std::fs::metadata(&paths.auth_file)
            .unwrap()
            .modified()
            .unwrap();

        let (base, receiver, handle) = start_server(Reply::ok(USAGE_ANSWER));
        read(&paths, &api_for(&base)).unwrap();
        let _ = captured(&receiver, handle);

        let after = std::fs::read(&paths.auth_file).unwrap();
        let after_mtime = std::fs::metadata(&paths.auth_file)
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(before_mtime, after_mtime);
    }
}
