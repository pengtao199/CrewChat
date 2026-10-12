//! [crewchat] Web-mode handlers for the agent role roster.
//!
//! See `crate::commands::agent_roles` for the storage contract (Phase 2.1:
//! `custom_agent` rows with `kind = 'role'`).

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::agent_roles as roles_commands;
use crate::commands::agent_roles::AgentRole;

#[derive(Deserialize)]
pub struct AgentRoleSaveParams {
    pub role: AgentRole,
}

#[derive(Deserialize)]
pub struct AgentRoleDeleteParams {
    pub id: String,
}

pub async fn agent_roles_list(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<AgentRole>>, AppCommandError> {
    Ok(Json(roles_commands::agent_roles_list_core(&state.db).await?))
}

pub async fn agent_roles_save(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AgentRoleSaveParams>,
) -> Result<Json<AgentRole>, AppCommandError> {
    Ok(Json(
        roles_commands::agent_roles_save_core(&state.db, params.role).await?,
    ))
}

pub async fn agent_roles_delete(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AgentRoleDeleteParams>,
) -> Result<Json<bool>, AppCommandError> {
    Ok(Json(
        roles_commands::agent_roles_delete_core(&state.db, &params.id).await?,
    ))
}
