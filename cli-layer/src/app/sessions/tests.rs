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
    EventSubscription, ModelCatalog, SessionRuntime, TeamRepository,
};
use crate::domain::team::TeamSnapshot;
use crate::infra::store::Store;
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

    fn set_external_id(&self, _conversation_id: &str, _external_id: &str) -> anyhow::Result<()> {
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

    fn events_since(&self, _connection_id: &str, _since_seq: u64) -> anyhow::Result<Vec<Value>> {
        Ok(Vec::new())
    }

    fn events_for_conversation(
        &self,
        _conversation_id: &str,
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
    SessionService::new(
        agents,
        conversations,
        runtime,
        Arc::new(FakeModel),
        engines,
        None,
    )
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
            Some("resume-1"),
        )
        .await
        .unwrap();

    assert_eq!(value["connectionId"], "fake");
    let request = runtime.request.lock().unwrap().clone().unwrap();
    assert_eq!(request.resume_session_id.as_deref(), Some("resume-1"));
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
            None,
        )
        .await
        .unwrap();

    let request = runtime.request.lock().unwrap().clone().unwrap();
    let control = request.leader_control.expect("leader control");
    assert_eq!(control.conversation_id, "team-conversation");
    assert_eq!(control.leader_role_id, "role-1");
}

#[tokio::test]
async fn leader_connection_is_recorded_on_the_team_member_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("leader.db")).unwrap();
    store.init_schema().unwrap();
    let leader = Agent {
        id: "leader".to_string(),
        name: "Leader".to_string(),
        agent_type: "claude_code".to_string(),
        ..Agent::default()
    };
    let member = Agent {
        id: "member".to_string(),
        name: "Member".to_string(),
        agent_type: "claude_code".to_string(),
        ..Agent::default()
    };
    let snapshot = TeamSnapshot::new(&leader, std::slice::from_ref(&member), crate::config::now());
    let conversation = store
        .create_team_conversation(None, "claude_code", Some("Team"), &snapshot)
        .unwrap();
    let engines = EngineService::new(Arc::new(FakeSettings), test_engines(), test_config());
    let conversation_repo: Arc<dyn ConversationRepository> = Arc::new(store.clone());
    let team_repo: Arc<dyn TeamRepository> = Arc::new(store.clone());
    let service = SessionService::new(
        Arc::new(FakeAgents {
            agents: vec![leader],
        }),
        conversation_repo,
        Arc::new(FakeRuntime::default()),
        Arc::new(FakeModel),
        engines,
        Some(team_repo),
    );

    service
        .connect(
            "claude_code",
            PathBuf::from("/tmp"),
            Some("leader"),
            Some(conversation.clone()),
            None,
        )
        .await
        .unwrap();

    let agents = store.conversation_agents(&conversation).unwrap();
    let leader_row = agents
        .iter()
        .find(|row| row["role_id"] == "leader")
        .unwrap();
    assert_eq!(leader_row["state"], "idle");
    assert_eq!(leader_row["connection_id"], "fake");
}

#[tokio::test]
async fn prompt_persists_the_assistant_turn_after_turn_complete() {
    let runtime = Arc::new(FakeRuntime {
        events: Mutex::new(vec![
            EventEnvelope {
                kind: "content_delta".to_string(),
                connection_id: "connection-1".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 1,
                payload: json!({"text": "answer"}),
            },
            EventEnvelope {
                kind: "turn_complete".to_string(),
                connection_id: "connection-1".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 2,
                payload: json!({"stop_reason": "end_turn"}),
            },
        ]),
        ..FakeRuntime::default()
    });
    let conversations = Arc::new(FakeConversations::default());
    let service = service(runtime, conversations.clone());

    service
        .prompt(
            "connection-1",
            "question".to_string(),
            Some("conversation-1"),
        )
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    assert_eq!(
        conversations.turns.lock().unwrap().as_slice(),
        &[
            ("conversation-1".to_string(), "user".to_string()),
            ("conversation-1".to_string(), "assistant".to_string()),
        ]
    );
}

#[tokio::test]
async fn test_run_uses_ephemeral_session_and_does_not_write_history() {
    let runtime = Arc::new(FakeRuntime {
        events: Mutex::new(vec![
            EventEnvelope {
                kind: "content_delta".to_string(),
                connection_id: "fake".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 1,
                payload: json!({"text": "OK"}),
            },
            EventEnvelope {
                kind: "turn_complete".to_string(),
                connection_id: "fake".to_string(),
                conversation_id: None,
                speaker_role_id: None,
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

#[tokio::test]
async fn delegated_turn_waits_through_permission_requests() {
    let events = vec![
        EventEnvelope {
            kind: "permission_request".to_string(),
            connection_id: "member".to_string(),
            conversation_id: Some("conv".to_string()),
            speaker_role_id: Some("member".to_string()),
            seq: 1,
            payload: json!({"request_id": "permission"}),
        },
        EventEnvelope {
            kind: "turn_complete".to_string(),
            connection_id: "member".to_string(),
            conversation_id: Some("conv".to_string()),
            speaker_role_id: Some("member".to_string()),
            seq: 2,
            payload: json!({"stop_reason": "end_turn"}),
        },
    ];
    let outcome = super::turn::wait_for_turn(
        "member",
        Box::new(VecSubscription {
            events: events.into_iter().collect(),
        }),
        5,
        false,
    )
    .await;
    assert!(outcome.0, "{outcome:?}");
}

#[tokio::test]
async fn test_run_still_fails_fast_on_permission_requests() {
    let events = vec![EventEnvelope {
        kind: "permission_request".to_string(),
        connection_id: "member".to_string(),
        conversation_id: None,
        speaker_role_id: None,
        seq: 1,
        payload: json!({"request_id": "permission"}),
    }];
    let outcome = super::turn::wait_for_turn(
        "member",
        Box::new(VecSubscription {
            events: events.into_iter().collect(),
        }),
        5,
        true,
    )
    .await;
    assert!(!outcome.0);
    assert_eq!(outcome.1, "permission");
}
