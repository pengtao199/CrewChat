use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::http::{api_error, authorized, blocks_to_text, field, unauthorized, AppState};
use crate::app::sessions::SessionError;
use crate::runtime::engine::manager;

pub(crate) async fn acp_list_agents(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(state.engines.list()).into_response()
}

pub(crate) async fn acp_connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or("claude_code");
    let cwd = field(&body, &["workingDir", "working_dir"])
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));
    let role_id = field(&body, &["roleId", "role_id"]);
    let conversation_id = field(&body, &["conversationId", "conversation_id"]).map(str::to_string);
    match state
        .sessions
        .connect(agent_type, cwd, role_id, conversation_id)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => session_error(error),
    }
}

pub(crate) async fn acp_prompt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let connection_id = field(&body, &["connectionId", "connection_id"]).unwrap_or_default();
    let text = body
        .get("blocks")
        .map(blocks_to_text)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| field(&body, &["text"]).map(str::to_string))
        .unwrap_or_default();
    let conversation_id = field(&body, &["conversationId", "conversation_id"]);
    match state.sessions.prompt(connection_id, text, conversation_id) {
        Ok(()) => Json(json!({})).into_response(),
        Err(error) => session_error(error),
    }
}

pub(crate) async fn acp_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let connection_id = field(&body, &["connectionId", "connection_id"]).unwrap_or_default();
    match state.sessions.cancel(connection_id) {
        Ok(()) => Json(json!({})).into_response(),
        Err(error) => session_error(error),
    }
}

pub(crate) async fn acp_disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let connection_id = field(&body, &["connectionId", "connection_id"]).unwrap_or_default();
    state.sessions.disconnect(connection_id);
    Json(json!({})).into_response()
}

pub(crate) async fn acp_respond_permission(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request_id = field(&body, &["requestId", "request_id"]).unwrap_or_default();
    let option_id = field(&body, &["optionId", "option_id"]).map(str::to_string);
    match state.sessions.respond_permission(request_id, option_id) {
        Ok(true) => Json(json!(true)).into_response(),
        Ok(false) => api_error(StatusCode::NOT_FOUND, "unknown permission request"),
        Err(error) => session_error(error),
    }
}

pub(crate) async fn acp_update_agent_preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    if agent_type.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "agentType is required");
    }
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let env: BTreeMap<String, String> = body
        .get("env")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    match state.sessions.update_preferences(agent_type, enabled, &env) {
        Ok(()) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn acp_fetch_agent_latest_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    match manager::latest_version(&state.config, agent_type).await {
        Ok(Some(version)) => Json(json!({"version": version})).into_response(),
        Ok(None) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn acp_prepare_npx_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    let version = field(&body, &["version", "registryVersion", "registry_version"]);
    let clean_first = body
        .get("cleanFirst")
        .or_else(|| body.get("clean_first"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match manager::prepare_npx(&state.config, agent_type, version, clean_first).await {
        Ok(version) => Json(json!(version)).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn acp_download_agent_binary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    let version = field(&body, &["version"]);
    match manager::download_binary(&state.config, agent_type, version).await {
        Ok(version) => Json(json!(version)).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn acp_install_pi_binary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match manager::install_pi_binary().await {
        Ok(_) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn acp_env_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    Json(manager::diagnostics(&state.config, agent_type).await).into_response()
}

fn session_error(error: SessionError) -> Response {
    match error {
        SessionError::BadRequest(message) => api_error(StatusCode::BAD_REQUEST, &message),
        SessionError::BadGateway(message) => api_error(StatusCode::BAD_GATEWAY, &message),
        SessionError::NotFound(message) => api_error(StatusCode::NOT_FOUND, &message),
    }
}
