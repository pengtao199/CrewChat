//! [crewchat] Agent role roster — the fork's core "Agents" concept.
//!
//! Upstream codeg configures *CLI types* (`agent_setting`), the global skill /
//! MCP matrices and the model providers. This module adds the layer above
//! them: user-owned **roles** (e.g. "写作") that bind one body (CLI), one
//! model, a subset of the machine's local skills / MCP servers, and the two
//! documents that define the role — `Soul.md` (personality) and `Agents.md`
//! (rules).
//!
//! - IDs are server-generated UUIDs; the UI never shows them.
//! - The roster is a single JSON file in the codeg data dir
//!   (`<data dir>/agent-roles.json`) — no upstream DB migration; writes are
//!   atomic (temp file + rename).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app_error::{AppCommandError, AppErrorCode};

const AGENT_ROLES_FILE: &str = "agent-roles.json";
const STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRole {
    /// Server-generated UUID. Empty on create.
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// `Soul.md` — the role's personality.
    #[serde(default)]
    pub soul: String,
    /// `Agents.md` — the role's rules.
    #[serde(default)]
    pub rules: String,
    /// Body: the CLI agent type (e.g. `claude_code`).
    pub agent_type: String,
    #[serde(default)]
    pub model: String,
    /// CCR provider the model belongs to (the picker's left-hand column).
    #[serde(default)]
    pub model_provider: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp: Vec<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn store_version() -> u32 {
    STORE_VERSION
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentRoleStore {
    #[serde(default = "store_version")]
    version: u32,
    #[serde(default)]
    roles: Vec<AgentRole>,
}

impl Default for AgentRoleStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            roles: Vec::new(),
        }
    }
}

/// `<data dir>/agent-roles.json`. Resolution mirrors `crate::paths`
/// (`$CODEG_HOME` → `$CODEG_DATA_DIR` → `~/.codeg`).
fn store_path() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEG_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(AGENT_ROLES_FILE);
    }
    if let Some(data) = std::env::var_os("CODEG_DATA_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(data).join(AGENT_ROLES_FILE);
    }
    dirs::home_dir()
        .map(|home| home.join(".codeg").join(AGENT_ROLES_FILE))
        .unwrap_or_else(|| PathBuf::from(".codeg").join(AGENT_ROLES_FILE))
}

fn io_error(message: String) -> AppCommandError {
    AppCommandError::new(AppErrorCode::IoError, message)
}

fn read_store() -> Result<AgentRoleStore, AppCommandError> {
    let path = store_path();
    if !path.exists() {
        return Ok(AgentRoleStore::default());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|error| io_error(format!("Failed to read {}: {error}", path.display())))?;
    if raw.trim().is_empty() {
        return Ok(AgentRoleStore::default());
    }
    serde_json::from_str(&raw).map_err(|error| {
        AppCommandError::invalid_input(format!("{} is not valid JSON: {error}", path.display()))
    })
}

fn write_store(store: &AgentRoleStore) -> Result<(), AppCommandError> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error(format!("Failed to create {}: {error}", parent.display())))?;
    }
    let json = serde_json::to_string_pretty(store)
        .map_err(|error| io_error(format!("Failed to serialize agent roles: {error}")))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json.as_bytes())
        .map_err(|error| io_error(format!("Failed to write {}: {error}", tmp.display())))?;
    fs::rename(&tmp, &path)
        .map_err(|error| io_error(format!("Failed to replace {}: {error}", path.display())))?;
    Ok(())
}

pub fn agent_roles_list_core() -> Result<Vec<AgentRole>, AppCommandError> {
    Ok(read_store()?.roles)
}

pub fn agent_roles_save_core(mut role: AgentRole) -> Result<AgentRole, AppCommandError> {
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
    let now = chrono::Utc::now().to_rfc3339();
    let mut store = read_store()?;
    role.id = role.id.trim().to_string();
    if role.id.is_empty() {
        role.id = uuid::Uuid::new_v4().to_string();
    }
    match store
        .roles
        .iter_mut()
        .find(|existing| existing.id == role.id)
    {
        Some(existing) => {
            role.created_at = existing.created_at.clone().or_else(|| Some(now.clone()));
            role.updated_at = Some(now);
            *existing = role.clone();
        }
        None => {
            role.created_at = Some(now.clone());
            role.updated_at = Some(now);
            store.roles.push(role.clone());
        }
    }
    write_store(&store)?;
    Ok(role)
}

pub fn agent_roles_delete_core(id: &str) -> Result<bool, AppCommandError> {
    let mut store = read_store()?;
    let before = store.roles.len();
    store.roles.retain(|role| role.id != id);
    let removed = store.roles.len() != before;
    if removed {
        write_store(&store)?;
    }
    Ok(removed)
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub fn agent_roles_list() -> Result<Vec<AgentRole>, AppCommandError> {
    agent_roles_list_core()
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub fn agent_roles_save(role: AgentRole) -> Result<AgentRole, AppCommandError> {
    agent_roles_save_core(role)
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub fn agent_roles_delete(id: String) -> Result<bool, AppCommandError> {
    agent_roles_delete_core(&id)
}
