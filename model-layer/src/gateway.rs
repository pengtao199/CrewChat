//! The local gateway: `/v1/models`, `/v1/messages`, `/v1/chat/completions`,
//! `/v1/responses`.
//!
//! Every handler resolves the request's `model` independently. There is no
//! "current model", no CLI binding and no per-process routing state.

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::app::App;
use crate::model_id::{self, ResolveError, Target};
use crate::translate;

pub fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .with_state(app)
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "service": "crewchat-model"}))
}

async fn models(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Err(response) = check_auth(&app, &headers, true) {
        return response;
    }
    let views = match app.store.provider_views() {
        Ok(views) => views,
        Err(error) => {
            return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
        }
    };
    let claude = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("claude"))
        .unwrap_or(false);

    let mut data = Vec::new();
    for view in views {
        for model in &view.models {
            let id = if claude {
                let hex = model_id::virtual_hex(&view.name, model);
                if view.one_million.iter().any(|m| m == model) {
                    format!("anthropic/claude-ccr-h{hex}[1m]")
                } else {
                    format!("anthropic/claude-ccr-h{hex}")
                }
            } else {
                format!("{}/{}", view.name, model)
            };
            data.push(json!({
                "id": id,
                "type": "model",
                "display_name": model,
                "created_at": "2026-01-01T00:00:00Z",
            }));
        }
    }

    let first = data.first().and_then(|row| row.get("id")).cloned();
    let last = data.last().and_then(|row| row.get("id")).cloned();
    Json(json!({
        "data": data,
        "has_more": false,
        "first_id": first,
        "last_id": last,
    }))
    .into_response()
}

async fn count_tokens(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(response) = check_auth(&app, &headers, true) {
        return response;
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return anthropic_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    Json(json!({"input_tokens": translate::estimate_tokens(&value)})).into_response()
}

async fn messages(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(response) = check_auth(&app, &headers, true) {
        return response;
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return anthropic_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let Some(requested_model) = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return anthropic_error(StatusCode::BAD_REQUEST, "model is required");
    };
    let target = match resolve(&app, &requested_model) {
        Ok(target) => target,
        Err(error) => return resolve_error(&error),
    };
    let key = match credential(&app, &target) {
        Ok(key) => key,
        Err(response) => return response,
    };

    if target.kind == "anthropic_messages" {
        let mut upstream_body = value.clone();
        upstream_body["model"] = Value::String(target.model.clone());
        return match app
            .upstream
            .send_json(&target, &key, "messages", &upstream_body)
            .await
        {
            Ok(response) => proxy_response(response, true).await,
            Err(error) => anthropic_error(error.status, &error.message),
        };
    }

    let mut chat = match translate::anthropic_to_chat(&value) {
        Ok(chat) => chat,
        Err(error) => return anthropic_error(StatusCode::BAD_REQUEST, &error),
    };
    chat["model"] = Value::String(target.model.clone());
    let stream = chat.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let response = match app
        .upstream
        .send_json(&target, &key, "chat/completions", &chat)
        .await
    {
        Ok(response) => response,
        Err(error) => return anthropic_error(error.status, &error.message),
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return anthropic_error(
            status,
            &format!(
                "upstream {}/{} returned {status}: {body}",
                target.provider_name, target.model
            ),
        );
    }

    if stream {
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if !content_type.contains("event-stream") {
            let body = response.text().await.unwrap_or_default();
            return anthropic_error(
                StatusCode::BAD_GATEWAY,
                &format!("upstream did not return an SSE stream: {body}"),
            );
        }
        return Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/event-stream")
            .header("cache-control", "no-cache")
            .body(Body::from_stream(translate::chat_stream(
                response,
                requested_model,
            )))
            .unwrap_or_else(|error| {
                anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
            });
    }

    let body: Value = match response.json().await {
        Ok(body) => body,
        Err(error) => return anthropic_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    };
    Json(translate::chat_to_anthropic(&body, &requested_model)).into_response()
}

async fn chat_completions(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    passthrough(app, headers, body, "chat/completions").await
}

async fn responses(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    passthrough(app, headers, body, "responses").await
}

async fn passthrough(app: App, headers: HeaderMap, body: Bytes, path: &str) -> Response {
    if let Err(response) = check_auth(&app, &headers, false) {
        return response;
    }
    let mut value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return openai_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let Some(requested_model) = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return openai_error(StatusCode::BAD_REQUEST, "model is required");
    };
    let target = match resolve(&app, &requested_model) {
        Ok(target) => target,
        Err(error) => return resolve_error_openai(&error),
    };
    let key = match credential(&app, &target) {
        Ok(key) => key,
        Err(response) => return response,
    };
    if target.kind == "anthropic_messages" {
        return openai_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "provider {} is anthropic_messages and cannot serve {path}",
                target.provider_name
            ),
        );
    }

    value["model"] = Value::String(target.model.clone());
    let body = match serde_json::to_vec(&value) {
        Ok(body) => body,
        Err(error) => return openai_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json");

    let response = match app
        .upstream
        .send_bytes(&target, &key, path, Some(content_type), body)
        .await
    {
        Ok(response) => response,
        Err(error) => return openai_error(error.status, &error.message),
    };
    proxy_response(response, false).await
}

/// Forward an upstream response: streams stay streams, JSON stays JSON, and
/// upstream errors keep their status.
async fn proxy_response(response: reqwest::Response, anthropic: bool) -> Response {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return if anthropic {
            anthropic_error(status, &body)
        } else {
            // Preserve the upstream JSON when possible; otherwise wrap it.
            match serde_json::from_str::<Value>(&body) {
                Ok(value) => (status, Json(value)).into_response(),
                Err(_) => openai_error(status, &body),
            }
        };
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if content_type.contains("event-stream") {
        return Response::builder()
            .status(status)
            .header(CONTENT_TYPE, content_type)
            .header("cache-control", "no-cache")
            .body(Body::from_stream(response.bytes_stream()))
            .unwrap_or_else(|error| {
                openai_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
            });
    }

    match response.bytes().await {
        Ok(bytes) => Response::builder()
            .status(status)
            .header(CONTENT_TYPE, content_type)
            .body(Body::from(bytes))
            .unwrap_or_else(|error| {
                openai_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
            }),
        Err(error) => openai_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

fn resolve(app: &App, raw: &str) -> Result<Target, ResolveError> {
    let views = app
        .store
        .provider_views()
        .map_err(|_| ResolveError::NotFound(raw.to_string()))?;
    model_id::resolve(raw, &views)
}

#[allow(clippy::result_large_err)]
fn credential(app: &App, target: &Target) -> Result<String, Response> {
    match app.store.pick_credential(&target.provider_id) {
        Ok(Some(credential)) => Ok(credential.api_key),
        Ok(None) => match app.store.fallback_key(&target.provider_id) {
            Ok(Some(key)) => Ok(key),
            _ => Err(openai_error(
                StatusCode::BAD_REQUEST,
                &format!(
                    "provider {} has no enabled credential",
                    target.provider_name
                ),
            )),
        },
        Err(error) => Err(openai_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &error.to_string(),
        )),
    }
}

#[allow(clippy::result_large_err)]
fn check_auth(app: &App, headers: &HeaderMap, anthropic: bool) -> Result<(), Response> {
    let configured = app.store.has_caller_keys().unwrap_or(false);
    if !configured {
        // First run, before the import: the listener is loopback-only and the
        // model page can still open. The moment caller keys exist, they are
        // enforced.
        return Ok(());
    }
    let key = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .map(str::trim)
        .unwrap_or_default();
    if app.store.caller_key_valid(key).unwrap_or(false) {
        return Ok(());
    }
    Err(if anthropic {
        anthropic_error(StatusCode::UNAUTHORIZED, "invalid gateway key")
    } else {
        openai_error(StatusCode::UNAUTHORIZED, "invalid gateway key")
    })
}

fn resolve_error(error: &ResolveError) -> Response {
    let status = match error {
        ResolveError::Ambiguous { .. } => StatusCode::BAD_REQUEST,
        ResolveError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_REQUEST,
    };
    anthropic_error(status, &error.to_string())
}

fn resolve_error_openai(error: &ResolveError) -> Response {
    let status = match error {
        ResolveError::Ambiguous { .. } => StatusCode::BAD_REQUEST,
        ResolveError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_REQUEST,
    };
    openai_error(status, &error.to_string())
}

pub fn anthropic_error(status: StatusCode, message: &str) -> Response {
    let kind = match status {
        StatusCode::UNAUTHORIZED => "authentication_error",
        StatusCode::NOT_FOUND => "not_found_error",
        StatusCode::BAD_REQUEST => "invalid_request_error",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        _ => "api_error",
    };
    (
        status,
        Json(translate::error_body(kind, message.to_string())),
    )
        .into_response()
}

pub fn openai_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": match status {
                    StatusCode::UNAUTHORIZED => "authentication_error",
                    StatusCode::BAD_REQUEST => "invalid_request_error",
                    StatusCode::NOT_FOUND => "not_found_error",
                    _ => "api_error",
                }
            }
        })),
    )
        .into_response()
}
