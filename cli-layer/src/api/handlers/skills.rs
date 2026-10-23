use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

use crate::api::http::{api_error, authorized, field, unauthorized, AppState};

pub(crate) async fn skill_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.skills.catalog().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

pub(crate) async fn skill_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["skillId", "skill_id", "id"]).unwrap_or_default();
    match state.skills.read(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

pub(crate) async fn skill_install(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.skills.install(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn skill_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["skillId", "skill_id", "id"]).unwrap_or_default();
    match state.skills.check(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn skill_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["skillId", "skill_id", "id"]).unwrap_or_default();
    match state.skills.update(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn skill_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["skillId", "skill_id", "id"]).unwrap_or_default();
    match state.skills.remove(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn skill_link_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]);
    match state.skills.link_status(agent_type).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}
