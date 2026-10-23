//! Team status and internal delegation endpoints.
//!
//! `team_delegate` is called by the Leader-only control MCP tool, never by the
//! AgentOS UI. The normal public command is `create_conversation` with `team`.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

use crate::api::http::{api_error, authorized, field, unauthorized, AppState};

pub(crate) async fn team_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let conversation_id = field(&body, &["conversationId", "conversation_id"]).unwrap_or_default();
    match state.team.status(conversation_id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn cancel_delegation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let delegation_id = field(&body, &["delegationId", "delegation_id"]).unwrap_or_default();
    match state.team.cancel(delegation_id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn team_delegate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let conversation_id = field(&body, &["conversationId", "conversation_id"]).unwrap_or_default();
    let leader_role_id = field(&body, &["leaderRoleId", "leader_role_id"]).unwrap_or_default();
    let member_role_id = field(&body, &["memberRoleId", "member_role_id"]).unwrap_or_default();
    let task = field(&body, &["task"]).unwrap_or_default();
    let context = field(&body, &["context"]);
    let expected = field(&body, &["expectedOutput", "expected_output"]);
    match state
        .team
        .delegate(
            conversation_id,
            leader_role_id,
            member_role_id,
            task,
            context,
            expected,
        )
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}
