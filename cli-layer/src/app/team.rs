//! Team conversations: frozen snapshots, member state and delegation.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::app::readiness::ReadinessService;
use crate::app::roster::RosterService;
use crate::app::sessions::SessionService;
use crate::config::now;
use crate::domain::agent::Agent;
use crate::domain::ports::{ConversationRepository, TeamRepository};
use crate::domain::team::{Delegation, TeamRef, TeamSnapshot};

const MAX_TEAM_MEMBERS: usize = 5;

#[derive(Clone)]
pub struct TeamService {
    roster: RosterService,
    conversations: Arc<dyn ConversationRepository>,
    teams: Arc<dyn TeamRepository>,
    readiness: ReadinessService,
    sessions: SessionService,
    active: Arc<Mutex<HashSet<String>>>,
}

impl TeamService {
    pub fn new(
        roster: RosterService,
        conversations: Arc<dyn ConversationRepository>,
        teams: Arc<dyn TeamRepository>,
        readiness: ReadinessService,
        sessions: SessionService,
    ) -> Self {
        Self {
            roster,
            conversations,
            teams,
            readiness,
            sessions,
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
        Ok(json!({
            "members": self.teams.conversation_agents(conversation_id)?,
            "delegations": self.teams.delegations(conversation_id)?,
        }))
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

        let prompt = delegation_prompt(&snapshot, &member.agent, task, context, expected_output);
        let cwd = self
            .teams
            .conversation_cwd(conversation_id)?
            .map(std::path::PathBuf::from);
        let outcome = match self
            .sessions
            .run_agent(
                &member.agent,
                &prompt,
                cwd,
                Some(conversation_id),
                Some(member_role_id),
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
        let error = outcome.get("error").cloned().unwrap_or(Value::Null);
        let phase = outcome.get("phase").and_then(Value::as_str).unwrap_or("");
        let status = if ok {
            "done"
        } else if phase == "cancelled" {
            "cancelled"
        } else {
            "failed"
        };
        let result = json!({
            "summary": text,
            "text": text,
            "error": error,
            "phase": outcome.get("phase").cloned().unwrap_or(Value::Null),
        });
        self.teams
            .update_delegation(&delegation.id, status, Some(&result), Some(&usage), None)?;
        self.teams.set_conversation_agent(
            conversation_id,
            member_role_id,
            if ok { "done" } else { "failed" },
            None,
        )?;

        drop(guard);

        Ok(json!({
            "delegationId": delegation.id,
            "status": status,
            "summary": result["summary"],
            "usage": usage,
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
