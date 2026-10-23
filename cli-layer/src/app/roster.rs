//! Agent roster use cases.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::domain::agent::Agent;
use crate::infra::model_layer::ModelLayerClient;
use crate::infra::store::Store;
use crate::runtime::engine::registry::{Engines, OverlayKind};

#[derive(Clone)]
pub struct RosterService {
    store: Store,
    model: Arc<ModelLayerClient>,
    engines: Engines,
}

impl RosterService {
    pub fn new(store: Store, model: Arc<ModelLayerClient>, engines: Engines) -> Self {
        Self {
            store,
            model,
            engines,
        }
    }

    pub fn list(&self) -> Result<Vec<Agent>> {
        self.store
            .list_roles()?
            .into_iter()
            .map(Agent::from_value)
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<Option<Agent>> {
        self.store.role(id)?.map(Agent::from_value).transpose()
    }

    pub fn save(&self, agent: &Agent) -> Result<Agent> {
        let value = self.store.save_role(&agent.to_value()?)?;
        Agent::from_value(value)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.store.delete_role(id)
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
