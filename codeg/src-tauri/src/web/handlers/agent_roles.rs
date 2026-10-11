//! [crewchat] Web-mode handlers for the agent role roster.
//!
//! See `crate::commands::agent_roles` for the storage contract.

use axum::Json;
use serde::Deserialize;

use crate::app_error::AppCommandError;
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

pub async fn agent_roles_list() -> Result<Json<Vec<AgentRole>>, AppCommandError> {
    Ok(Json(roles_commands::agent_roles_list_core()?))
}

pub async fn agent_roles_save(
    Json(params): Json<AgentRoleSaveParams>,
) -> Result<Json<AgentRole>, AppCommandError> {
    Ok(Json(roles_commands::agent_roles_save_core(params.role)?))
}

pub async fn agent_roles_delete(
    Json(params): Json<AgentRoleDeleteParams>,
) -> Result<Json<bool>, AppCommandError> {
    Ok(Json(roles_commands::agent_roles_delete_core(&params.id)?))
}
