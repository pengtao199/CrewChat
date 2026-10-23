//! Agent roster use cases.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::domain::agent::Agent;
use crate::domain::ports::{AgentRepository, ModelCatalog};
use crate::runtime::engine::registry::{Engines, OverlayKind};

#[derive(Clone)]
pub struct RosterService {
    repo: Arc<dyn AgentRepository>,
    model: Arc<dyn ModelCatalog>,
    engines: Engines,
}

impl RosterService {
    pub fn new(
        repo: Arc<dyn AgentRepository>,
        model: Arc<dyn ModelCatalog>,
        engines: Engines,
    ) -> Self {
        Self {
            repo,
            model,
            engines,
        }
    }

    pub fn list(&self) -> Result<Vec<Agent>> {
        self.repo.list()
    }

    pub fn get(&self, id: &str) -> Result<Option<Agent>> {
        self.repo.get(id)
    }

    pub fn save(&self, agent: &Agent) -> Result<Agent> {
        self.repo.save(agent)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.repo.delete(id)
    }

    pub async fn model_face(&self, agent_type: &str) -> Result<Value> {
        if let Some(spec) = self.engines.find(agent_type) {
            if spec.overlay == OverlayKind::None {
                return Ok(json!({
                    "supported": false,
                    "reason": format!(
                        "{} does not expose a per-session API endpoint for model injection",
                        spec.name
                    ),
                }));
            }
        }
        self.model
            .model_face(agent_type)
            .await
            .map_err(|error| anyhow!(error))
    }
}
