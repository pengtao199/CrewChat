//! Upstream HTTP client and provider quirks.
//!
//! One `reqwest::Client` per process; one request-scoped route per call. The
//! only mutable state here is the OpenCode client session id, which is a
//! fingerprint, not a routing decision.

use std::time::Duration;

use anyhow::{Context, Result};
use http::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde_json::Value;

use crate::model_id::Target;

#[derive(Debug)]
pub struct UpstreamError {
    pub status: StatusCode,
    pub message: String,
}

impl UpstreamError {
    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UpstreamError {}

pub struct Upstream {
    client: reqwest::Client,
    opencode_session: String,
}

impl Upstream {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            client,
            opencode_session: format!("ccr-{}", uuid::Uuid::new_v4().simple()),
        })
    }

    /// POST a JSON body to `path` (`chat/completions`, `responses`, `messages`).
    pub async fn send_json(
        &self,
        target: &Target,
        api_key: &str,
        path: &str,
        body: &Value,
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = endpoint(&target.base_url, path);
        let mut headers = self.headers(target, api_key);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(|error| UpstreamError::bad_gateway(format!("upstream {url}: {error}")))?;
        Ok(response)
    }

    /// POST raw bytes, preserving the caller's content type (pass-through
    /// endpoints must not re-encode a body we do not understand).
    pub async fn send_bytes(
        &self,
        target: &Target,
        api_key: &str,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = endpoint(&target.base_url, path);
        let mut headers = self.headers(target, api_key);
        if let Some(content_type) = content_type {
            if let Ok(value) = HeaderValue::from_str(content_type) {
                headers.insert(CONTENT_TYPE, value);
            }
        }
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|error| UpstreamError::bad_gateway(format!("upstream {url}: {error}")))?;
        Ok(response)
    }

    fn headers(&self, target: &Target, api_key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {api_key}")) {
            headers.insert(AUTHORIZATION, value);
        }
        if is_opencode(&target.base_url) {
            // OpenCode Go wants a client-shaped session header; the value is
            // only a fingerprint, so a stable per-process id is enough.
            if let Ok(value) = HeaderValue::from_str(&self.opencode_session) {
                headers.insert("x-opencode-session", value);
            }
            headers.insert("x-opencode-client", HeaderValue::from_static("cli"));
            headers.insert(USER_AGENT, HeaderValue::from_static("opencode/1.18.31"));
        } else {
            headers.insert(USER_AGENT, HeaderValue::from_static("crewchat-model/0.1"));
        }
        headers
    }

    pub async fn probe_models(base_url: &str, api_key: &str) -> Result<Vec<String>, UpstreamError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| UpstreamError::bad_gateway(error.to_string()))?;
        let url = endpoint(base_url, "models");
        let response = client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(USER_AGENT, "opencode/1.18.31")
            .send()
            .await
            .map_err(|error| UpstreamError::bad_gateway(format!("probe {url}: {error}")))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|error| UpstreamError::bad_gateway(format!("probe {url}: {error}")))?;
        if !status.is_success() {
            return Err(UpstreamError {
                status,
                message: format!("probe {url} returned {status}: {body}"),
            });
        }
        Ok(model_ids(&body))
    }
}

pub fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    let path = path.trim().trim_start_matches('/');
    let path = path.strip_prefix("v1/").unwrap_or(path);
    if base.ends_with("/v1") {
        format!("{base}/{path}")
    } else {
        format!("{base}/v1/{path}")
    }
}

fn is_opencode(base: &str) -> bool {
    reqwest::Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .map(|host| host == "opencode.ai" || host.ends_with(".opencode.ai"))
        .unwrap_or(false)
}

fn model_ids(body: &Value) -> Vec<String> {
    if let Some(rows) = body.get("data").and_then(Value::as_array) {
        return rows
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
    }
    if let Some(rows) = body.get("models").and_then(Value::as_array) {
        return rows
            .iter()
            .filter_map(|row| {
                row.as_str()
                    .map(str::to_string)
                    .or_else(|| row.get("id").and_then(Value::as_str).map(str::to_string))
            })
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_v1_once() {
        assert_eq!(
            endpoint("https://opencode.ai/zen/go/v1", "chat/completions"),
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.openai.com", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://openrouter.ai/api/v1/", "responses"),
            "https://openrouter.ai/api/v1/responses"
        );
    }

    #[test]
    fn parses_both_model_list_shapes() {
        assert_eq!(
            model_ids(&serde_json::json!({"data": [{"id": "a"}, {"id": "b"}]})),
            vec!["a", "b"]
        );
        assert_eq!(
            model_ids(&serde_json::json!({"models": ["a", {"id": "b"}]})),
            vec!["a", "b"]
        );
    }
}
