//! ACP session use cases: role normalization, engine preferences and turns.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::app::engines::EngineService;
use crate::infra::model_layer::ModelLayerClient;
use crate::infra::store::Store;
use crate::runtime::acp::host::AcpHost;

#[derive(Clone)]
pub struct SessionService {
    store: Store,
    host: AcpHost,
    model: std::sync::Arc<ModelLayerClient>,
    engines: EngineService,
}

#[derive(Debug)]
pub enum SessionError {
    BadRequest(String),
    BadGateway(String),
    NotFound(String),
}

impl SessionService {
    pub fn new(
        store: Store,
        host: AcpHost,
        model: std::sync::Arc<ModelLayerClient>,
        engines: EngineService,
    ) -> Self {
        Self {
            store,
            host,
            model,
            engines,
        }
    }

    pub fn subscribe(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::runtime::acp::events::EventEnvelope> {
        self.host.subscribe()
    }

    pub async fn connect(
        &self,
        agent_type: &str,
        cwd: PathBuf,
        role_id: Option<&str>,
        conversation_id: Option<String>,
    ) -> std::result::Result<Value, SessionError> {
        let preferences = self.engines.preferences(agent_type);
        if !preferences.enabled {
            return Err(SessionError::BadRequest(
                "this engine is disabled".to_string(),
            ));
        }

        let mut role = role_id.and_then(|id| self.store.role(id).ok().flatten());
        if let Some(role) = role.as_mut() {
            if let Some(provider) = role
                .get("model_provider")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                let model = role
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(display) = self.model.normalize_provider(&provider, &model).await {
                    role["model_provider"] = json!(display);
                }
            }
        }

        let api_key = self.model.local_gateway_key().await;
        self.host
            .connect(
                agent_type,
                cwd,
                role,
                api_key,
                conversation_id,
                preferences.env,
            )
            .await
            .map_err(|error| SessionError::BadGateway(error.to_string()))
    }

    pub fn prompt(
        &self,
        connection_id: &str,
        text: String,
        conversation_id: Option<&str>,
    ) -> std::result::Result<(), SessionError> {
        if text.trim().is_empty() {
            return Err(SessionError::BadRequest("prompt has no text".to_string()));
        }
        if let Some(conversation_id) = conversation_id {
            let _ = self.store.append_turn(
                conversation_id,
                "user",
                &json!([{"type": "text", "text": text}]),
            );
        }
        self.host
            .prompt(connection_id, text)
            .map_err(|error| SessionError::BadRequest(error.to_string()))
    }

    pub fn events_since(&self, connection_id: &str, since_seq: u64) -> Result<Vec<Value>> {
        self.store.events_since(connection_id, since_seq)
    }

    pub fn cancel(&self, connection_id: &str) -> std::result::Result<(), SessionError> {
        self.host
            .cancel(connection_id)
            .map_err(|error| SessionError::BadRequest(error.to_string()))
    }

    pub fn disconnect(&self, connection_id: &str) {
        self.host.close(connection_id);
    }

    pub fn respond_permission(
        &self,
        request_id: &str,
        option_id: Option<String>,
    ) -> std::result::Result<bool, SessionError> {
        if self.host.respond_permission(request_id, option_id) {
            Ok(true)
        } else {
            Err(SessionError::NotFound(
                "unknown permission request".to_string(),
            ))
        }
    }

    pub fn update_preferences(
        &self,
        agent_type: &str,
        enabled: bool,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.engines.save_preferences(agent_type, enabled, env)
    }
}
