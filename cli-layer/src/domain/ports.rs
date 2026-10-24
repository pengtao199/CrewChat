//! Repository and runtime ports used by the app layer.
//!
//! Implementations live in `infra` and `runtime`; app services depend only on
//! these traits so SQLite / HTTP / ACP details do not leak upward.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::domain::agent::Agent;
use crate::domain::engine::EnginePreferences;
use crate::domain::event::EventEnvelope;
use crate::domain::team::{Delegation, TeamSnapshot};

pub trait AgentRepository: Send + Sync {
    fn list(&self) -> Result<Vec<Agent>>;
    fn get(&self, id: &str) -> Result<Option<Agent>>;
    fn save(&self, agent: &Agent) -> Result<Agent>;
    fn delete(&self, id: &str) -> Result<()>;
}

pub trait ConversationRepository: Send + Sync {
    fn list_folders(&self) -> Result<Vec<Value>>;
    fn open_folder(&self, path: &str) -> Result<Value>;
    fn remove_folder(&self, id: &str) -> Result<()>;
    fn list_conversations(&self, folder_ids: &[String]) -> Result<Vec<Value>>;
    fn create_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
    ) -> Result<String>;
    fn get_conversation(&self, id: &str) -> Result<Option<Value>>;
    fn delete_conversation(&self, id: &str) -> Result<()>;
    fn set_external_id(&self, conversation_id: &str, external_id: &str) -> Result<()>;
    fn append_turn(&self, conversation_id: &str, role: &str, blocks: &Value) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    fn append_turn_with_meta(
        &self,
        conversation_id: &str,
        role: &str,
        speaker_role_id: Option<&str>,
        blocks: &Value,
        usage: Option<&Value>,
        duration_ms: Option<u64>,
        model: Option<&str>,
    ) -> Result<()>;
    fn events_since(&self, connection_id: &str, since_seq: u64) -> Result<Vec<Value>>;
    fn events_for_conversation(&self, conversation_id: &str, since_seq: u64) -> Result<Vec<Value>>;
}

/// Team-specific persistence. Kept separate from `ConversationRepository` so
/// single-conversation callers do not grow team concerns.
pub trait TeamRepository: Send + Sync {
    fn create_team_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
        snapshot: &TeamSnapshot,
    ) -> Result<String>;
    fn conversation_agents(&self, conversation_id: &str) -> Result<Vec<Value>>;
    fn add_conversation_member(
        &self,
        conversation_id: &str,
        snapshot: &TeamSnapshot,
        role_id: &str,
    ) -> Result<()>;
    fn conversation_agent(&self, conversation_id: &str, role_id: &str) -> Result<Option<Value>>;
    fn set_conversation_agent_session(
        &self,
        conversation_id: &str,
        role_id: &str,
        external_session_id: Option<&str>,
    ) -> Result<()>;
    fn conversation_cwd(&self, conversation_id: &str) -> Result<Option<String>>;
    fn set_conversation_agent(
        &self,
        conversation_id: &str,
        role_id: &str,
        state: &str,
        connection_id: Option<&str>,
    ) -> Result<()>;
    fn create_delegation(&self, delegation: &Delegation) -> Result<()>;
    fn update_delegation(
        &self,
        id: &str,
        status: &str,
        result: Option<&Value>,
        usage: Option<&Value>,
        child_conversation_id: Option<&str>,
    ) -> Result<()>;
    fn delegation(&self, id: &str) -> Result<Option<Value>>;
    fn delegations(&self, conversation_id: &str) -> Result<Vec<Value>>;
}

pub trait EventRepository: Send + Sync {
    fn append_event(
        &self,
        connection_id: &str,
        conversation_id: Option<&str>,
        envelope: &Value,
    ) -> Result<()>;
}

pub trait EngineSettingsRepository: Send + Sync {
    fn engine_preferences(&self, agent_type: &str) -> Result<EnginePreferences>;
    fn save_engine_preferences(
        &self,
        agent_type: &str,
        enabled: bool,
        env: &BTreeMap<String, String>,
    ) -> Result<()>;
}

/// Inputs needed to start one ACP session. Kept free of runtime types so a
/// fake or remote runtime can implement the same port.
#[derive(Clone, Debug)]
pub struct ConnectRequest {
    pub agent_type: String,
    pub cwd: PathBuf,
    pub role: Option<Value>,
    pub api_key: Option<String>,
    pub conversation_id: Option<String>,
    pub resume_session_id: Option<String>,
    pub engine_env: BTreeMap<String, String>,
    /// Present only for a team Leader. The runtime injects the internal
    /// `delegate_to_agent` / `team_status` MCP server for this connection.
    pub leader_control: Option<LeaderControl>,
}

#[derive(Clone, Debug)]
pub struct LeaderControl {
    pub conversation_id: String,
    pub leader_role_id: String,
}

#[async_trait]
pub trait EventSubscription: Send {
    async fn recv(&mut self) -> Option<EventEnvelope>;
}

#[async_trait]
pub trait SessionRuntime: Send + Sync {
    async fn connect(&self, request: ConnectRequest) -> Result<Value>;
    fn prompt(&self, connection_id: &str, text: String) -> Result<()>;
    fn cancel(&self, connection_id: &str) -> Result<()>;
    fn close(&self, connection_id: &str);
    fn respond_permission(&self, request_id: &str, option_id: Option<String>) -> bool;
    fn subscribe(&self) -> Box<dyn EventSubscription>;
    /// Whether a connection is still alive. Defaults to true for fake runtimes.
    fn is_connected(&self, _connection_id: &str) -> bool {
        true
    }
}

#[async_trait]
pub trait ModelCatalog: Send + Sync {
    fn health(&self) -> Value;
    async fn rpc(&self, method: &str, args: Value) -> Result<Value, String>;
    async fn provider_save(&self, body: &Value) -> Result<Value, String>;
    async fn provider_delete(&self, body: &Value) -> Result<Value, String>;
    async fn provider_probe(&self, body: &Value) -> Result<Value, String>;
    async fn set_model_enabled(
        &self,
        _provider_id: &str,
        _model: &str,
        _enabled: bool,
    ) -> Result<Value, String> {
        Err("model toggle is not supported by this catalog".to_string())
    }
    async fn set_credential_enabled(
        &self,
        _provider_id: &str,
        _credential_id: &str,
        _enabled: bool,
    ) -> Result<Value, String> {
        Err("credential toggle is not supported by this catalog".to_string())
    }
    async fn disabled_models(&self) -> Result<Value, String>;
    async fn model_face(&self, agent_type: &str) -> Result<Value, String>;
    async fn normalize_provider(&self, provider: &str, model: &str) -> Option<String>;
    async fn local_gateway_key(&self) -> Option<String>;
}

#[async_trait]
pub trait McpCatalog: Send + Sync {
    async fn scan(&self) -> Result<Value, String> {
        Err("scan is not supported by this catalog".to_string())
    }
    async fn sync_global(&self) -> Result<Value, String> {
        Err("global sync is not supported by this catalog".to_string())
    }
    async fn list(&self) -> Result<Value, String>;
    async fn read(&self, id: &str) -> Result<Value, String>;
    async fn upsert(&self, id: &str, spec: &Value) -> Result<Value, String>;
    async fn remove(&self, id: &str) -> Result<Value, String>;
    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Value, String>;
    async fn check(&self, id: Option<&str>) -> Result<Value, String>;
    async fn sync_agent(&self, agent_id: &str, server_ids: &[String]) -> Result<Value, String>;
    async fn agent(&self, agent_id: &str) -> Result<Value, String>;
    async fn gateway(&self) -> Result<Value, String>;
}

#[async_trait]
pub trait SkillCatalog: Send + Sync {
    async fn list(&self) -> Result<Value, String>;
    async fn detail(&self, id: &str) -> Result<Value, String>;
    async fn install(&self, body: &Value) -> Result<Value, String>;
    async fn check(&self, id: &str) -> Result<Value, String>;
    async fn update(&self, id: &str) -> Result<Value, String>;
    async fn remove(&self, id: &str) -> Result<Value, String>;
    async fn collections(&self) -> Result<Value, String>;
    async fn global_links(&self) -> Result<Value, String>;
    async fn materialize(
        &self,
        home: &Path,
        agent_type: &str,
        skill_ids: &[String],
    ) -> Result<Value, String>;
}

pub trait EventPublisher: Send + Sync {
    fn emit(&self, event: EventEnvelope);
}
