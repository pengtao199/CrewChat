//! Engine preferences and listing use cases.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use crate::domain::ports::EngineSettingsRepository;
use crate::infra::store::EnginePreferences;
use crate::runtime::engine::registry::Engines;

#[derive(Clone)]
pub struct EngineService {
    repo: Arc<dyn EngineSettingsRepository>,
    engines: Engines,
}

impl EngineService {
    pub fn new(repo: Arc<dyn EngineSettingsRepository>, engines: Engines) -> Self {
        Self { repo, engines }
    }

    pub fn list(&self) -> Vec<Value> {
        let mut agents = self.engines.list();
        for agent in &mut agents {
            if let Some(agent_type) = agent.get("agent_type").and_then(Value::as_str) {
                if let Ok(preferences) = self.repo.engine_preferences(agent_type) {
                    agent["enabled"] = json!(preferences.enabled);
                    agent["env"] = json!(preferences.env);
                }
            }
        }
        agents
    }

    pub fn preferences(&self, agent_type: &str) -> EnginePreferences {
        self.repo.engine_preferences(agent_type).unwrap_or_default()
    }

    pub fn save_preferences(
        &self,
        agent_type: &str,
        enabled: bool,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.repo.save_engine_preferences(agent_type, enabled, env)
    }
}
