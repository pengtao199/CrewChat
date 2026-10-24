use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::TeamService;
use crate::app::engines::EngineService;
use crate::app::readiness::ReadinessService;
use crate::app::roster::RosterService;
use crate::app::sessions::SessionService;
use crate::config::Config;
use crate::domain::agent::Agent;
use crate::domain::engine::EnginePreferences;
use crate::domain::event::EventEnvelope;
use crate::domain::ports::{
    AgentRepository, ConnectRequest, ConversationRepository, EngineSettingsRepository,
    EventPublisher, EventSubscription, ModelCatalog, SessionRuntime, SkillCatalog, TeamRepository,
};
use crate::domain::team::{Delegation, TeamSnapshot};
use crate::infra::store::Store;
use crate::runtime::engine::registry::Engines;

#[derive(Default)]
struct FakeRuntime {
    events: Mutex<Vec<EventEnvelope>>,
    connects: Mutex<usize>,
}

struct VecSubscription(std::collections::VecDeque<EventEnvelope>);

#[async_trait]
impl EventSubscription for VecSubscription {
    async fn recv(&mut self) -> Option<EventEnvelope> {
        self.0.pop_front()
    }
}

#[derive(Default)]
struct FakeEvents {
    events: Mutex<Vec<EventEnvelope>>,
}

impl EventPublisher for FakeEvents {
    fn emit(&self, event: EventEnvelope) {
        self.events.lock().unwrap().push(event);
    }
}

#[async_trait]
impl SessionRuntime for FakeRuntime {
    async fn connect(&self, _request: ConnectRequest) -> anyhow::Result<Value> {
        *self.connects.lock().unwrap() += 1;
        Ok(json!({
            "connectionId": "test-connection",
            "session_id": "test-session",
        }))
    }

    fn prompt(&self, _connection_id: &str, _text: String) -> anyhow::Result<()> {
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
        Box::new(VecSubscription(
            self.events.lock().unwrap().drain(..).collect(),
        ))
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
        Ok(json!({"supported": true, "models": []}))
    }

    async fn normalize_provider(&self, _provider: &str, _model: &str) -> Option<String> {
        Some("OpenAI".to_string())
    }

    async fn local_gateway_key(&self) -> Option<String> {
        None
    }
}

struct FakeSkills;

#[async_trait]
impl SkillCatalog for FakeSkills {
    async fn list(&self) -> Result<Value, String> {
        Ok(json!({"skills": []}))
    }

    async fn detail(&self, _id: &str) -> Result<Value, String> {
        Ok(json!({}))
    }

    async fn install(&self, _body: &Value) -> Result<Value, String> {
        Ok(json!({}))
    }

    async fn check(&self, _id: &str) -> Result<Value, String> {
        Ok(json!({}))
    }

    async fn update(&self, _id: &str) -> Result<Value, String> {
        Ok(json!({}))
    }

    async fn remove(&self, _id: &str) -> Result<Value, String> {
        Ok(json!({}))
    }

    async fn collections(&self) -> Result<Value, String> {
        Ok(json!({"collections": []}))
    }

    async fn global_links(&self) -> Result<Value, String> {
        Ok(json!({"links": []}))
    }

    async fn materialize(
        &self,
        _home: &Path,
        _agent_type: &str,
        _skill_ids: &[String],
    ) -> Result<Value, String> {
        Ok(json!({}))
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

fn config() -> Arc<Config> {
    Arc::new(Config {
        data_dir: PathBuf::from("/tmp"),
        host: "127.0.0.1".to_string(),
        port: 0,
        token: "test".to_string(),
        model_layer_url: "http://127.0.0.1:3458".to_string(),
        model_layer_gateway: "http://127.0.0.1:3456".to_string(),
        model_layer_token: None,
        claude_command: "python3".to_string(),
        codex_command: "python3".to_string(),
        opencode_command: "python3".to_string(),
    })
}

#[tokio::test]
async fn delegation_uses_snapshot_and_records_speaker() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("team.db")).unwrap();
    store.init_schema().unwrap();

    let leader = Agent {
        id: "leader".to_string(),
        name: "Manager".to_string(),
        agent_type: "claude_code".to_string(),
        model: "gpt-5".to_string(),
        model_provider: "openai".to_string(),
        ..Agent::default()
    };
    let member = Agent {
        id: "member".to_string(),
        name: "Builder".to_string(),
        agent_type: "claude_code".to_string(),
        model: "gpt-5".to_string(),
        model_provider: "openai".to_string(),
        ..Agent::default()
    };
    let leader = Agent::from_value(store.save_role(&leader.to_value().unwrap()).unwrap()).unwrap();
    let member = Agent::from_value(store.save_role(&member.to_value().unwrap()).unwrap()).unwrap();
    let snapshot = TeamSnapshot::new(&leader, std::slice::from_ref(&member), crate::config::now());
    let conversation = store
        .create_team_conversation(None, "claude_code", Some("Team"), &snapshot)
        .unwrap();

    let model = Arc::new(FakeModel);
    let engines = Engines::new(config());
    let agent_repo: Arc<dyn AgentRepository> = Arc::new(store.clone());
    let conversations: Arc<dyn ConversationRepository> = Arc::new(store.clone());
    let teams: Arc<dyn TeamRepository> = Arc::new(store.clone());
    let roster = RosterService::new(agent_repo.clone(), model.clone(), engines.clone());
    let engine_service = EngineService::new(Arc::new(FakeSettings), engines, config());
    let readiness = ReadinessService::new(
        roster.clone(),
        engine_service.clone(),
        model.clone(),
        Arc::new(FakeSkills),
    );
    let runtime = Arc::new(FakeRuntime {
        connects: Mutex::new(0),
        events: Mutex::new(vec![
            EventEnvelope {
                kind: "content_delta".to_string(),
                connection_id: "test-connection".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 1,
                payload: json!({"text": "built"}),
            },
            EventEnvelope {
                kind: "thinking".to_string(),
                connection_id: "test-connection".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 2,
                payload: json!({"text": "considering"}),
            },
            EventEnvelope {
                kind: "tool_call".to_string(),
                connection_id: "test-connection".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 3,
                payload: json!({
                    "tool_call_id": "tool-1",
                    "tool_name": "Write",
                    "input_preview": "/tmp/result.swift",
                    "status": "running",
                    "started_at": "2026-10-24T00:00:00Z",
                    "raw": {"locations": [{"path": "/tmp/result.swift"}]}
                }),
            },
            EventEnvelope {
                kind: "tool_call_update".to_string(),
                connection_id: "test-connection".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 4,
                payload: json!({
                    "tool_call_id": "tool-1",
                    "tool_name": "Write",
                    "status": "completed",
                    "raw_output": {"output": "written"}
                }),
            },
            EventEnvelope {
                kind: "usage".to_string(),
                connection_id: "test-connection".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 5,
                payload: json!({"usage": {"input": 2, "output": 1}}),
            },
            EventEnvelope {
                kind: "turn_complete".to_string(),
                connection_id: "test-connection".to_string(),
                conversation_id: None,
                speaker_role_id: None,
                seq: 6,
                payload: json!({"stop_reason": "end_turn"}),
            },
        ]),
    });
    let sessions = SessionService::new(
        agent_repo,
        conversations.clone(),
        runtime.clone(),
        model,
        engine_service,
        Some(teams.clone()),
    );
    let events = Arc::new(FakeEvents::default());
    let team = TeamService::new(
        roster,
        conversations,
        teams,
        readiness,
        sessions,
        Some(events.clone()),
    );

    let result = team
        .delegate(
            &conversation,
            "leader",
            "member",
            "build the page",
            Some("keep it small"),
            Some("summary"),
        )
        .await
        .unwrap();

    assert_eq!(result["status"], "done");
    let detail = store.get_conversation(&conversation).unwrap().unwrap();
    assert_eq!(detail["turns"][0]["speaker_role_id"], "member");
    assert_eq!(detail["summary"]["delegations"][0]["status"], "done");
    let member_row = &store.conversation_agents(&conversation).unwrap()[1];
    assert_eq!(member_row["state"], "done");
    assert_eq!(member_row["external_session_id"], "test-session");
    let kinds = events
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.kind.clone())
        .collect::<Vec<_>>();
    assert!(kinds.iter().any(|kind| kind == "delegation_started"));
    assert!(kinds.iter().any(|kind| kind == "delegation_completed"));
    let status = team.status(&conversation).unwrap();
    assert_eq!(status["usage"]["input"], 2);
    assert_eq!(status["members"][1]["usage"]["input"], 2);
    assert_eq!(result["artifacts"][0], "/tmp/result.swift");
    assert!(team
        .retry(result["delegationId"].as_str().unwrap())
        .await
        .is_err());

    // The second delegation reuses the member connection.
    runtime.events.lock().unwrap().extend([
        EventEnvelope {
            kind: "content_delta".to_string(),
            connection_id: "test-connection".to_string(),
            conversation_id: None,
            speaker_role_id: None,
            seq: 3,
            payload: json!({"text": "again"}),
        },
        EventEnvelope {
            kind: "turn_complete".to_string(),
            connection_id: "test-connection".to_string(),
            conversation_id: None,
            speaker_role_id: None,
            seq: 4,
            payload: json!({"stop_reason": "end_turn"}),
        },
    ]);
    let second = team
        .delegate(&conversation, "leader", "member", "second", None, None)
        .await
        .unwrap();
    assert_eq!(second["status"], "done");
    assert_eq!(*runtime.connects.lock().unwrap(), 1);

    // A non-member cannot be delegated to.
    assert!(team
        .delegate(&conversation, "leader", "outsider", "x", None, None)
        .await
        .is_err());
    // A non-leader cannot delegate.
    assert!(team
        .delegate(&conversation, "member", "leader", "x", None, None)
        .await
        .is_err());
}

#[tokio::test]
async fn cancel_conversation_cancels_running_delegations() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("cancel.db")).unwrap();
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
    let running = Delegation::new(
        &conversation,
        "leader",
        "member",
        "still running",
        None,
        None,
        crate::config::now(),
    );
    store.create_delegation(&running).unwrap();
    store
        .update_delegation(&running.id, "running", None, None, None)
        .unwrap();

    let model = Arc::new(FakeModel);
    let engines = Engines::new(config());
    let conversations: Arc<dyn ConversationRepository> = Arc::new(store.clone());
    let teams: Arc<dyn TeamRepository> = Arc::new(store.clone());
    let agent_repo: Arc<dyn AgentRepository> = Arc::new(store.clone());
    let roster = RosterService::new(agent_repo.clone(), model.clone(), engines.clone());
    let engine_service = EngineService::new(Arc::new(FakeSettings), engines, config());
    let readiness = ReadinessService::new(
        roster.clone(),
        engine_service.clone(),
        model.clone(),
        Arc::new(FakeSkills),
    );
    let sessions = SessionService::new(
        agent_repo,
        conversations.clone(),
        Arc::new(FakeRuntime::default()),
        model,
        engine_service,
        Some(teams.clone()),
    );
    let team = TeamService::new(roster, conversations, teams, readiness, sessions, None);

    let result = team.cancel_conversation(&conversation).unwrap();
    assert_eq!(result["cancelledDelegations"], 1);
    assert_eq!(
        store.delegation(&running.id).unwrap().unwrap()["status"],
        "cancelled"
    );
}
