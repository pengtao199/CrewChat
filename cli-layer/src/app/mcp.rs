//! MCP library and Agent MCP-binding use cases.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::app::roster::RosterService;
use crate::domain::ports::McpCatalog;

#[derive(Clone)]
pub struct McpService {
    roster: RosterService,
    mcp: Arc<dyn McpCatalog>,
}

impl McpService {
    pub fn new(roster: RosterService, mcp: Arc<dyn McpCatalog>) -> Self {
        Self { roster, mcp }
    }

    pub async fn list(&self) -> Result<Value> {
        self.mcp.list().await.map_err(anyhow::Error::msg)
    }

    pub async fn read(&self, id: &str) -> Result<Value> {
        self.mcp.read(id).await.map_err(anyhow::Error::msg)
    }

    pub async fn upsert(&self, id: &str, spec: &Value) -> Result<Value> {
        self.mcp.upsert(id, spec).await.map_err(anyhow::Error::msg)
    }

    pub async fn remove(&self, id: &str) -> Result<Value> {
        let value = self.mcp.remove(id).await.map_err(anyhow::Error::msg)?;
        let removed = value
            .get("removed")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if removed {
            for mut role in self.roster.list()? {
                if role.mcp.iter().any(|server| server == id) {
                    role.mcp.retain(|server| server != id);
                    self.roster.save(&role)?;
                }
            }
        }

        Ok(value)
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Value> {
        self.mcp
            .set_enabled(id, enabled)
            .await
            .map_err(anyhow::Error::msg)
    }

    pub async fn gateway(&self) -> Result<Value> {
        self.mcp.gateway().await.map_err(anyhow::Error::msg)
    }

    pub async fn set_agent_mcps(
        &self,
        role_id: &str,
        mut server_ids: Vec<String>,
    ) -> Result<Value> {
        let mut role = self
            .roster
            .get(role_id)?
            .ok_or_else(|| anyhow!("role not found"))?;
        server_ids.sort();
        server_ids.dedup();

        // Start/stop the per-Agent proxy before persisting. A failed sync must
        // not leave the roster claiming a binding that has no gateway.
        self.sync_agent_mcps(role_id, &server_ids).await?;

        role.mcp = server_ids;
        let saved = self.roster.save(&role)?;
        Ok(json!({
            "roleId": saved.id,
            "serverIds": saved.mcp,
        }))
    }

    pub async fn sync_agent_mcps(&self, role_id: &str, server_ids: &[String]) -> Result<()> {
        self.mcp
            .sync_agent(role_id, server_ids)
            .await
            .map(|_| ())
            .map_err(anyhow::Error::msg)
    }

    pub async fn agent_status(&self, role_id: &str) -> Result<Value> {
        let role = self
            .roster
            .get(role_id)?
            .ok_or_else(|| anyhow!("role not found"))?;
        let available = self
            .mcp
            .list()
            .await
            .ok()
            .and_then(|value| value.get("servers").and_then(Value::as_array).cloned())
            .unwrap_or_default();

        let mut rows = Vec::new();
        let mut missing = Vec::new();
        for id in &role.mcp {
            let found = available
                .iter()
                .find(|server| server.get("id").and_then(Value::as_str) == Some(id.as_str()));
            if found.is_none() {
                missing.push(id.clone());
            }
            rows.push(json!({
                "id": id,
                "name": found
                    .and_then(|server| server.get("spec"))
                    .and_then(|spec| spec.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(id),
                "available": found.is_some(),
            }));
        }

        let proxy = self.mcp.agent(role_id).await.ok();
        Ok(json!({
            "servers": rows,
            "missing": missing,
            "proxy": proxy,
        }))
    }
}
