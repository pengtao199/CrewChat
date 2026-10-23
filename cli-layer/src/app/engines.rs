//! Engine preferences and listing use cases.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use crate::config::Config;
use crate::domain::engine::EnginePreferences;
use crate::domain::ports::EngineSettingsRepository;
use crate::runtime::engine::{manager, registry::Engines};

#[derive(Clone)]
pub struct EngineService {
    repo: Arc<dyn EngineSettingsRepository>,
    engines: Engines,
    config: Arc<Config>,
}

impl EngineService {
    pub fn new(
        repo: Arc<dyn EngineSettingsRepository>,
        engines: Engines,
        config: Arc<Config>,
    ) -> Self {
        Self {
            repo,
            engines,
            config,
        }
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

    pub async fn latest_release(&self, agent_type: &str) -> Result<Option<String>> {
        manager::latest_version(&self.config, agent_type).await
    }

    pub async fn prepare_npx(
        &self,
        agent_type: &str,
        version: Option<&str>,
        clean_first: bool,
    ) -> Result<String> {
        manager::prepare_npx(&self.config, agent_type, version, clean_first).await
    }

    pub async fn download_binary(&self, agent_type: &str, version: Option<&str>) -> Result<String> {
        manager::download_binary(&self.config, agent_type, version).await
    }

    pub async fn install_pi(&self) -> Result<String> {
        manager::install_pi_binary().await
    }

    pub async fn diagnostics(&self, agent_type: &str) -> Value {
        manager::diagnostics(&self.config, agent_type).await
    }
}
