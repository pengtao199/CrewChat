use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::http::{api_error, authorized, field, unauthorized, AppState};

pub(crate) async fn skill_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let skills = state.skill.list().await;
    let collections = state.skill.collections().await;
    let links = state.skill.global_links().await;
    match (skills, collections, links) {
        (Ok(skills), Ok(collections), Ok(links)) => Json(json!({
            "skills": skills.get("skills").cloned().unwrap_or_else(|| json!([])),
            "collections": collections.get("collections").cloned().unwrap_or_else(|| json!([])),
            "globalLinks": links.get("links").cloned().unwrap_or_else(|| json!([])),
        }))
        .into_response(),
        (skills, collections, links) => api_error(
            StatusCode::BAD_GATEWAY,
            &skills
                .err()
                .or_else(|| collections.err())
                .or_else(|| links.err())
                .unwrap_or_else(|| "skill-layer request failed".to_string()),
        ),
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
    match state.skill.detail(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error),
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
    match state.skill.install(&body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
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
    match state.skill.check(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
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
    match state.skill.update(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
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
    match state.skill.remove(id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error),
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
    match state.skill.global_links().await {
        Ok(value) => {
            let links = value
                .get("links")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|link| {
                    agent_type.is_none_or(|agent| {
                        link.get("agentType")
                            .or_else(|| link.get("agent_type"))
                            .and_then(Value::as_str)
                            == Some(agent)
                    })
                })
                .collect::<Vec<_>>();
            Json(json!({"links": links})).into_response()
        }
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error),
    }
}
