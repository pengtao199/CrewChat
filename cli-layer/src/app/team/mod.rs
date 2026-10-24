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
        let snapshot = self
            .conversations
            .get_conversation(conversation_id)?
            .and_then(|detail| detail.pointer("/summary/team").cloned())
            .and_then(|value| serde_json::from_value::<TeamSnapshot>(value).ok());
        let mut members = self.teams.conversation_agents(conversation_id)?;
        for member in &mut members {
            let role_id = member.get("role_id").and_then(Value::as_str).unwrap_or("");
            if let Some(snapshot) = &snapshot {
                if let Some(role) = snapshot.member(role_id) {
                    member["name"] = json!(role.agent.name);
                    member["agent_type"] = json!(role.agent.agent_type);
                    member["model"] = json!(role.agent.model);
                    member["permission_mode"] = json!(role.agent.permission_mode);
                    member["auto_approve"] = json!(role.agent.auto_approve);
                }
            }
        }
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

    /// Cancel every queued/running member delegation in a conversation.
    /// Used when the user stops the whole team run, so members do not keep
    /// working after the Leader is cancelled.
    pub fn cancel_conversation(&self, conversation_id: &str) -> Result<Value> {
        let delegations = self.teams.delegations(conversation_id)?;
        let mut cancelled = 0usize;
        for delegation in delegations {
            let status = delegation
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(status, "queued" | "running") {
                continue;
            }
            let Some(delegation_id) = delegation.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(member_role_id) = delegation.get("member_role_id").and_then(Value::as_str)
            else {
                continue;
            };
            let connection_cancelled = self.sessions.cancel_role(conversation_id, member_role_id);
            self.teams.update_delegation(
                delegation_id,
                "cancelled",
                Some(&json!({"cancelled": true, "reason": "conversation_cancelled"})),
                None,
                None,
            )?;
            self.teams.set_conversation_agent(
                conversation_id,
                member_role_id,
                if connection_cancelled {
                    "cancelled"
                } else {
                    "idle"
                },
                None,
            )?;
            self.emit_team_event(
                conversation_id,
                "delegation_cancelled",
                Some(member_role_id),
                json!({
                    "delegation_id": delegation_id,
                    "connection_cancelled": connection_cancelled,
                }),
            );
            cancelled += 1;
        }
        self.emit_team_event(
            conversation_id,
            "conversation_cancelled",
            None,
            json!({"cancelled_delegations": cancelled}),
        );
        Ok(json!({
            "conversationId": conversation_id,
            "cancelledDelegations": cancelled,
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
        let timeout_seconds = member
            .agent
            .limits
            .timeout_seconds
            .unwrap_or(300)
            .clamp(1, 3600);
        let outcome = match self
            .sessions
            .run_member(
                &member.agent,
                &prompt,
                cwd.unwrap_or_else(|| std::path::PathBuf::from("/")),
                conversation_id,
                timeout_seconds,
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
mod tests;
