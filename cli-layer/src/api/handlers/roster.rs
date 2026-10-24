use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::http::{api_error, authorized, field, unauthorized, AppState};
use crate::domain::agent::Agent;

pub(crate) async fn agent_roles_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.roster.list() {
        Ok(roles) => Json(roles).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn agent_roles_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role = body.get("role").cloned().unwrap_or(body);
    let mut agent = match Agent::from_value(role) {
        Ok(agent) => agent,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    agent.mcp = normalized_mcp(&agent.mcp);

    let previous = state.roster.get(&agent.id).ok().flatten();
    let mcp_changed = previous
        .as_ref()
        .map(|role| normalized_mcp(&role.mcp) != agent.mcp)
        .unwrap_or(!agent.mcp.is_empty());

    let saved = match state.roster.save(&agent) {
        Ok(role) => role,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };

    if mcp_changed {
        if let Err(error) = state.mcp.sync_agent_mcps(&saved.id, &saved.mcp).await {
            let rollback = match previous {
                Some(role) => state.roster.save(&role).map(|_| ()),
                None => state.roster.delete(&saved.id),
            };
            let _ = rollback;
            return api_error(
                StatusCode::BAD_GATEWAY,
                &format!("save role mcp binding: {error}"),
            );
        }
    }

    Json(saved).into_response()
}

pub(crate) async fn agent_roles_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["id"]).unwrap_or_default();
    match state.roster.delete(id) {
        Ok(()) => Json(json!(true)).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(crate) async fn agent_role_model_face(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or("claude_code");
    match state.roster.model_face(agent_type).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

pub(crate) async fn agent_role_permission_face(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or("claude_code");
    Json(state.permissions.face(agent_type)).into_response()
}

pub(crate) async fn agent_role_readiness(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    match state.readiness.check(role_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

pub(crate) async fn agent_role_test_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    let readiness = match state.readiness.check(role_id).await {
        Ok(value) => value,
        Err(error) => return api_error(StatusCode::NOT_FOUND, &error.to_string()),
    };
    if readiness.get("ready").and_then(Value::as_bool) != Some(true) {
        return Json(json!({
            "ok": false,
            "phase": "readiness",
            "readiness": readiness,
        }))
        .into_response();
    }
    match state.sessions.test_run(role_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn agent_skills_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    let skill_ids = string_array(body.get("skillIds").or_else(|| body.get("skill_ids")));
    let collection_ids = string_array(
        body.get("collectionIds")
            .or_else(|| body.get("collection_ids")),
    );
    match state
        .skills
        .set_agent_skills(role_id, skill_ids, collection_ids)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

pub(crate) async fn agent_skill_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    match state.skills.agent_status(role_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

fn normalized_mcp(ids: &[String]) -> Vec<String> {
    let mut values = ids.to_vec();
    values.sort();
    values.dedup();
    values
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
