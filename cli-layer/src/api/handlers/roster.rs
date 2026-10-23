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
    let Some(mut role) = state.roster.get(role_id).ok().flatten() else {
        return api_error(StatusCode::NOT_FOUND, "role not found");
    };
    let mut ids = string_array(body.get("skillIds").or_else(|| body.get("skill_ids")));
    let collection_ids = string_array(
        body.get("collectionIds")
            .or_else(|| body.get("collection_ids")),
    );
    if !collection_ids.is_empty() {
        match state.skill.collections().await {
            Ok(value) => {
                if let Some(collections) = value.get("collections").and_then(Value::as_array) {
                    for collection in collections {
                        let id = collection
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !collection_ids.iter().any(|wanted| wanted == id) {
                            continue;
                        }
                        ids.extend(string_array(
                            collection
                                .get("skillIds")
                                .or_else(|| collection.get("skill_ids")),
                        ));
                    }
                }
            }
            Err(error) => return api_error(StatusCode::BAD_GATEWAY, &error),
        }
    }
    ids.sort();
    ids.dedup();
    role.skills = ids;
    match state.roster.save(&role) {
        Ok(role) => Json(json!({
            "roleId": role.id,
            "skillIds": role.skills,
        }))
        .into_response(),
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
    let Some(role) = state.roster.get(role_id).ok().flatten() else {
        return api_error(StatusCode::NOT_FOUND, "role not found");
    };
    let selected = role.skills;
    let available = state
        .skill
        .list()
        .await
        .ok()
        .and_then(|value| value.get("skills").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let mut rows = Vec::new();
    let mut missing = Vec::new();
    for id in selected {
        let found = available
            .iter()
            .find(|skill| skill.get("id").and_then(Value::as_str) == Some(id.as_str()));
        if found.is_none() {
            missing.push(id.clone());
        }
        rows.push(json!({
            "id": id,
            "name": found
                .and_then(|skill| skill.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(&id),
            "available": found.is_some(),
        }));
    }
    Json(json!({"skills": rows, "missing": missing})).into_response()
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
