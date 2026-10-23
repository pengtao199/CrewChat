use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::http::{api_error, authorized, unauthorized, AppState};

pub(crate) async fn model_health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(state.models.health()).into_response()
}

pub(crate) async fn model_call(
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
    match state.models.rpc(method, args).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error),
    }
}

pub(crate) async fn model_provider_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.models.provider_save(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(crate) async fn model_provider_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.models.provider_delete(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(crate) async fn model_provider_probe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.models.provider_probe(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(crate) async fn model_disabled_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.models.disabled_models().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error),
    }
}

pub(crate) async fn model_supervisor_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(json!({"state": "running", "endpoint": state.models.health()["url"]})).into_response()
}

pub(crate) async fn model_provider_set_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let provider_id = body
        .get("providerId")
        .or_else(|| body.get("provider_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match state
        .models
        .set_model_enabled(provider_id, model, enabled)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(crate) async fn model_provider_set_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let provider_id = body
        .get("providerId")
        .or_else(|| body.get("provider_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let credential_id = body
        .get("credentialId")
        .or_else(|| body.get("credential_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match state
        .models
        .set_credential_enabled(provider_id, credential_id, enabled)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
    }
}
