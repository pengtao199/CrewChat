use sea_orm::entity::prelude::*;

/// [crewchat] Row kinds (Phase 2.1). `KIND_ACP` is the original concept — a
/// launchable external ACP agent; `KIND_ROLE` is a crewchat roster role that
/// shares the table but never launches as its own ACP server.
pub const KIND_ACP: &str = "acp";
pub const KIND_ROLE: &str = "role";

/// A user-registered ACP agent. See `crate::acp::custom_registry` for how a row
/// becomes launch metadata, and `crate::models::agent::AgentType::Custom` for
/// how it is referenced from conversations.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "custom_agent")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// ACP registry id — the agent's identity everywhere else in codeg.
    #[sea_orm(unique)]
    pub registry_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// `npx` | `uvx` | `binary`.
    pub distribution_kind: String,
    /// The ACP registry `distribution` object, verbatim.
    pub spec_json: String,
    pub icon_url: Option<String>,
    /// User declaration that the agent reads the shared `.agents/skills`
    /// store — with `skills_dir`, the gate for every skills surface. See
    /// `CustomAgentDef::skills_shared_store`.
    pub skills_shared_store: bool,
    /// Absolute path of the agent's own skills directory, when declared. See
    /// `CustomAgentDef::skills_dir`.
    pub skills_dir: Option<String>,
    /// `registry` | `manual`. See `CustomAgentDef::source`.
    pub source: String,
    /// Optional command that prints the locally installed version. See
    /// `CustomAgentDef::version_probe`.
    pub version_probe: Option<String>,
    /// Whether MCP servers may be forwarded to the agent over the ACP wire.
    /// See `CustomAgentDef::supports_mcp`.
    pub supports_mcp: bool,
    /// [crewchat] `acp` (the original launchable agent) or `role` (a crewchat
    /// roster role; never launched as its own ACP server). See
    /// `m20261012_000002_custom_agent_role_fields`.
    pub kind: String,
    /// [crewchat] Role rows only: the built-in CLI body's wire slug.
    pub body: Option<String>,
    /// [crewchat] Role rows only: the Soul document (personality).
    pub instructions: String,
    /// [crewchat] Role rows only: the Agents.md rules document.
    pub rules: String,
    /// [crewchat] Role rows only: brain model name.
    pub brain_model: String,
    /// [crewchat] Role rows only: brain model's CCR provider.
    pub brain_provider: String,
    /// [crewchat] Role rows only: JSON array of skill names.
    pub skills_json: String,
    /// [crewchat] Role rows only: JSON array of MCP server names.
    pub mcp_json: String,
    /// [crewchat] Role rows only: the session-mode id the role's sessions start
    /// in (a CLI approval/sandbox preset such as codex's `workspace-write`).
    /// Empty = leave whatever the CLI / CCR default is. See
    /// `m20261019_000001_custom_agent_role_permissions`.
    pub permission_mode: String,
    /// [crewchat] Role rows only: answer this role's permission requests
    /// automatically instead of interrupting the conversation. OFF by default —
    /// auto-approving is a trust decision the user makes per Agent, not a
    /// default this app should assume.
    pub auto_approve: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
