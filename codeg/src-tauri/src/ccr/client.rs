//! [crewchat] CCR management **transport** — the one place that knows how to
//! reach CCR's management server.
//!
//! In this fork Codeg does not own model management: the single source of truth
//! is CCR (vendored at `ccr/`), reachable through its local management server.
//! Everything Codeg does with models — the settings UI, the Agent Host launch
//! recipe, the roster brain resolution, and the Phase 5.1 supervisor — goes
//! through this module, so the URL/token contract has exactly one owner.
//!
//! Environment:
//! - `CCR_WEB_URL`        — management base URL (overrides discovery)
//! - `CCR_WEB_AUTH_TOKEN` — management auth token (overrides discovery)
//!
//! Both have a DISCOVERY fallback so a double-clicked desktop app works with no
//! environment at all: `ccr start` writes `<ccr home>/service.json` whose `url`
//! carries the web token as `?ccr_web_token=…`, and that file is the same one
//! the Phase 5.1 supervisor already trusts for liveness. Env still wins (CI, a
//! remote management URL, a deliberately overridden token).
//!
//! `parse_web_addr` / `is_loopback_host` are pure so the supervisor's
//! "may I start a local instance?" decision is unit-testable, and
//! [`parse_service_file`] is pure so discovery is testable without a filesystem.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::app_error::AppCommandError;

pub const DEFAULT_CCR_WEB_URL: &str = "http://127.0.0.1:3458";
const RPC_TIMEOUT_SECS: u64 = 30;

/// CCR's own home directory (where `ccr start` writes `service.json`).
const CCR_HOME_DIR: &str = ".claude-code-router";
const SERVICE_FILE: &str = "service.json";
/// The query parameter CCR puts its web token in, inside `service.json`'s `url`.
const WEB_TOKEN_PARAM: &str = "ccr_web_token";

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// One management endpoint: a base URL plus the token to use with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcrEndpoint {
    pub base_url: String,
    pub token: Option<String>,
}

/// Pure: what one `service.json` payload says about the management endpoint.
///
/// `None` when the payload is not an object or carries no usable `url`. The
/// token is taken ONLY from the `ccr_web_token` query parameter (or a flat
/// `ccr_web_token` field on versions that write it that way): CCR's
/// `serviceToken` is a different thing (a service identity, compared against
/// `CCR_SERVICE_INSTANCE_TOKEN`) and using it as the web token would 401.
pub fn parse_service_file(raw: &str) -> Option<CcrEndpoint> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    let url = object.get("url")?.as_str()?.trim();
    if url.is_empty() {
        return None;
    }
    // The token lives in the URL's query string; strip it so the base URL stays
    // a base URL.
    let mut token = None;
    let mut base = url.to_string();
    if let Ok(mut parsed) = reqwest::Url::parse(url) {
        for (key, value) in parsed.query_pairs() {
            if key == WEB_TOKEN_PARAM && !value.trim().is_empty() {
                token = Some(value.trim().to_string());
            }
        }
        parsed.set_query(None);
        base = parsed.as_str().trim_end_matches('/').to_string();
    }
    if token.is_none() {
        token = object
            .get(WEB_TOKEN_PARAM)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    Some(CcrEndpoint {
        base_url: base,
        token,
    })
}

/// Read CCR's `service.json` and parse it. Best effort: any IO/parse failure is
/// `None` (the caller falls back to the default URL and "no token").
pub fn discovered_endpoint() -> Option<CcrEndpoint> {
    let home = dirs::home_dir()?;
    read_service_file(&home.join(CCR_HOME_DIR).join(SERVICE_FILE))
}

fn read_service_file(path: &PathBuf) -> Option<CcrEndpoint> {
    let raw = std::fs::read_to_string(path).ok()?;
    parse_service_file(&raw)
}

/// The management base URL (no trailing slash): env → CCR's own service file →
/// the default loopback address.
pub fn web_url() -> String {
    if let Some(from_env) = env_value("CCR_WEB_URL") {
        return from_env.trim_end_matches('/').to_string();
    }
    discovered_endpoint()
        .map(|endpoint| endpoint.base_url)
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| DEFAULT_CCR_WEB_URL.to_string())
}

/// The management auth token: env → CCR's own service file.
pub fn web_token() -> Option<String> {
    if let Some(from_env) = env_value("CCR_WEB_AUTH_TOKEN") {
        return Some(from_env);
    }
    discovered_endpoint().and_then(|endpoint| endpoint.token)
}

/// Where the token came from, for the settings page to explain itself.
pub fn web_token_source() -> Option<&'static str> {
    if env_value("CCR_WEB_AUTH_TOKEN").is_some() {
        return Some("env");
    }
    match discovered_endpoint().and_then(|endpoint| endpoint.token) {
        Some(_) => Some("service-file"),
        None => None,
    }
}

/// Connection info for the settings page header (no network call).
pub fn health() -> Value {
    json!({
        "url": web_url(),
        "tokenConfigured": web_token().is_some(),
        "tokenSource": web_token_source(),
    })
}

/// Host + port of a management URL, when it is something we can dial.
pub fn parse_web_addr(url: &str) -> Option<(String, u16)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_string();
    let port = parsed.port_or_known_default()?;
    Some((host, port))
}

/// True for hosts that name this machine. Only these may be auto-started: a
/// remote management URL is somebody else's process, and a local `ccr start`
/// would neither reach it nor help it.
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Forward one management RPC call to CCR and return its `value` field.
pub async fn call(method: &str, args: Option<Vec<Value>>) -> Result<Value, AppCommandError> {
    call_with_timeout(method, args, RPC_TIMEOUT_SECS).await
}

/// [`call`] with an explicit timeout — the supervisor probes are deliberately
/// impatient, so a hung management server cannot stall a status refresh.
pub async fn call_with_timeout(
    method: &str,
    args: Option<Vec<Value>>,
    timeout_secs: u64,
) -> Result<Value, AppCommandError> {
    let method = method.trim();
    if method.is_empty() {
        return Err(AppCommandError::invalid_input("CCR RPC method is required"));
    }
    let token = web_token().ok_or_else(|| {
        AppCommandError::configuration_missing(
            "CCR_WEB_AUTH_TOKEN is not set; start CCR with a pinned token so Codeg can manage models.",
        )
    })?;
    let url = format!("{}/api/ccr/rpc", web_url());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|error| AppCommandError::network(format!("CCR HTTP client failed: {error}")))?;
    let response = client
        .post(&url)
        .header("x-ccr-web-auth", token)
        .json(&json!({ "method": method, "args": args.unwrap_or_default() }))
        .send()
        .await
        .map_err(|error| AppCommandError::network(format!("CCR is unreachable at {url}: {error}")))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| AppCommandError::network(format!("CCR returned invalid JSON: {error}")))?;
    if !status.is_success() || body.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("CCR RPC failed");
        return Err(AppCommandError::network(format!("CCR {method}: {message}")));
    }
    Ok(body.get("value").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_and_port_from_a_management_url() {
        assert_eq!(
            parse_web_addr("http://127.0.0.1:3458"),
            Some(("127.0.0.1".to_string(), 3458))
        );
        assert_eq!(
            parse_web_addr("http://localhost:3458/"),
            Some(("localhost".to_string(), 3458))
        );
        // No port: the scheme default is the only sane reading.
        assert_eq!(
            parse_web_addr("http://127.0.0.1"),
            Some(("127.0.0.1".to_string(), 80))
        );
    }

    #[test]
    fn rejects_a_management_url_we_cannot_dial() {
        assert_eq!(parse_web_addr("not a url"), None);
        assert_eq!(parse_web_addr("file:///tmp/ccr.sock"), None);
    }

    #[test]
    fn only_local_hosts_are_startable() {
        for host in ["127.0.0.1", "localhost", "::1"] {
            assert!(is_loopback_host(host), "{host} is this machine");
        }
        for host in ["10.0.0.7", "ccr.internal", "example.com", "0.0.0.0"] {
            assert!(!is_loopback_host(host), "{host} is not this machine");
        }
    }

    #[test]
    fn reads_endpoint_and_web_token_out_of_ccrs_service_file() {
        // The shape `ccr start` actually writes (measured 2026-10-19).
        let raw = r#"{
            "pid": 92581,
            "serviceToken": "Qwu5-DT7jR_LbjzQhey-tL8IlCtm5kLXzUiBiuJQNd0",
            "url": "http://127.0.0.1:3458/?ccr_web_token=d8b44d5ea2d7de03b29a16c6eec1f3d3"
        }"#;
        let endpoint = parse_service_file(raw).unwrap();
        assert_eq!(endpoint.base_url, "http://127.0.0.1:3458");
        assert_eq!(
            endpoint.token.as_deref(),
            Some("d8b44d5ea2d7de03b29a16c6eec1f3d3")
        );
    }

    #[test]
    fn never_mistakes_the_service_token_for_the_web_token() {
        // A file whose URL carries no web token yields NO token: falling back to
        // `serviceToken` would 401 every management call instead.
        let raw = r#"{
            "serviceToken": "service-identity",
            "url": "http://127.0.0.1:3458/"
        }"#;
        let endpoint = parse_service_file(raw).unwrap();
        assert_eq!(endpoint.base_url, "http://127.0.0.1:3458");
        assert_eq!(endpoint.token, None);
    }

    #[test]
    fn accepts_a_flat_web_token_field_and_tolerates_junk() {
        let flat = parse_service_file(
            r#"{ "url": "http://127.0.0.1:3458/?other=1", "ccr_web_token": "flat-token" }"#,
        )
        .unwrap();
        assert_eq!(flat.token.as_deref(), Some("flat-token"));
        // The whole query is dropped, not just our parameter: the base URL is
        // what `{base}/api/ccr/rpc` is built from, and a leftover `?other=1`
        // there would be meaningless at best.
        assert_eq!(flat.base_url, "http://127.0.0.1:3458");

        assert!(parse_service_file("not json").is_none());
        assert!(parse_service_file(r#"{ "pid": 1 }"#).is_none());
        assert!(parse_service_file(r#"{ "url": "  " }"#).is_none());
    }
}
