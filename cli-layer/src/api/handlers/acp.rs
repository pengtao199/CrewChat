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
use crate::runtime::engine::manager;

pub(crate) async fn acp_list_agents(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let mut agents = state.host.engines().list();
    for agent in &mut agents {
        if let Some(agent_type) = agent.get("agent_type").and_then(Value::as_str) {
            if let Ok(preferences) = state.store.engine_preferences(agent_type) {
                agent["enabled"] = json!(preferences.enabled);
                agent["env"] = json!(preferences.env);
            }
        }
    }
    Json(agents).into_response()
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
    let preferences = state
        .store
        .engine_preferences(agent_type)
        .unwrap_or_default();
    if !preferences.enabled {
        return api_error(StatusCode::BAD_REQUEST, "this engine is disabled");
    }
    let cwd = field(&body, &["workingDir", "working_dir"])
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));
    let mut role =
        field(&body, &["roleId", "role_id"]).and_then(|id| state.store.role(id).ok().flatten());
    if let Some(role) = role.as_mut() {
        if let Some(provider) = role
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let model = role
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(display) = state.model.normalize_provider(&provider, &model).await {
                role["model_provider"] = json!(display);
            }
        }
    }
    let api_key = state.model.local_gateway_key().await;
    match state
        .host
        .connect(
            agent_type,
            cwd,
            role,
            api_key,
            field(&body, &["conversationId", "conversation_id"]).map(str::to_string),
            preferences.env,
        )
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error.to_string()),
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
    if text.trim().is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "prompt has no text");
    }
    if let Some(conversation_id) = field(&body, &["conversationId", "conversation_id"]) {
        let _ = state.store.append_turn(
            conversation_id,
            "user",
            &json!([{"type": "text", "text": text.clone()}]),
        );
    }
    match state.host.prompt(connection_id, text) {
        Ok(()) => Json(json!({})).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
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
    match state.host.cancel(connection_id) {
        Ok(()) => Json(json!({})).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
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
    state.host.close(connection_id);
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
    if state.host.respond_permission(request_id, option_id) {
        Json(json!(true)).into_response()
    } else {
        api_error(StatusCode::NOT_FOUND, "unknown permission request")
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
    match state
        .store
        .save_engine_preferences(agent_type, enabled, &env)
    {
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
