//! Port implementations backed by SQLite.

use anyhow::Result;
use serde_json::Value;

use crate::domain::agent::Agent;
use crate::domain::ports::{AgentRepository, ConversationRepository, EngineSettingsRepository};
use crate::infra::store::{EnginePreferences, Store};

impl AgentRepository for Store {
    fn list(&self) -> Result<Vec<Agent>> {
        self.list_roles()?
            .into_iter()
            .map(Agent::from_value)
            .collect()
    }

    fn get(&self, id: &str) -> Result<Option<Agent>> {
        self.role(id)?.map(Agent::from_value).transpose()
    }

    fn save(&self, agent: &Agent) -> Result<Agent> {
        let value = self.save_role(&agent.to_value()?)?;
        Agent::from_value(value)
    }

    fn delete(&self, id: &str) -> Result<()> {
        self.delete_role(id)
    }
}

impl ConversationRepository for Store {
    fn list_folders(&self) -> Result<Vec<Value>> {
        self.list_folders()
    }

    fn open_folder(&self, path: &str) -> Result<Value> {
        self.open_folder(path)
    }

    fn remove_folder(&self, id: &str) -> Result<()> {
        self.remove_folder(id)
    }

    fn list_conversations(&self, folder_ids: &[String]) -> Result<Vec<Value>> {
        self.list_conversations(folder_ids)
    }

    fn create_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
    ) -> Result<String> {
        self.create_conversation(folder_id, agent_type, title)
    }

    fn get_conversation(&self, id: &str) -> Result<Option<Value>> {
        self.get_conversation(id)
    }

    fn delete_conversation(&self, id: &str) -> Result<()> {
        self.delete_conversation(id)
    }
}

impl EngineSettingsRepository for Store {
    fn engine_preferences(&self, agent_type: &str) -> Result<EnginePreferences> {
        self.engine_preferences(agent_type)
    }

    fn save_engine_preferences(
        &self,
        agent_type: &str,
        enabled: bool,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        self.save_engine_preferences(agent_type, enabled, env)
    }
}
