//! [crewchat] Thin proxy to the CCR (Claude Code Router) management RPC.
//!
//! In this fork Codeg does not own model management: the single source of truth
//! is CCR (vendored at `ccr/`), reachable through its local management server.
//! This module forwards `POST /api/ccr/rpc {method, args}` verbatim so the
//! settings UI can read/write providers, agent profiles and gateway state.
//!
//! Environment:
//! - `CCR_WEB_URL`        — management base URL (default `http://127.0.0.1:3458`)
//! - `CCR_WEB_AUTH_TOKEN` — management auth token (required; CCR writes it to
//!   stdout at startup unless it is pinned)

use serde_json::{json, Value};

use crate::app_error::AppCommandError;

const DEFAULT_CCR_WEB_URL: &str = "http://127.0.0.1:3458";
const RPC_TIMEOUT_SECS: u64 = 30;

fn ccr_web_url() -> String {
    std::env::var("CCR_WEB_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_CCR_WEB_URL.to_string())
}

fn ccr_web_token() -> Option<String> {
    std::env::var("CCR_WEB_AUTH_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Connection info for the settings page header (no network call).
pub fn ccr_health_core() -> Value {
    json!({
        "url": ccr_web_url(),
        "tokenConfigured": ccr_web_token().is_some(),
    })
}

/// Forward one management RPC call to CCR and return its `value` field.
pub async fn ccr_call_core(method: &str, args: Option<Vec<Value>>) -> Result<Value, AppCommandError> {
    let method = method.trim();
    if method.is_empty() {
        return Err(AppCommandError::invalid_input("CCR RPC method is required"));
    }
    let token = ccr_web_token().ok_or_else(|| {
        AppCommandError::configuration_missing(
            "CCR_WEB_AUTH_TOKEN is not set; start CCR with a pinned token so Codeg can manage models.",
        )
    })?;
    let url = format!("{}/api/ccr/rpc", ccr_web_url());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(RPC_TIMEOUT_SECS))
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

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn ccr_health() -> Value {
    ccr_health_core()
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn ccr_call(method: String, args: Option<Vec<Value>>) -> Result<Value, AppCommandError> {
    ccr_call_core(&method, args).await
}
