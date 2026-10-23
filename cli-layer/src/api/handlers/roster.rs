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
    let agent = match Agent::from_value(role) {
        Ok(agent) => agent,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    match state.roster.save(&agent) {
        Ok(role) => Json(role).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
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
