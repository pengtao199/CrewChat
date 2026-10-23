use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::http::{api_error, authorized, field, unauthorized, AppState};

pub(crate) async fn mcp_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.mcp.list().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

pub(crate) async fn mcp_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["serverId", "server_id", "id"]).unwrap_or_default();
    match state.mcp.read(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

pub(crate) async fn mcp_upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["serverId", "server_id", "id"])
        .or_else(|| {
            body.get("spec")
                .and_then(|spec| spec.get("id"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    if id.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "serverId is required");
    }
    let mut spec = body.get("spec").cloned().unwrap_or_else(|| body.clone());
    if let Some(object) = spec.as_object_mut() {
        object.insert("id".to_string(), json!(id));
        object.remove("serverId");
        object.remove("server_id");
    }
    match state.mcp.upsert(id, &spec).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn mcp_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["serverId", "server_id", "id"]).unwrap_or_default();
    match state.mcp.remove(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn mcp_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    set_enabled(state, headers, body, true).await
}

pub(crate) async fn mcp_disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    set_enabled(state, headers, body, false).await
}

async fn set_enabled(state: AppState, headers: HeaderMap, body: Value, enabled: bool) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["serverId", "server_id", "id"]).unwrap_or_default();
    match state.mcp.set_enabled(id, enabled).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn mcp_gateway(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.mcp.gateway().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

pub(crate) async fn agent_mcps_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    let server_ids = string_array(body.get("serverIds").or_else(|| body.get("server_ids")));
    match state.mcp.set_agent_mcps(role_id, server_ids).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn agent_mcp_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    match state.mcp.agent_status(role_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(id) => Some(id.clone()),
                    Value::Object(object) => {
                        object.get("id").and_then(Value::as_str).map(str::to_string)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
