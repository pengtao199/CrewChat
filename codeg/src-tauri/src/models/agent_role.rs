//! [crewchat] The roster wire type (Phase 2.1).
//!
//! A role binds global capabilities (CLI body, brain model, local skills,
//! local MCP servers) to the two documents that define it: `Soul.md`
//! (personality — stored as `instructions`) and `Agents.md` (rules). Storage
//! is the `custom_agent` table's `kind = 'role'` rows
//! (`db::service::agent_role_service`); this struct is the shape the whole
//! frontend and the team surfaces speak, kept identical to the pre-2.1 file
//! store so the migration is invisible on the wire.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRole {
    /// Server-generated UUID. Empty on create.
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// `Soul.md` — the role's personality (the registry's `instructions`).
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
