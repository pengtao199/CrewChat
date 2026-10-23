//! ACP session use cases: role normalization, engine preferences and turns.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::app::engines::EngineService;
use crate::domain::agent::Agent;
use crate::domain::ports::{
    AgentRepository, ConnectRequest, ConversationRepository, EventSubscription, LeaderControl,
    ModelCatalog, SessionRuntime,
};

#[derive(Clone)]
pub struct SessionService {
    agents: Arc<dyn AgentRepository>,
    conversations: Arc<dyn ConversationRepository>,
    runtime: Arc<dyn SessionRuntime>,
    model: Arc<dyn ModelCatalog>,
    engines: EngineService,
}

#[derive(Debug)]
pub enum SessionError {
    BadRequest(String),
    BadGateway(String),
    NotFound(String),
}

impl std::error::Error for SessionError {}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRequest(message) | Self::BadGateway(message) | Self::NotFound(message) => {
                f.write_str(message)
            }
        }
    }
}

impl SessionService {
    pub fn new(
        agents: Arc<dyn AgentRepository>,
        conversations: Arc<dyn ConversationRepository>,
        runtime: Arc<dyn SessionRuntime>,
        model: Arc<dyn ModelCatalog>,
        engines: EngineService,
    ) -> Self {
        Self {
            agents,
            conversations,
            runtime,
            model,
            engines,
        }
    }

    pub fn subscribe(&self) -> Box<dyn EventSubscription> {
        self.runtime.subscribe()
    }

    fn leader_control(
        &self,
        conversation_id: Option<&str>,
        role: Option<&Value>,
    ) -> Option<LeaderControl> {
        let conversation_id = conversation_id?;
        let role_id = role?.get("id").and_then(Value::as_str)?;
        let detail = self
            .conversations
            .get_conversation(conversation_id)
            .ok()??;
        let leader_role_id = detail
            .pointer("/summary/team/leader_role_id")
            .and_then(Value::as_str)?;
        (leader_role_id == role_id).then(|| LeaderControl {
            conversation_id: conversation_id.to_string(),
            leader_role_id: leader_role_id.to_string(),
        })
    }

    pub async fn connect(
        &self,
        agent_type: &str,
        cwd: PathBuf,
        role_id: Option<&str>,
        conversation_id: Option<String>,
    ) -> std::result::Result<Value, SessionError> {
        let role = self.load_role(role_id)?;
        self.connect_with_role(agent_type, cwd, role, conversation_id)
            .await
    }

    async fn connect_with_role(
        &self,
        agent_type: &str,
        cwd: PathBuf,
        mut role: Option<Value>,
        conversation_id: Option<String>,
    ) -> std::result::Result<Value, SessionError> {
        let preferences = self.engines.preferences(agent_type);
        if !preferences.enabled {
            return Err(SessionError::BadRequest(
                "this engine is disabled".to_string(),
            ));
        }

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
        let leader_control = self.leader_control(conversation_id.as_deref(), role.as_ref());
        self.runtime
            .connect(ConnectRequest {
                agent_type: agent_type.to_string(),
                cwd,
                role,
                api_key,
                conversation_id,
                engine_env: preferences.env,
                leader_control,
            })
            .await
            .map_err(|error| SessionError::BadGateway(error.to_string()))
    }

    fn load_role(&self, role_id: Option<&str>) -> std::result::Result<Option<Value>, SessionError> {
        role_id
            .and_then(|id| self.agents.get(id).ok().flatten())
            .map(|agent| agent.to_value())
            .transpose()
            .map_err(|error| SessionError::BadRequest(error.to_string()))
    }

    /// Run one prompt against an already-resolved Agent snapshot.
    ///
    /// This is shared by Test Run and team delegation. `conversation_id` is
    /// optional: Test Run passes `None` so no history is written; delegated
    /// member turns pass the team conversation and a speaker id.
    pub async fn run_agent(
        &self,
        agent: &Agent,
        prompt: &str,
        conversation_id: Option<&str>,
        speaker_role_id: Option<&str>,
        timeout_seconds: u64,
    ) -> std::result::Result<Value, SessionError> {
        let started = Instant::now();
        let model = model_label(agent);
        let mut events = self.runtime.subscribe();

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let value = match self
            .connect_with_role(
                &agent.agent_type,
                cwd,
                Some(
                    agent
                        .to_value()
                        .map_err(|error| SessionError::BadRequest(error.to_string()))?,
                ),
                conversation_id.map(str::to_string),
            )
            .await
        {
            Ok(value) => value,
            Err(error) => {
                return Ok(test_result(
                    false,
                    "connect",
                    error.to_string(),
                    model,
                    started.elapsed(),
                    None,
                    None,
                ))
            }
        };
        let connection_id = value
            .get("connectionId")
            .or_else(|| value.get("connection_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if let Err(error) = self.runtime.prompt(&connection_id, prompt.to_string()) {
            self.runtime.close(&connection_id);
            return Ok(test_result(
                false,
                "prompt",
                error.to_string(),
                model,
                started.elapsed(),
                None,
                Some(connection_id),
            ));
        }

        let outcome = timeout(Duration::from_secs(timeout_seconds), async {
            let mut text = String::new();
            let mut usage = Value::Null;
            let mut events_seen = 0u64;
            loop {
                let Some(event) = events.recv().await else {
                    return (
                        false,
                        "protocol",
                        "event stream closed".to_string(),
                        text,
                        usage,
                        events_seen,
                    );
                };
                if event.connection_id != connection_id {
                    continue;
                }
                events_seen += 1;
                match event.kind.as_str() {
                    "content_delta" => {
                        if let Some(delta) = event.payload.get("text").and_then(Value::as_str) {
                            text.push_str(delta);
                        }
                    }
                    "usage" => usage = event.payload.get("usage").cloned().unwrap_or(Value::Null),
                    "permission_request" => {
                        return (
                            false,
                            "permission",
                            "the prompt raised a permission request".to_string(),
                            text,
                            usage,
                            events_seen,
                        )
                    }
                    "turn_complete" => {
                        return (true, "complete", String::new(), text, usage, events_seen)
                    }
                    _ => {}
                }
            }
        })
        .await;

        self.runtime.close(&connection_id);

        let (ok, phase, error, text, usage, events_seen) = match outcome {
            Ok(outcome) => outcome,
            Err(_) => (
                false,
                "timeout",
                format!("the prompt did not finish within {timeout_seconds} seconds"),
                String::new(),
                Value::Null,
                0,
            ),
        };
        if ok {
            if let Some(conversation_id) = conversation_id {
                let _ = self.conversations.append_turn_with_speaker(
                    conversation_id,
                    "assistant",
                    speaker_role_id,
                    &json!([{"type": "text", "text": text}]),
                );
            }
        }
        let mut value = test_result(
            ok,
            phase,
            error,
            model,
            started.elapsed(),
            Some(usage),
            Some(connection_id),
        );
        value["text"] = json!(text);
        value["events"] = json!(events_seen);
        Ok(value)
    }

    /// Start a throwaway ACP session with the Agent's full configuration.
    /// It never writes a conversation or delegation.
    pub async fn test_run(&self, role_id: &str) -> std::result::Result<Value, SessionError> {
        let agent = self
            .agents
            .get(role_id)
            .map_err(|error| SessionError::BadRequest(error.to_string()))?
            .ok_or_else(|| SessionError::NotFound("role not found".to_string()))?;
        self.run_agent(&agent, "Reply with OK.", None, None, 90)
            .await
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
            let _ = self.conversations.append_turn(
                conversation_id,
                "user",
                &json!([{"type": "text", "text": text}]),
            );
        }
        self.runtime
            .prompt(connection_id, text)
            .map_err(|error| SessionError::BadRequest(error.to_string()))
    }

    pub fn events_since(&self, connection_id: &str, since_seq: u64) -> Result<Vec<Value>> {
        self.conversations.events_since(connection_id, since_seq)
    }

    pub fn cancel(&self, connection_id: &str) -> std::result::Result<(), SessionError> {
        self.runtime
            .cancel(connection_id)
            .map_err(|error| SessionError::BadRequest(error.to_string()))
    }

    pub fn disconnect(&self, connection_id: &str) {
        self.runtime.close(connection_id);
    }

    pub fn respond_permission(
        &self,
        request_id: &str,
        option_id: Option<String>,
    ) -> std::result::Result<bool, SessionError> {
        if self.runtime.respond_permission(request_id, option_id) {
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

fn model_label(agent: &Agent) -> Option<String> {
    if agent.model.trim().is_empty() {
        return None;
    }
    if agent.model_provider.trim().is_empty() {
        return Some(agent.model.clone());
    }
    Some(format!("{} / {}", agent.model_provider, agent.model))
}

fn test_result(
    ok: bool,
    phase: &str,
    error: String,
    model: Option<String>,
    elapsed: Duration,
    usage: Option<Value>,
    connection_id: Option<String>,
) -> Value {
    json!({
        "ok": ok,
        "phase": phase,
        "error": if ok { Value::Null } else { json!(error) },
        "model": model,
        "durationMs": elapsed.as_millis() as u64,
        "usage": usage.unwrap_or(Value::Null),
        "connectionId": connection_id,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::{json, Value};

    use super::SessionService;
    use crate::app::engines::EngineService;
    use crate::config::Config;
    use crate::domain::agent::Agent;
    use crate::domain::engine::EnginePreferences;
    use crate::domain::event::EventEnvelope;
    use crate::domain::ports::{
        AgentRepository, ConnectRequest, ConversationRepository, EngineSettingsRepository,
        EventSubscription, ModelCatalog, SessionRuntime,
    };
    use crate::runtime::engine::registry::Engines;

    #[derive(Default)]
    struct FakeAgents {
        agents: Vec<Agent>,
    }

    impl AgentRepository for FakeAgents {
        fn list(&self) -> anyhow::Result<Vec<Agent>> {
            Ok(self.agents.clone())
        }

        fn get(&self, id: &str) -> anyhow::Result<Option<Agent>> {
            Ok(self.agents.iter().find(|agent| agent.id == id).cloned())
        }

        fn save(&self, agent: &Agent) -> anyhow::Result<Agent> {
            Ok(agent.clone())
        }

        fn delete(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeConversations {
        turns: Mutex<Vec<(String, String)>>,
        team: Option<Value>,
    }

    impl ConversationRepository for FakeConversations {
        fn list_folders(&self) -> anyhow::Result<Vec<Value>> {
            Ok(Vec::new())
        }

        fn open_folder(&self, _path: &str) -> anyhow::Result<Value> {
            Ok(json!({}))
        }

        fn remove_folder(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn list_conversations(&self, _folder_ids: &[String]) -> anyhow::Result<Vec<Value>> {
            Ok(Vec::new())
        }

        fn create_conversation(
            &self,
            _folder_id: Option<&str>,
            _agent_type: &str,
            _title: Option<&str>,
        ) -> anyhow::Result<String> {
            Ok("conversation".to_string())
        }

        fn get_conversation(&self, _id: &str) -> anyhow::Result<Option<Value>> {
            Ok(self.team.clone())
        }

        fn delete_conversation(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn append_turn(
            &self,
            conversation_id: &str,
            role: &str,
            _blocks: &Value,
        ) -> anyhow::Result<()> {
            self.turns
                .lock()
                .unwrap()
                .push((conversation_id.to_string(), role.to_string()));
            Ok(())
        }

        fn append_turn_with_speaker(
            &self,
            conversation_id: &str,
            role: &str,
            _speaker_role_id: Option<&str>,
            _blocks: &Value,
        ) -> anyhow::Result<()> {
            self.turns
                .lock()
                .unwrap()
                .push((conversation_id.to_string(), role.to_string()));
            Ok(())
        }

        fn events_since(
            &self,
            _connection_id: &str,
            _since_seq: u64,
        ) -> anyhow::Result<Vec<Value>> {
            Ok(Vec::new())
        }
    }

    #[derive(Default)]
    struct FakeRuntime {
        request: Mutex<Option<ConnectRequest>>,
        prompts: Mutex<Vec<String>>,
        events: Mutex<Vec<EventEnvelope>>,
    }

    struct VecSubscription {
        events: std::collections::VecDeque<EventEnvelope>,
    }

    #[async_trait]
    impl EventSubscription for VecSubscription {
        async fn recv(&mut self) -> Option<EventEnvelope> {
            self.events.pop_front()
        }
    }

    #[async_trait]
    impl SessionRuntime for FakeRuntime {
        async fn connect(&self, request: ConnectRequest) -> anyhow::Result<Value> {
            *self.request.lock().unwrap() = Some(request);
            Ok(json!({"connectionId": "fake"}))
        }

        fn prompt(&self, _connection_id: &str, text: String) -> anyhow::Result<()> {
            self.prompts.lock().unwrap().push(text);
            Ok(())
        }

        fn cancel(&self, _connection_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn close(&self, _connection_id: &str) {}

        fn respond_permission(&self, _request_id: &str, _option_id: Option<String>) -> bool {
            true
        }

        fn subscribe(&self) -> Box<dyn EventSubscription> {
            Box::new(VecSubscription {
                events: self.events.lock().unwrap().drain(..).collect(),
            })
        }
    }

    struct FakeModel;

    #[async_trait]
    impl ModelCatalog for FakeModel {
        fn health(&self) -> Value {
            json!({})
        }

        async fn rpc(&self, _method: &str, _args: Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn provider_save(&self, _body: &Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn provider_delete(&self, _body: &Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn provider_probe(&self, _body: &Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn disabled_models(&self) -> Result<Value, String> {
            Ok(json!([]))
        }

        async fn model_face(&self, _agent_type: &str) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn normalize_provider(&self, provider: &str, model: &str) -> Option<String> {
            Some(format!("{provider}/{model}"))
        }

        async fn local_gateway_key(&self) -> Option<String> {
            Some("test-key".to_string())
        }
    }

    struct FakeSettings;

    impl EngineSettingsRepository for FakeSettings {
        fn engine_preferences(&self, _agent_type: &str) -> anyhow::Result<EnginePreferences> {
            Ok(EnginePreferences::default())
        }

        fn save_engine_preferences(
            &self,
            _agent_type: &str,
            _enabled: bool,
            _env: &BTreeMap<String, String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            data_dir: PathBuf::from("/tmp"),
            host: "127.0.0.1".to_string(),
            port: 0,
            token: "test".to_string(),
            model_layer_url: "http://127.0.0.1:3458".to_string(),
            model_layer_gateway: "http://127.0.0.1:3456".to_string(),
            model_layer_token: None,
            claude_command: "claude".to_string(),
            codex_command: "codex".to_string(),
            opencode_command: "opencode".to_string(),
        })
    }

    fn test_engines() -> Engines {
        Engines::new(test_config())
    }

    fn service(
        runtime: Arc<dyn SessionRuntime>,
        conversations: Arc<dyn ConversationRepository>,
    ) -> SessionService {
        let agents = Arc::new(FakeAgents {
            agents: vec![Agent {
                id: "role-1".to_string(),
                name: "Reviewer".to_string(),
                agent_type: "claude_code".to_string(),
                model: "gpt-5".to_string(),
                model_provider: "openai".to_string(),
                ..Agent::default()
            }],
        });
        let engines = EngineService::new(Arc::new(FakeSettings), test_engines(), test_config());
        SessionService::new(agents, conversations, runtime, Arc::new(FakeModel), engines)
    }

    #[tokio::test]
    async fn connect_normalizes_role_and_uses_runtime_port() {
        let runtime = Arc::new(FakeRuntime::default());
        let service = service(runtime.clone(), Arc::new(FakeConversations::default()));

        let value = service
            .connect(
                "claude_code",
                PathBuf::from("/tmp"),
                Some("role-1"),
                Some("conversation-1".to_string()),
            )
            .await
            .unwrap();

        assert_eq!(value["connectionId"], "fake");
        let request = runtime.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.api_key.as_deref(), Some("test-key"));
        assert_eq!(
            request.role.as_ref().unwrap()["model_provider"],
            "openai/gpt-5"
        );
    }

    #[tokio::test]
    async fn prompt_appends_user_turn_before_runtime_prompt() {
        let runtime = Arc::new(FakeRuntime::default());
        let conversations = Arc::new(FakeConversations::default());
        let service = service(runtime.clone(), conversations.clone());

        service
            .prompt("connection-1", "hello".to_string(), Some("conversation-1"))
            .unwrap();

        assert_eq!(
            conversations.turns.lock().unwrap().as_slice(),
            &[("conversation-1".to_string(), "user".to_string())]
        );
        assert_eq!(runtime.prompts.lock().unwrap().as_slice(), ["hello"]);
    }
    #[tokio::test]
    async fn team_leader_gets_control_channel_but_other_roles_do_not() {
        let runtime = Arc::new(FakeRuntime::default());
        let conversations = Arc::new(FakeConversations {
            team: Some(json!({
                "summary": {"team": {"leader_role_id": "role-1"}},
                "turns": [],
            })),
            ..FakeConversations::default()
        });
        let service = service(runtime.clone(), conversations);

        service
            .connect(
                "claude_code",
                PathBuf::from("/tmp"),
                Some("role-1"),
                Some("team-conversation".to_string()),
            )
            .await
            .unwrap();

        let request = runtime.request.lock().unwrap().clone().unwrap();
        let control = request.leader_control.expect("leader control");
        assert_eq!(control.conversation_id, "team-conversation");
        assert_eq!(control.leader_role_id, "role-1");
    }

    #[tokio::test]
    async fn test_run_uses_ephemeral_session_and_does_not_write_history() {
        let runtime = Arc::new(FakeRuntime {
            events: Mutex::new(vec![
                EventEnvelope {
                    kind: "content_delta".to_string(),
                    connection_id: "fake".to_string(),
                    seq: 1,
                    payload: json!({"text": "OK"}),
                },
                EventEnvelope {
                    kind: "turn_complete".to_string(),
                    connection_id: "fake".to_string(),
                    seq: 2,
                    payload: json!({"stop_reason": "end_turn"}),
                },
            ]),
            ..FakeRuntime::default()
        });
        let conversations = Arc::new(FakeConversations::default());
        let service = service(runtime, conversations.clone());

        let value = service.test_run("role-1").await.unwrap();

        assert_eq!(value["ok"], true);
        assert_eq!(value["text"], "OK");
        assert!(conversations.turns.lock().unwrap().is_empty());
    }
}
