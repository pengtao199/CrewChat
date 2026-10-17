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
//! - `CCR_WEB_URL`        — management base URL (default `http://127.0.0.1:3458`)
//! - `CCR_WEB_AUTH_TOKEN` — management auth token (required; CCR writes it to
//!   stdout at startup unless it is pinned)
//!
//! `parse_web_addr` / `is_loopback_host` are pure so the supervisor's
//! "may I start a local instance?" decision is unit-testable.

use serde_json::{json, Value};

use crate::app_error::AppCommandError;

pub const DEFAULT_CCR_WEB_URL: &str = "http://127.0.0.1:3458";
const RPC_TIMEOUT_SECS: u64 = 30;

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The management base URL (no trailing slash).
pub fn web_url() -> String {
    env_value("CCR_WEB_URL")
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_CCR_WEB_URL.to_string())
}

/// The management auth token, when one is configured.
pub fn web_token() -> Option<String> {
    env_value("CCR_WEB_AUTH_TOKEN")
}

/// Connection info for the settings page header (no network call).
pub fn health() -> Value {
    json!({
        "url": web_url(),
        "tokenConfigured": web_token().is_some(),
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
}
