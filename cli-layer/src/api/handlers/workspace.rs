use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::http::{api_error, authorized, field, unauthorized, AppState};

pub(crate) async fn list_folders(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.store.list_folders() {
        Ok(folders) => Json(folders).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn open_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let path = field(&body, &["path"]).unwrap_or_default();
    match state.store.open_folder(path) {
        Ok(folder) => Json(folder).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn remove_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["folder_id", "folderId"]).unwrap_or_default();
    match state.store.remove_folder(id) {
        Ok(()) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn list_conversations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let folders: Vec<String> = body
        .get("folderIds")
        .or_else(|| body.get("folder_ids"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    match state.store.list_conversations(&folders) {
        Ok(rows) => Json(rows).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn create_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let folder = field(&body, &["folderId", "folder_id"]);
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or("claude_code");
    let title = field(&body, &["title"]);
    match state.store.create_conversation(folder, agent_type, title) {
        Ok(id) => Json(json!(id)).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn get_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["conversationId", "conversation_id"]).unwrap_or_default();
    match state.store.get_conversation(id) {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "conversation not found"),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn delete_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["conversationId", "conversation_id"]).unwrap_or_default();
    match state.store.delete_conversation(id) {
        Ok(()) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}
