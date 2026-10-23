//! Repository and runtime ports used by the app layer.
//!
//! Implementations live in `infra` and `runtime`; app services depend only on
//! these traits so SQLite / HTTP / ACP details do not leak upward.

use anyhow::Result;
use serde_json::Value;

use crate::domain::agent::Agent;

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
}

pub trait EngineSettingsRepository: Send + Sync {
    fn engine_preferences(
        &self,
        agent_type: &str,
    ) -> Result<crate::infra::store::EnginePreferences>;
    fn save_engine_preferences(
        &self,
        agent_type: &str,
        enabled: bool,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<()>;
}
