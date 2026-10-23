//! Port implementations backed by SQLite.

use anyhow::Result;
use serde_json::Value;

use crate::domain::agent::Agent;
use crate::domain::engine::EnginePreferences;
use crate::domain::ports::{
    AgentRepository, ConversationRepository, EngineSettingsRepository, EventRepository,
    TeamRepository,
};
use crate::domain::team::{Delegation, TeamSnapshot};
use crate::infra::store::Store;

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

    fn append_turn(&self, conversation_id: &str, role: &str, blocks: &Value) -> Result<()> {
        self.append_turn(conversation_id, role, blocks)
    }

    fn append_turn_with_speaker(
        &self,
        conversation_id: &str,
        role: &str,
        speaker_role_id: Option<&str>,
        blocks: &Value,
    ) -> Result<()> {
        self.append_turn_with_speaker(conversation_id, role, speaker_role_id, blocks)
    }

    fn events_since(&self, connection_id: &str, since_seq: u64) -> Result<Vec<Value>> {
        self.events_since(connection_id, since_seq)
    }
}

impl EventRepository for Store {
    fn append_event(
        &self,
        connection_id: &str,
        conversation_id: Option<&str>,
        envelope: &Value,
    ) -> Result<()> {
        self.append_event(connection_id, conversation_id, envelope)
            .map(|_| ())
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

impl TeamRepository for Store {
    fn create_team_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
        snapshot: &TeamSnapshot,
    ) -> Result<String> {
        self.create_team_conversation(folder_id, agent_type, title, snapshot)
    }

    fn conversation_agents(&self, conversation_id: &str) -> Result<Vec<Value>> {
        self.conversation_agents(conversation_id)
    }

    fn set_conversation_agent(
        &self,
        conversation_id: &str,
        role_id: &str,
        state: &str,
        connection_id: Option<&str>,
    ) -> Result<()> {
        self.set_conversation_agent(conversation_id, role_id, state, connection_id)
    }

    fn create_delegation(&self, delegation: &Delegation) -> Result<()> {
        self.create_delegation(delegation)
    }

    fn update_delegation(
        &self,
        id: &str,
        status: &str,
        result: Option<&Value>,
        usage: Option<&Value>,
        child_conversation_id: Option<&str>,
    ) -> Result<()> {
        self.update_delegation(id, status, result, usage, child_conversation_id)
    }

    fn delegations(&self, conversation_id: &str) -> Result<Vec<Value>> {
        self.delegations(conversation_id)
    }
}
