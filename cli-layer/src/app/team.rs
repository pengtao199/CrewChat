//! Team conversations: frozen snapshots, member state and delegation.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::app::readiness::ReadinessService;
use crate::app::roster::RosterService;
use crate::app::sessions::SessionService;
use crate::config::now;
use crate::domain::agent::Agent;
use crate::domain::event::EventEnvelope;
use crate::domain::ports::{ConversationRepository, EventPublisher, TeamRepository};
use crate::domain::team::{aggregate_usage, Delegation, TeamRef, TeamSnapshot};

const MAX_TEAM_MEMBERS: usize = 5;

#[derive(Clone)]
pub struct TeamService {
    roster: RosterService,
    conversations: Arc<dyn ConversationRepository>,
    teams: Arc<dyn TeamRepository>,
    readiness: ReadinessService,
    sessions: SessionService,
    events: Option<Arc<dyn EventPublisher>>,
    active: Arc<Mutex<HashSet<String>>>,
}

impl TeamService {
    pub fn new(
        roster: RosterService,
        conversations: Arc<dyn ConversationRepository>,
        teams: Arc<dyn TeamRepository>,
        readiness: ReadinessService,
        sessions: SessionService,
        events: Option<Arc<dyn EventPublisher>>,
    ) -> Self {
        Self {
            roster,
            conversations,
            teams,
            readiness,
            sessions,
            events,
            active: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn create(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
        team: &TeamRef,
    ) -> Result<String> {
        let (leader, members) = self.load_team(team)?;
        if !agent_type.trim().is_empty() && agent_type != leader.agent_type {
            bail!(
                "leader engine mismatch: request uses '{agent_type}', leader '{}' uses '{}'",
                leader.name,
                leader.agent_type
            );
        }

        for agent in std::iter::once(&leader).chain(members.iter()) {
            let report = self.readiness.check(&agent.id).await?;
            if report.get("ready").and_then(Value::as_bool) != Some(true) {
                let blockers = report
                    .get("blockers")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.get("message").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_else(|| "unknown readiness blocker".to_string());
                bail!("{} is not ready: {blockers}", agent.name);
            }
        }

        let snapshot = TeamSnapshot::new(&leader, &members, now());
        self.teams
            .create_team_conversation(folder_id, &leader.agent_type, title, &snapshot)
    }

    fn load_team(&self, team: &TeamRef) -> Result<(Agent, Vec<Agent>)> {
        if team.leader_role_id.trim().is_empty() {
            bail!("team leader is required");
        }
        if team.member_role_ids.is_empty() {
            bail!("a team needs at least one member");
        }
        if team.member_role_ids.len() + 1 > MAX_TEAM_MEMBERS {
            bail!("a team can have at most {MAX_TEAM_MEMBERS} agents including the leader");
        }
        let mut seen = HashSet::new();
        for id in &team.member_role_ids {
            if id == &team.leader_role_id {
                bail!("the leader cannot also be a member");
            }
            if !seen.insert(id) {
                bail!("duplicate team member '{id}'");
            }
        }
        let leader = self
            .roster
            .get(&team.leader_role_id)?
            .ok_or_else(|| anyhow!("leader role '{}' not found", team.leader_role_id))?;
        let mut members = Vec::with_capacity(team.member_role_ids.len());
        for id in &team.member_role_ids {
            let agent = self
                .roster
                .get(id)?
                .ok_or_else(|| anyhow!("member role '{id}' not found"))?;
            members.push(agent);
        }
        Ok((leader, members))
    }

    pub fn status(&self, conversation_id: &str) -> Result<Value> {
        self.sessions.reap_idle(Duration::from_secs(1800));
        let delegations = self.teams.delegations(conversation_id)?;
        let mut members = self.teams.conversation_agents(conversation_id)?;
        for member in &mut members {
            let Some(role_id) = member.get("role_id").and_then(Value::as_str) else {
                continue;
            };
            let member_delegations = delegations
                .iter()
                .filter(|delegation| delegation["member_role_id"] == role_id)
                .cloned()
                .collect::<Vec<_>>();
            member["usage"] = aggregate_usage(&member_delegations);
        }
        Ok(json!({
            "members": members,
            "delegations": delegations,
            "usage": aggregate_usage(&delegations),
        }))
    }

    fn emit_team_event(
        &self,
        conversation_id: &str,
        kind: &str,
        speaker_role_id: Option<&str>,
        payload: Value,
    ) {
        if let Some(events) = &self.events {
            events.emit(EventEnvelope {
                kind: kind.to_string(),
                connection_id: String::new(),
                conversation_id: Some(conversation_id.to_string()),
                speaker_role_id: speaker_role_id.map(str::to_string),
                seq: 0,
                payload,
            });
        }
    }

    /// Cancel a queued/running delegation. The member connection receives an
    /// ACP cancel; the running delegation then records the cancelled stop
    /// reason. This is idempotent for already-finished delegations.
    pub fn cancel(&self, delegation_id: &str) -> Result<Value> {
        let delegation = self
            .teams
            .delegation(delegation_id)?
            .ok_or_else(|| anyhow!("delegation not found"))?;
        let status = delegation
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(status, "done" | "failed" | "cancelled") {
            return Ok(json!({
                "delegationId": delegation_id,
                "cancelled": status == "cancelled",
                "status": status,
            }));
        }
        let conversation_id = delegation
            .get("conversation_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("delegation has no conversation"))?;
        let member_role_id = delegation
            .get("member_role_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("delegation has no member"))?;
        let connection_cancelled = self.sessions.cancel_role(conversation_id, member_role_id);
        self.teams.update_delegation(
            delegation_id,
            "cancelled",
            Some(&json!({"cancelled": true})),
            None,
            None,
        )?;
        self.teams
            .set_conversation_agent(conversation_id, member_role_id, "idle", None)?;
        Ok(json!({
            "delegationId": delegation_id,
            "cancelled": true,
            "connectionCancelled": connection_cancelled,
        }))
    }

    /// Retry a failed/cancelled delegation with the same frozen member and task.
    pub async fn retry(&self, delegation_id: &str) -> Result<Value> {
        let delegation = self
            .teams
            .delegation(delegation_id)?
            .ok_or_else(|| anyhow!("delegation not found"))?;
        let status = delegation
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(status, "failed" | "cancelled") {
            bail!("only failed or cancelled delegations can be retried");
        }
        let conversation_id = delegation
            .get("conversation_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("delegation has no conversation"))?;
        let leader_role_id = delegation
            .get("leader_role_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("delegation has no leader"))?;
        let member_role_id = delegation
            .get("member_role_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("delegation has no member"))?;
        let task = delegation
            .get("task")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("delegation has no task"))?;
        let context = delegation.get("context").and_then(Value::as_str);
        let expected = delegation.get("expected_output").and_then(Value::as_str);
        let mut result = self
            .delegate(
                conversation_id,
                leader_role_id,
                member_role_id,
                task,
                context,
                expected,
            )
            .await?;
        result["retriedFrom"] = json!(delegation_id);
        self.emit_team_event(
            conversation_id,
            "delegation_retried",
            Some(member_role_id),
            json!({
                "previous_delegation_id": delegation_id,
                "delegation_id": result["delegationId"].clone(),
                "status": result["status"].clone(),
            }),
        );
        Ok(result)
    }

    /// Run one member turn for a Leader delegation.
    ///
    /// The member snapshot comes from the frozen team JSON, never from the
    /// current roster. Only a member of this conversation can be called, and
    /// only with the conversation's leader id.
    pub async fn delegate(
        &self,
        conversation_id: &str,
        leader_role_id: &str,
        member_role_id: &str,
        task: &str,
        context: Option<&str>,
        expected_output: Option<&str>,
    ) -> Result<Value> {
        if task.trim().is_empty() {
            bail!("delegation task is empty");
        }
        let detail = self
            .conversations
            .get_conversation(conversation_id)?
            .ok_or_else(|| anyhow!("conversation not found"))?;
        let snapshot = detail
            .pointer("/summary/team")
            .cloned()
            .ok_or_else(|| anyhow!("conversation is not a team"))?;
        let snapshot: TeamSnapshot = serde_json::from_value(snapshot)
            .map_err(|error| anyhow!("invalid team snapshot: {error}"))?;
        if snapshot.leader_role_id != leader_role_id {
            bail!("only the conversation leader can delegate");
        }
        let member = snapshot
            .members
            .iter()
            .find(|member| member.role_id == member_role_id)
            .ok_or_else(|| anyhow!("'{member_role_id}' is not a member of this team"))?
            .clone();

        let key = format!("{conversation_id}:{member_role_id}");
        let guard = ActiveGuard::enter(self.active.clone(), key)?;

        let delegation = Delegation::new(
            conversation_id,
            leader_role_id,
            member_role_id,
            task,
            context,
            expected_output,
            now(),
        );
        self.teams.create_delegation(&delegation)?;
        self.teams
            .set_conversation_agent(conversation_id, member_role_id, "running", None)?;
        self.emit_team_event(
            conversation_id,
            "delegation_started",
            Some(member_role_id),
            json!({
                "delegation_id": delegation.id,
                "member_role_id": member_role_id,
                "task": task,
            }),
        );

        let prompt = delegation_prompt(&snapshot, &member.agent, task, context, expected_output);
        let cwd = self
            .teams
            .conversation_cwd(conversation_id)?
            .map(std::path::PathBuf::from);
        let outcome = match self
            .sessions
            .run_member(
                &member.agent,
                &prompt,
                cwd.unwrap_or_else(|| std::path::PathBuf::from("/")),
                conversation_id,
                300,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                self.teams.update_delegation(
                    &delegation.id,
                    "failed",
                    Some(&json!({"error": error.to_string()})),
                    None,
                    None,
                )?;
                self.teams.set_conversation_agent(
                    conversation_id,
                    member_role_id,
                    "failed",
                    None,
                )?;
                return Err(error.into());
            }
        };
        let ok = outcome.get("ok").and_then(Value::as_bool) == Some(true);
        let text = outcome
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let usage = outcome.get("usage").cloned().unwrap_or(Value::Null);
        let artifacts = outcome
            .get("artifacts")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let error = outcome.get("error").cloned().unwrap_or(Value::Null);
        let phase = outcome.get("phase").and_then(Value::as_str).unwrap_or("");
        let current_status = self
            .teams
            .delegation(&delegation.id)?
            .and_then(|value| value.get("status").cloned())
            .and_then(|value| value.as_str().map(str::to_string));
        let status = if current_status.as_deref() == Some("cancelled") {
            "cancelled"
        } else if ok {
            "done"
        } else if phase == "cancelled" {
            "cancelled"
        } else {
            "failed"
        };
        let duration_ms = outcome.get("durationMs").cloned().unwrap_or(Value::Null);
        let result = json!({
            "summary": text,
            "text": text,
            "error": error,
            "phase": outcome.get("phase").cloned().unwrap_or(Value::Null),
            "durationMs": duration_ms,
            "artifacts": artifacts,
        });
        self.teams
            .update_delegation(&delegation.id, status, Some(&result), Some(&usage), None)?;
        self.teams.set_conversation_agent(
            conversation_id,
            member_role_id,
            match status {
                "done" => "done",
                "cancelled" => "cancelled",
                _ => "failed",
            },
            None,
        )?;
        self.emit_team_event(
            conversation_id,
            "delegation_completed",
            Some(member_role_id),
            json!({
                "delegation_id": delegation.id,
                "status": status,
                "summary": result["summary"],
                "usage": usage,
                "durationMs": duration_ms,
                "artifacts": result["artifacts"],
                "error": if ok { Value::Null } else { error.clone() },
            }),
        );

        drop(guard);

        Ok(json!({
            "delegationId": delegation.id,
            "status": status,
            "summary": result["summary"],
            "usage": usage,
            "artifacts": result["artifacts"],
            "error": if ok { Value::Null } else { error },
        }))
    }
}

struct ActiveGuard {
    active: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl ActiveGuard {
    fn enter(active: Arc<Mutex<HashSet<String>>>, key: String) -> Result<Self> {
        if !active.lock().unwrap().insert(key.clone()) {
            bail!("that member is already running a delegation in this conversation");
        }
        Ok(Self { active, key })
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.active.lock().unwrap().remove(&self.key);
    }
}

fn delegation_prompt(
    snapshot: &TeamSnapshot,
    member: &Agent,
    task: &str,
    context: Option<&str>,
    expected_output: Option<&str>,
) -> String {
    let leader = snapshot
        .member(&snapshot.leader_role_id)
        .map(|leader| leader.agent.name.as_str())
        .unwrap_or("the Leader");
    let mut prompt = format!(
        "You are {} on a team led by {leader}.\n\nTask:\n{task}",
        if member.name.is_empty() {
            "a team member"
        } else {
            &member.name
        }
    );
    if let Some(context) = context.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\n\nContext:\n");
        prompt.push_str(context);
    }
    if let Some(expected) = expected_output.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\n\nExpected output:\n");
        prompt.push_str(expected);
    }
    prompt.push_str(
        "\n\nDo not delegate further. Finish the task and return the result your Leader needs.",
    );
    prompt
}

#[cfg(test)]
mod tests {
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
        EventPublisher, EventSubscription, ModelCatalog, SessionRuntime, SkillCatalog,
        TeamRepository,
    };
    use crate::domain::team::TeamSnapshot;
    use crate::infra::store::Store;
    use crate::runtime::engine::registry::Engines;

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
        let leader =
            Agent::from_value(store.save_role(&leader.to_value().unwrap()).unwrap()).unwrap();
        let member =
            Agent::from_value(store.save_role(&member.to_value().unwrap()).unwrap()).unwrap();
        let snapshot =
            TeamSnapshot::new(&leader, std::slice::from_ref(&member), crate::config::now());
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
                    kind: "tool_call".to_string(),
                    connection_id: "test-connection".to_string(),
                    conversation_id: None,
                    speaker_role_id: None,
                    seq: 2,
                    payload: json!({"raw": {"locations": [{"path": "/tmp/result.swift"}]}}),
                },
                EventEnvelope {
                    kind: "usage".to_string(),
                    connection_id: "test-connection".to_string(),
                    conversation_id: None,
                    speaker_role_id: None,
                    seq: 3,
                    payload: json!({"usage": {"input": 2, "output": 1}}),
                },
                EventEnvelope {
                    kind: "turn_complete".to_string(),
                    connection_id: "test-connection".to_string(),
                    conversation_id: None,
                    speaker_role_id: None,
                    seq: 4,
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
}
