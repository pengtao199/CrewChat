//! Team conversation, member state and delegation persistence.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::Store;
use crate::config::now;
use crate::domain::team::{Delegation, TeamSnapshot};

impl Store {
    pub fn create_team_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
        snapshot: &TeamSnapshot,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let title = title
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("New team conversation")
            .to_string();
        let timestamp = now();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO conversations
             (id, folder_id, title, agent_type, status, kind, leader_role_id,
              team_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', 'team', ?5, ?6, ?7, ?7)",
            params![
                id,
                folder_id,
                title,
                agent_type,
                snapshot.leader_role_id,
                snapshot.to_json()?,
                timestamp
            ],
        )?;
        tx.execute(
            "INSERT INTO conversation_agents
             (conversation_id, role_id, seat, state, joined_at, updated_at)
             VALUES (?1, ?2, 'leader', 'idle', ?3, ?3)",
            params![id, snapshot.leader_role_id, timestamp],
        )?;
        for member in &snapshot.members {
            tx.execute(
                "INSERT INTO conversation_agents
                 (conversation_id, role_id, seat, state, joined_at, updated_at)
                 VALUES (?1, ?2, 'member', 'idle', ?3, ?3)",
                params![id, member.role_id, timestamp],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn conversation_cwd(&self, conversation_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT f.path FROM conversations c
                 JOIN folders f ON f.id = c.folder_id
                 WHERE c.id = ?1 AND c.deleted_at IS NULL",
                params![conversation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    pub fn conversation_agent(
        &self,
        conversation_id: &str,
        role_id: &str,
    ) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT role_id, seat, state, connection_id, external_session_id,
                        joined_at, updated_at
                 FROM conversation_agents
                 WHERE conversation_id = ?1 AND role_id = ?2",
                params![conversation_id, role_id],
                conversation_agent_json,
            )
            .optional()?)
    }

    pub fn conversation_agents(&self, conversation_id: &str) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT role_id, seat, state, connection_id, external_session_id,
                    joined_at, updated_at
             FROM conversation_agents WHERE conversation_id = ?1
             ORDER BY seat = 'leader' DESC, joined_at",
        )?;
        let rows = stmt
            .query_map(params![conversation_id], |row| {
                Ok(json!({
                    "role_id": row.get::<_, String>(0)?,
                    "seat": row.get::<_, String>(1)?,
                    "state": row.get::<_, String>(2)?,
                    "connection_id": row.get::<_, Option<String>>(3)?,
                    "external_session_id": row.get::<_, Option<String>>(4)?,
                    "joined_at": row.get::<_, String>(5)?,
                    "updated_at": row.get::<_, String>(6)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_conversation_agent(
        &self,
        conversation_id: &str,
        role_id: &str,
        state: &str,
        connection_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let timestamp = now();
        conn.execute(
            "INSERT INTO conversation_agents
             (conversation_id, role_id, seat, state, connection_id, joined_at, updated_at)
             VALUES (?1, ?2, 'member', ?3, ?4, ?5, ?5)
             ON CONFLICT(conversation_id, role_id) DO UPDATE SET
                state = excluded.state,
                connection_id = excluded.connection_id,
                updated_at = excluded.updated_at",
            params![conversation_id, role_id, state, connection_id, timestamp],
        )?;
        Ok(())
    }

    pub fn set_conversation_agent_session(
        &self,
        conversation_id: &str,
        role_id: &str,
        external_session_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversation_agents
             SET external_session_id = ?1, updated_at = ?2
             WHERE conversation_id = ?3 AND role_id = ?4",
            params![external_session_id, now(), conversation_id, role_id],
        )?;
        Ok(())
    }

    pub fn create_delegation(&self, delegation: &Delegation) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO delegations
             (id, conversation_id, leader_role_id, member_role_id,
              child_conversation_id, task, context, expected_output,
              status, result_json, usage_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                delegation.id,
                delegation.conversation_id,
                delegation.leader_role_id,
                delegation.member_role_id,
                delegation.child_conversation_id,
                delegation.task,
                delegation.context,
                delegation.expected_output,
                delegation.status,
                delegation
                    .result
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                delegation
                    .usage
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                delegation.created_at,
                delegation.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn update_delegation(
        &self,
        id: &str,
        status: &str,
        result: Option<&Value>,
        usage: Option<&Value>,
        child_conversation_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE delegations SET
                status = ?2,
                result_json = COALESCE(?3, result_json),
                usage_json = COALESCE(?4, usage_json),
                child_conversation_id = COALESCE(?5, child_conversation_id),
                updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                status,
                result.map(serde_json::to_string).transpose()?,
                usage.map(serde_json::to_string).transpose()?,
                child_conversation_id,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn delegation(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT id, conversation_id, leader_role_id, member_role_id,
                        child_conversation_id, task, context, expected_output,
                        status, result_json, usage_json, created_at, updated_at
                 FROM delegations WHERE id = ?1",
                params![id],
                delegation_json,
            )
            .optional()?)
    }

    pub fn delegations(&self, conversation_id: &str) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, leader_role_id, member_role_id,
                    child_conversation_id, task, context, expected_output,
                    status, result_json, usage_json, created_at, updated_at
             FROM delegations WHERE conversation_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map(params![conversation_id], delegation_json)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn conversation_agent_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "role_id": row.get::<_, String>(0)?,
        "seat": row.get::<_, String>(1)?,
        "state": row.get::<_, String>(2)?,
        "connection_id": row.get::<_, Option<String>>(3)?,
        "external_session_id": row.get::<_, Option<String>>(4)?,
        "joined_at": row.get::<_, String>(5)?,
        "updated_at": row.get::<_, String>(6)?,
    }))
}

fn delegation_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let result = row.get::<_, Option<String>>(9)?;
    let usage = row.get::<_, Option<String>>(10)?;
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "conversation_id": row.get::<_, String>(1)?,
        "leader_role_id": row.get::<_, String>(2)?,
        "member_role_id": row.get::<_, String>(3)?,
        "child_conversation_id": row.get::<_, Option<String>>(4)?,
        "task": row.get::<_, String>(5)?,
        "context": row.get::<_, Option<String>>(6)?,
        "expected_output": row.get::<_, Option<String>>(7)?,
        "status": row.get::<_, String>(8)?,
        "result": result.as_deref().and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
        "usage": usage.as_deref().and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
        "created_at": row.get::<_, String>(11)?,
        "updated_at": row.get::<_, String>(12)?,
    }))
}
