use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::http::{api_error, authorized, unauthorized, AppState};

pub(crate) async fn ccr_health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(state.model.health()).into_response()
}

pub(crate) async fn ccr_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = body.get("args").cloned().unwrap_or_else(|| json!([]));
    match state.model.rpc(method, args).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error),
    }
}

pub(crate) async fn ccr_provider_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.model.provider_save(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(crate) async fn ccr_provider_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.model.provider_delete(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(crate) async fn ccr_provider_probe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.model.provider_probe(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(crate) async fn ccr_disabled_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.model.disabled_models().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error),
    }
}

pub(crate) async fn ccr_supervisor_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(json!({"state": "running", "endpoint": state.model.health()["url"]})).into_response()
}
