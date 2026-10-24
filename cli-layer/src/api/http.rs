//! HTTP compatibility surface for AgentOS.
//!
//! Phase 1 implements health, model proxy, roster, folders/conversations and the
//! ACP session path. Skills/MCP/engine-install/delegation return a structured
//! `not_implemented` until their phases land.

use std::sync::Arc;

use axum::{
    extract::{State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;

use crate::api::handlers::*;
use crate::app::engines::EngineService;
use crate::app::mcp::McpService;
use crate::app::models::ModelService;
use crate::app::permissions::PermissionService;
use crate::app::readiness::ReadinessService;
use crate::app::roster::RosterService;
use crate::app::sessions::SessionService;
use crate::app::skills::SkillService;
use crate::app::team::TeamService;
use crate::app::workspace::WorkspaceService;
use serde_json::{json, Value};

use crate::api::ws;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub models: ModelService,
    pub roster: RosterService,
    pub workspace: WorkspaceService,
    pub skills: SkillService,
    pub mcp: McpService,
    pub readiness: ReadinessService,
    pub permissions: PermissionService,
    pub engines: EngineService,
    pub sessions: SessionService,
    pub team: TeamService,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", post(health))
        .route("/api/model_health", post(model_health))
        .route("/api/model_call", post(model_call))
        .route("/api/model_provider_save", post(model_provider_save))
        .route("/api/model_provider_delete", post(model_provider_delete))
        .route("/api/model_provider_probe", post(model_provider_probe))
        .route(
            "/api/model_provider_set_model",
            post(model_provider_set_model),
        )
        .route(
            "/api/model_provider_set_credential",
            post(model_provider_set_credential),
        )
        .route("/api/model_disabled_models", post(model_disabled_models))
        .route("/api/model_supervisor_start", post(model_supervisor_start))
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
        .route(
            "/api/agent_role_permission_face",
            post(agent_role_permission_face),
        )
        .route("/api/agent_role_readiness", post(agent_role_readiness))
        .route("/api/agent_role_test_run", post(agent_role_test_run))
        .route("/api/list_open_folder_details", post(list_folders))
        .route("/api/open_folder", post(open_folder))
        .route("/api/remove_folder_from_workspace", post(remove_folder))
        .route("/api/list_all_conversations", post(list_conversations))
        .route("/api/create_conversation", post(create_conversation))
        .route("/api/get_folder_conversation", post(get_conversation))
        .route("/api/get_conversation", post(get_conversation))
        .route("/api/team_status", post(team_status))
        .route("/api/cancel_delegation", post(cancel_delegation))
        .route("/api/cancel_conversation", post(cancel_conversation))
        .route("/api/retry_delegation", post(retry_delegation))
        .route("/api/internal/team_delegate", post(team_delegate))
        .route("/api/delete_conversation", post(delete_conversation))
        .route("/api/skill_list", post(skill_list))
        .route("/api/skill_read", post(skill_read))
        .route("/api/skill_install", post(skill_install))
        .route("/api/skill_check", post(skill_check))
        .route("/api/skill_update", post(skill_update))
        .route("/api/skill_remove", post(skill_remove))
        .route("/api/skill_link_status", post(skill_link_status))
        .route("/api/agent_skills_set", post(agent_skills_set))
        .route("/api/agent_skill_status", post(agent_skill_status))
        .route("/api/mcp_list", post(mcp_list))
        .route("/api/mcp_scan", post(mcp_scan))
        .route("/api/mcp_global_sync", post(mcp_global_sync))
        .route("/api/mcp_read", post(mcp_read))
        .route("/api/mcp_upsert", post(mcp_upsert))
        .route("/api/mcp_remove", post(mcp_remove))
        .route("/api/mcp_check", post(mcp_check))
        .route("/api/mcp_enable", post(mcp_enable))
        .route("/api/mcp_disable", post(mcp_disable))
        .route("/api/mcp_gateway", post(mcp_gateway))
        .route("/api/agent_mcps_set", post(agent_mcps_set))
        .route("/api/agent_mcp_status", post(agent_mcp_status))
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

pub(crate) fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
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
        let Some(token) = part.trim().strip_prefix("crewchat-token.") else {
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

pub(crate) fn field<'a>(body: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| body.get(*name).and_then(Value::as_str))
}

pub(crate) fn blocks_to_text(blocks: &Value) -> String {
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
