//! HTTP compatibility surface for AgentOS.
//!
//! Phase 1 implements health, model proxy, roster, folders/conversations and the
//! ACP session path. Skills/MCP/engine-install/delegation return a structured
//! `not_implemented` until their phases land.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use serde_json::{json, Value};

use crate::acp::host::AcpHost;
use crate::config::Config;
use crate::engine_manager;
use crate::model_proxy::ModelLayerClient;
use crate::skill_proxy::SkillLayerClient;
use crate::store::Store;
use crate::ws;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Store,
    pub host: AcpHost,
    pub model: Arc<ModelLayerClient>,
    pub skill: Arc<SkillLayerClient>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", post(health))
        .route("/api/ccr_health", post(ccr_health))
        .route("/api/ccr_call", post(ccr_call))
        .route("/api/ccr_provider_save", post(ccr_provider_save))
        .route("/api/ccr_provider_delete", post(ccr_provider_delete))
        .route("/api/ccr_provider_probe", post(ccr_provider_probe))
        .route("/api/ccr_disabled_models", post(ccr_disabled_models))
        .route("/api/ccr_supervisor_start", post(ccr_supervisor_start))
        .route("/api/acp_list_agents", post(acp_list_agents))
        .route("/api/acp_connect", post(acp_connect))
        .route("/api/acp_prompt", post(acp_prompt))
        .route("/api/acp_cancel", post(acp_cancel))
        .route("/api/acp_disconnect", post(acp_disconnect))
        .route("/api/acp_respond_permission", post(acp_respond_permission))
        .route("/api/agent_roles_list", post(agent_roles_list))
        .route("/api/agent_roles_save", post(agent_roles_save))
        .route("/api/agent_roles_delete", post(agent_roles_delete))
        .route("/api/agent_role_model_face", post(agent_role_model_face))
        .route("/api/list_open_folder_details", post(list_folders))
        .route("/api/open_folder", post(open_folder))
        .route("/api/remove_folder_from_workspace", post(remove_folder))
        .route("/api/list_all_conversations", post(list_conversations))
        .route("/api/create_conversation", post(create_conversation))
        .route("/api/get_folder_conversation", post(get_conversation))
        .route("/api/get_conversation", post(get_conversation))
        .route("/api/delete_conversation", post(delete_conversation))
        .route("/api/acp_list_agent_skills", post(acp_list_agent_skills))
        .route("/api/acp_read_agent_skill", post(acp_read_agent_skill))
        .route("/api/acp_save_agent_skill", post(not_implemented))
        .route("/api/acp_delete_agent_skill", post(not_implemented))
        .route("/api/skill_list", post(skill_list))
        .route("/api/skill_read", post(skill_read))
        .route("/api/skill_install", post(skill_install))
        .route("/api/skill_check", post(skill_check))
        .route("/api/skill_update", post(skill_update))
        .route("/api/skill_remove", post(skill_remove))
        .route("/api/skill_link_status", post(skill_link_status))
        .route("/api/agent_skills_set", post(agent_skills_set))
        .route("/api/agent_skill_status", post(agent_skill_status))
        .route("/api/mcp_scan_local", post(not_implemented))
        .route("/api/mcp_upsert_local_server", post(not_implemented))
        .route("/api/mcp_remove_server", post(not_implemented))
        .route(
            "/api/acp_update_agent_preferences",
            post(acp_update_agent_preferences),
        )
        .route(
            "/api/acp_fetch_agent_latest_release",
            post(acp_fetch_agent_latest_release),
        )
        .route("/api/acp_prepare_npx_agent", post(acp_prepare_npx_agent))
        .route(
            "/api/acp_download_agent_binary",
            post(acp_download_agent_binary),
        )
        .route("/api/acp_install_pi_binary", post(acp_install_pi_binary))
        .route("/api/acp_env_diagnostics", post(acp_env_diagnostics))
        .route("/api/get_delegation_settings", post(not_implemented))
        .route("/api/set_delegation_settings", post(not_implemented))
        .route("/ws/events", get(events_ws))
        .with_state(state)
}

async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")})).into_response()
}

async fn ccr_health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(state.model.health()).into_response()
}

async fn ccr_call(
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

async fn ccr_provider_save(
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

async fn ccr_provider_delete(
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

async fn ccr_provider_probe(
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

async fn ccr_disabled_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.model.disabled_models().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error),
    }
}

async fn ccr_supervisor_start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    Json(json!({"state": "running", "endpoint": state.model.health()["url"]})).into_response()
}

async fn acp_list_agents(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let mut agents = state.host.engines().list();
    for agent in &mut agents {
        if let Some(agent_type) = agent.get("agent_type").and_then(Value::as_str) {
            if let Ok(preferences) = state.store.engine_preferences(agent_type) {
                agent["enabled"] = json!(preferences.enabled);
                agent["env"] = json!(preferences.env);
            }
        }
    }
    Json(agents).into_response()
}

async fn acp_connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or("claude_code");
    let preferences = state
        .store
        .engine_preferences(agent_type)
        .unwrap_or_default();
    if !preferences.enabled {
        return api_error(StatusCode::BAD_REQUEST, "this engine is disabled");
    }
    let cwd = field(&body, &["workingDir", "working_dir"])
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));
    let mut role =
        field(&body, &["roleId", "role_id"]).and_then(|id| state.store.role(id).ok().flatten());
    if let Some(role) = role.as_mut() {
        if let Some(provider) = role
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let model = role
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(display) = state.model.normalize_provider(&provider, &model).await {
                role["model_provider"] = json!(display);
            }
        }
    }
    let api_key = state.model.local_gateway_key().await;
    match state
        .host
        .connect(
            agent_type,
            cwd,
            role,
            api_key,
            field(&body, &["conversationId", "conversation_id"]).map(str::to_string),
            preferences.env,
        )
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

async fn acp_prompt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let connection_id = field(&body, &["connectionId", "connection_id"]).unwrap_or_default();
    let text = body
        .get("blocks")
        .map(blocks_to_text)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| field(&body, &["text"]).map(str::to_string))
        .unwrap_or_default();
    if text.trim().is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "prompt has no text");
    }
    if let Some(conversation_id) = field(&body, &["conversationId", "conversation_id"]) {
        let _ = state.store.append_turn(
            conversation_id,
            "user",
            &json!([{"type": "text", "text": text.clone()}]),
        );
    }
    match state.host.prompt(connection_id, text) {
        Ok(()) => Json(json!({})).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn acp_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let connection_id = field(&body, &["connectionId", "connection_id"]).unwrap_or_default();
    match state.host.cancel(connection_id) {
        Ok(()) => Json(json!({})).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn acp_disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let connection_id = field(&body, &["connectionId", "connection_id"]).unwrap_or_default();
    state.host.close(connection_id);
    Json(json!({})).into_response()
}

async fn acp_respond_permission(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request_id = field(&body, &["requestId", "request_id"]).unwrap_or_default();
    let option_id = field(&body, &["optionId", "option_id"]).map(str::to_string);
    if state.host.respond_permission(request_id, option_id) {
        Json(json!(true)).into_response()
    } else {
        api_error(StatusCode::NOT_FOUND, "unknown permission request")
    }
}

async fn acp_update_agent_preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    if agent_type.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "agentType is required");
    }
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let env: BTreeMap<String, String> = body
        .get("env")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    match state
        .store
        .save_engine_preferences(agent_type, enabled, &env)
    {
        Ok(()) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn acp_fetch_agent_latest_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    match engine_manager::latest_version(&state.config, agent_type).await {
        Ok(Some(version)) => Json(json!({"version": version})).into_response(),
        Ok(None) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn acp_prepare_npx_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    let version = field(&body, &["version", "registryVersion", "registry_version"]);
    let clean_first = body
        .get("cleanFirst")
        .or_else(|| body.get("clean_first"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match engine_manager::prepare_npx(&state.config, agent_type, version, clean_first).await {
        Ok(version) => Json(json!(version)).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn acp_download_agent_binary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    let version = field(&body, &["version"]);
    match engine_manager::download_binary(&state.config, agent_type, version).await {
        Ok(version) => Json(json!(version)).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn acp_install_pi_binary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match engine_manager::install_pi_binary().await {
        Ok(_) => Json(Value::Null).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn acp_env_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or_default();
    Json(engine_manager::diagnostics(&state.config, agent_type).await).into_response()
}

async fn agent_roles_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.store.list_roles() {
        Ok(roles) => Json(roles).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn agent_roles_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role = body.get("role").cloned().unwrap_or(body);
    match state.store.save_role(&role) {
        Ok(role) => Json(role).into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn agent_roles_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["id"]).unwrap_or_default();
    match state.store.delete_role(id) {
        Ok(()) => Json(json!(true)).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn agent_role_model_face(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let agent_type = field(&body, &["agentType", "agent_type"]).unwrap_or("claude_code");
    if let Some(spec) = state.host.engines().find(agent_type) {
        if spec.overlay == crate::engines::OverlayKind::None {
            return Json(json!({
                "supported": false,
                "reason": format!(
                    "{} does not expose a per-session API endpoint for model injection",
                    spec.name
                ),
            }))
            .into_response();
        }
    }
    match state.model.model_face(agent_type).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(StatusCode::BAD_GATEWAY, &error),
    }
}

async fn acp_list_agent_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.skill.list().await {
        Ok(value) => {
            let skills = value
                .get("skills")
                .and_then(Value::as_array)
                .map(|rows| rows.iter().map(legacy_skill).collect::<Vec<_>>())
                .unwrap_or_default();
            Json(json!({"supported": true, "message": Value::Null, "skills": skills}))
                .into_response()
        }
        Err(error) => Json(json!({
            "supported": false,
            "message": error,
            "skills": [],
        }))
        .into_response(),
    }
}

async fn acp_read_agent_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let id = field(&body, &["skillId", "skill_id", "id"]).unwrap_or_default();
    match state.skill.detail(id).await {
        Ok(detail) => {
            let skill = detail.get("skill").cloned().unwrap_or(Value::Null);
            Json(json!({
                "skill": legacy_skill(&skill),
                "content": detail.get("content").cloned().unwrap_or(Value::Null),
            }))
            .into_response()
        }
        Err(error) => api_error(StatusCode::NOT_FOUND, &error),
    }
}

fn legacy_skill(skill: &Value) -> Value {
    let id = skill.get("id").and_then(Value::as_str).unwrap_or_default();
    let root = std::env::var_os("SKILL_LAYER_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".agents")));
    let path = root
        .map(|root| root.join("skills").join(id))
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    json!({
        "id": id,
        "name": skill.get("name").and_then(Value::as_str).unwrap_or(id),
        "scope": "global",
        "layout": "directory",
        "path": path,
        "description": skill.get("description").cloned().unwrap_or(Value::Null),
        "read_only": false,
    })
}

async fn skill_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
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

async fn skill_read(
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

async fn skill_install(
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

async fn skill_check(
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

async fn skill_update(
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

async fn skill_remove(
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

async fn skill_link_status(
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

async fn agent_skills_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    let Some(mut role) = state.store.role(role_id).ok().flatten() else {
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
    role["skills"] = json!(ids);
    match state.store.save_role(&role) {
        Ok(role) => Json(json!({
            "roleId": role.get("id").cloned().unwrap_or(Value::Null),
            "skillIds": role.get("skills").cloned().unwrap_or_else(|| json!([])),
        }))
        .into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

async fn agent_skill_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let role_id = field(&body, &["roleId", "role_id"]).unwrap_or_default();
    let Some(role) = state.store.role(role_id).ok().flatten() else {
        return api_error(StatusCode::NOT_FOUND, "role not found");
    };
    let selected = string_array(role.get("skills"));
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

async fn list_folders(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    match state.store.list_folders() {
        Ok(folders) => Json(folders).into_response(),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn open_folder(
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

async fn remove_folder(
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

async fn list_conversations(
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

async fn create_conversation(
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

async fn get_conversation(
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

async fn delete_conversation(
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

async fn not_implemented(headers: HeaderMap) -> Response {
    let _ = headers;
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "code": "not_implemented",
            "message": "This command is scheduled for a later cli-layer phase.",
        })),
    )
        .into_response()
}

async fn events_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !ws_authorized(&state, &headers) {
        return unauthorized();
    }
    ws.on_upgrade(move |socket| ws::serve(state, socket))
}

fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| value.trim() == state.config.token)
        .unwrap_or(false)
}

fn ws_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    if authorized(state, headers) {
        return true;
    }
    let Some(protocol) = headers.get("sec-websocket-protocol") else {
        return false;
    };
    let Ok(protocol) = protocol.to_str() else {
        return false;
    };
    for part in protocol.split(',') {
        let Some(token) = part.trim().strip_prefix("codeg-token.") else {
            continue;
        };
        if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token) {
            if String::from_utf8(bytes).ok().as_deref() == Some(state.config.token.as_str()) {
                return true;
            }
        }
    }
    false
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"code": "unauthorized", "message": "invalid token"})),
    )
        .into_response()
}

pub fn api_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"code": "api_error", "message": message})),
    )
        .into_response()
}

fn field<'a>(body: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| body.get(*name).and_then(Value::as_str))
}

fn blocks_to_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
