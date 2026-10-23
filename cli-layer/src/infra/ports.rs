//! Port implementations for HTTP-backed model / skill clients.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::domain::ports::{McpCatalog, ModelCatalog, SkillCatalog};
use crate::infra::mcp_layer::McpLayerClient;
use crate::infra::model_layer::ModelLayerClient;
use crate::infra::skill_layer::SkillLayerClient;

#[async_trait]
impl ModelCatalog for ModelLayerClient {
    fn health(&self) -> Value {
        self.health()
    }

    async fn rpc(&self, method: &str, args: Value) -> Result<Value, String> {
        self.rpc(method, args).await
    }

    async fn provider_save(&self, body: &Value) -> Result<Value, String> {
        self.provider_save(body).await
    }

    async fn provider_delete(&self, body: &Value) -> Result<Value, String> {
        self.provider_delete(body).await
    }

    async fn provider_probe(&self, body: &Value) -> Result<Value, String> {
        self.provider_probe(body).await
    }

    async fn set_model_enabled(
        &self,
        provider_id: &str,
        model: &str,
        enabled: bool,
    ) -> Result<Value, String> {
        self.set_model_enabled(provider_id, model, enabled).await
    }

    async fn set_credential_enabled(
        &self,
        provider_id: &str,
        credential_id: &str,
        enabled: bool,
    ) -> Result<Value, String> {
        self.set_credential_enabled(provider_id, credential_id, enabled)
            .await
    }

    async fn disabled_models(&self) -> Result<Value, String> {
        self.disabled_models().await
    }

    async fn model_face(&self, agent_type: &str) -> Result<Value, String> {
        self.model_face(agent_type).await
    }

    async fn normalize_provider(&self, provider: &str, model: &str) -> Option<String> {
        self.normalize_provider(provider, model).await
    }

    async fn local_gateway_key(&self) -> Option<String> {
        self.local_gateway_key().await
    }
}

#[async_trait]
impl SkillCatalog for SkillLayerClient {
    async fn list(&self) -> Result<Value, String> {
        self.list().await
    }

    async fn detail(&self, id: &str) -> Result<Value, String> {
        self.detail(id).await
    }

    async fn install(&self, body: &Value) -> Result<Value, String> {
        self.install(body).await
    }

    async fn check(&self, id: &str) -> Result<Value, String> {
        self.check(id).await
    }

    async fn update(&self, id: &str) -> Result<Value, String> {
        self.update(id).await
    }

    async fn remove(&self, id: &str) -> Result<Value, String> {
        self.remove(id).await
    }

    async fn collections(&self) -> Result<Value, String> {
        self.collections().await
    }

    async fn global_links(&self) -> Result<Value, String> {
        self.global_links().await
    }

    async fn materialize(
        &self,
        home: &Path,
        agent_type: &str,
        skill_ids: &[String],
    ) -> Result<Value, String> {
        self.materialize(home, agent_type, skill_ids).await
    }
}

#[async_trait]
impl McpCatalog for McpLayerClient {
    async fn list(&self) -> Result<Value, String> {
        self.list().await
    }

    async fn read(&self, id: &str) -> Result<Value, String> {
        self.read(id).await
    }

    async fn upsert(&self, id: &str, spec: &Value) -> Result<Value, String> {
        self.upsert(id, spec).await
    }

    async fn remove(&self, id: &str) -> Result<Value, String> {
        self.remove(id).await
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Value, String> {
        self.set_enabled(id, enabled).await
    }

    async fn sync_agent(&self, agent_id: &str, server_ids: &[String]) -> Result<Value, String> {
        self.sync_agent(agent_id, server_ids).await
    }

    async fn agent(&self, agent_id: &str) -> Result<Value, String> {
        self.agent(agent_id).await
    }

    async fn gateway(&self) -> Result<Value, String> {
        self.gateway().await
    }
}
