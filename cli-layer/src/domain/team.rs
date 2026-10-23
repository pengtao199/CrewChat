//! Team snapshot and delegation types.
//!
//! A team is frozen when the conversation is created. The snapshot contains the
//! full Agent revision (not just an id), so editing the roster later cannot
//! silently change a running team.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::agent::Agent;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamRef {
    pub leader_role_id: String,
    pub member_role_ids: Vec<String>,
}

impl TeamRef {
    pub fn from_value(value: &Value) -> Result<Self> {
        let leader_role_id = value
            .get("leader_role_id")
            .or_else(|| value.get("leaderRoleId"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("team.leader_role_id is required"))?
            .to_string();
        let member_role_ids = value
            .get("member_role_ids")
            .or_else(|| value.get("memberRoleIDs"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(Self {
            leader_role_id,
            member_role_ids,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMemberSnapshot {
    pub role_id: String,
    pub version: u64,
    pub agent: Agent,
}

impl TeamMemberSnapshot {
    pub fn new(agent: &Agent) -> Self {
        Self {
            role_id: agent.id.clone(),
            version: agent.version,
            agent: agent.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamSnapshot {
    pub leader_role_id: String,
    pub leader: TeamMemberSnapshot,
    pub members: Vec<TeamMemberSnapshot>,
    pub created_at: String,
}

impl TeamSnapshot {
    pub fn new(leader: &Agent, members: &[Agent], created_at: String) -> Self {
        Self {
            leader_role_id: leader.id.clone(),
            leader: TeamMemberSnapshot::new(leader),
            members: members.iter().map(TeamMemberSnapshot::new).collect(),
            created_at,
        }
    }

    pub fn all(&self) -> impl Iterator<Item = &TeamMemberSnapshot> {
        std::iter::once(&self.leader).chain(self.members.iter())
    }

    pub fn member(&self, role_id: &str) -> Option<&TeamMemberSnapshot> {
        self.members
            .iter()
            .find(|member| member.role_id == role_id)
            .or_else(|| (self.leader.role_id == role_id).then_some(&self.leader))
    }

    pub fn from_json(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).map_err(|error| anyhow!("invalid team snapshot: {error}"))
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Delegation {
    pub id: String,
    pub conversation_id: String,
    pub leader_role_id: String,
    pub member_role_id: String,
    pub child_conversation_id: Option<String>,
    pub task: String,
    pub context: Option<String>,
    pub expected_output: Option<String>,
    pub status: String,
    pub result: Option<Value>,
    pub usage: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl Delegation {
    pub fn new(
        conversation_id: &str,
        leader_role_id: &str,
        member_role_id: &str,
        task: &str,
        context: Option<&str>,
        expected_output: Option<&str>,
        created_at: String,
    ) -> Self {
        Self {
            id: format!("dlg_{}", uuid::Uuid::new_v4()),
            conversation_id: conversation_id.to_string(),
            leader_role_id: leader_role_id.to_string(),
            member_role_id: member_role_id.to_string(),
            child_conversation_id: None,
            task: task.to_string(),
            context: context.map(str::to_string),
            expected_output: expected_output.map(str::to_string),
            status: "queued".to_string(),
            result: None,
            usage: None,
            created_at: created_at.clone(),
            updated_at: created_at,
        }
    }
}
