//! Model-layer proxy use cases.

use std::sync::Arc;

use serde_json::Value;

use crate::domain::ports::ModelCatalog;

#[derive(Clone)]
pub struct ModelService {
    model: Arc<dyn ModelCatalog>,
}

impl ModelService {
    pub fn new(model: Arc<dyn ModelCatalog>) -> Self {
        Self { model }
    }

    pub fn health(&self) -> Value {
        self.model.health()
    }

    pub async fn rpc(&self, method: &str, args: Value) -> Result<Value, String> {
        self.model.rpc(method, args).await
    }

    pub async fn provider_save(&self, body: &Value) -> Result<Value, String> {
        self.model.provider_save(body).await
    }

    pub async fn provider_delete(&self, body: &Value) -> Result<Value, String> {
        self.model.provider_delete(body).await
    }

    pub async fn provider_probe(&self, body: &Value) -> Result<Value, String> {
        self.model.provider_probe(body).await
    }

    pub async fn disabled_models(&self) -> Result<Value, String> {
        self.model.disabled_models().await
    }
}
