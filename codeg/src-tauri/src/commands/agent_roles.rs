//! [crewchat] Agent role roster — the core "Agents" concept of this fork.
//!
//! A role binds the global capabilities (CLI body, model, local skills, local
//! MCP servers) to the two documents that define it: `Soul.md` (personality)
//! and `Agents.md` (rules). IDs are server-generated UUIDs; the UI never shows
//! them.
//!
//! Phase 2.1: storage is the custom-agent registry's `kind = 'role'` rows
//! (`db::service::agent_role_service`) — the single writer for roles. The
//! wire shape ([`AgentRole`]) is unchanged from the pre-2.1 file store, so
//! every consumer (settings page, team picker, member strip, delegation
//! scope gate) kept its own code untouched.

use crate::app_error::AppCommandError;
use crate::db::service::agent_role_service;
use crate::db::AppDatabase;

pub use crate::models::agent_role::AgentRole;

pub async fn agent_roles_list_core(db: &AppDatabase) -> Result<Vec<AgentRole>, AppCommandError> {
    Ok(agent_role_service::list_roles(&db.conn).await?)
}

pub async fn agent_roles_save_core(
    db: &AppDatabase,
    mut role: AgentRole,
) -> Result<AgentRole, AppCommandError> {
    role.name = role.name.trim().to_string();
    role.agent_type = role.agent_type.trim().to_string();
    if role.name.is_empty() {
        return Err(AppCommandError::invalid_input("Agent name is required"));
    }
    if role.agent_type.is_empty() {
        return Err(AppCommandError::invalid_input(
            "Agent body (agent_type) is required",
        ));
    }
    Ok(agent_role_service::save_role(&db.conn, role).await?)
}

pub async fn agent_roles_delete_core(
    db: &AppDatabase,
    id: &str,
) -> Result<bool, AppCommandError> {
    Ok(agent_role_service::delete_role(&db.conn, id).await?)
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn agent_roles_list(
    db: tauri::State<'_, AppDatabase>,
) -> Result<Vec<AgentRole>, AppCommandError> {
    agent_roles_list_core(&db).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn agent_roles_save(
    role: AgentRole,
    db: tauri::State<'_, AppDatabase>,
) -> Result<AgentRole, AppCommandError> {
    agent_roles_save_core(&db, role).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn agent_roles_delete(
    id: String,
    db: tauri::State<'_, AppDatabase>,
) -> Result<bool, AppCommandError> {
    agent_roles_delete_core(&db, &id).await
}
