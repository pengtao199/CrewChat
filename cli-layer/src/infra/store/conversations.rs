use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::Store;
use crate::config::now;
use crate::domain::team::{aggregate_usage, TeamSnapshot};

const SUMMARY_COLUMNS: &str =
    "id, folder_id, title, agent_type, status, updated_at, kind, leader_role_id, team_json, external_id";

impl Store {
    pub fn list_conversations(&self, folder_ids: &[String]) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        if folder_ids.is_empty() {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SUMMARY_COLUMNS} FROM conversations
                 WHERE deleted_at IS NULL ORDER BY updated_at DESC"
            ))?;
            for row in stmt.query_map([], conversation_json)? {
                out.push(row?);
            }
            return Ok(out);
        }
        for folder_id in folder_ids {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SUMMARY_COLUMNS} FROM conversations
                 WHERE folder_id = ?1 AND deleted_at IS NULL
                 ORDER BY updated_at DESC"
            ))?;
            for row in stmt.query_map(params![folder_id], conversation_json)? {
                out.push(row?);
            }
        }
        Ok(out)
    }

    pub fn create_conversation(
        &self,
        folder_id: Option<&str>,
        agent_type: &str,
        title: Option<&str>,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let title = title
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("New conversation")
            .to_string();
        let timestamp = now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations
             (id, folder_id, title, agent_type, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5)",
            params![id, folder_id, title, agent_type, timestamp],
        )?;
        Ok(id)
    }

    pub fn get_conversation(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let summary: Option<Value> = conn
            .query_row(
                &format!(
                    "SELECT {SUMMARY_COLUMNS} FROM conversations
                     WHERE id = ?1 AND deleted_at IS NULL"
                ),
                params![id],
                conversation_json,
            )
            .optional()?;
        let Some(mut summary) = summary else {
            return Ok(None);
        };

        let team_json = conn
            .query_row(
                "SELECT team_json FROM conversations WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let snapshot = team_json
            .as_deref()
            .and_then(|raw| TeamSnapshot::from_json(raw).ok());

        let mut stmt = conn.prepare(
            "SELECT id, role, blocks_json, created_at, speaker_role_id FROM turns
             WHERE conversation_id = ?1 ORDER BY created_at",
        )?;
        let turns = stmt
            .query_map(params![id], |row| {
                let blocks: String = row.get(2)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "role": row.get::<_, String>(1)?,
                    "timestamp": row.get::<_, String>(3)?,
                    "speaker_role_id": row.get::<_, Option<String>>(4)?,
                    "blocks": serde_json::from_str::<Value>(&blocks).unwrap_or(Value::Null),
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let agents = {
            let mut stmt = conn.prepare(
                "SELECT role_id, seat, state, connection_id, joined_at, updated_at
                 FROM conversation_agents WHERE conversation_id = ?1
                 ORDER BY seat = 'leader' DESC, joined_at",
            )?;
            let rows = stmt
                .query_map(params![id], |row| {
                    Ok(json!({
                        "role_id": row.get::<_, String>(0)?,
                        "seat": row.get::<_, String>(1)?,
                        "state": row.get::<_, String>(2)?,
                        "connection_id": row.get::<_, Option<String>>(3)?,
                        "joined_at": row.get::<_, String>(4)?,
                        "updated_at": row.get::<_, String>(5)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let delegations = self_delegations(&conn, id)?;
        if let Some(snapshot) = &snapshot {
            summary["team"] = serde_json::to_value(snapshot)?;
            let members = snapshot
                .members
                .iter()
                .map(|member| {
                    let row = agents.iter().find(|row| row["role_id"] == member.role_id);
                    let member_delegations = delegations
                        .iter()
                        .filter(|delegation| delegation["member_role_id"] == member.role_id)
                        .cloned()
                        .collect::<Vec<_>>();
                    json!({
                        "role_id": member.role_id,
                        "name": member.agent.name,
                        "version": member.version,
                        "agent_type": member.agent.agent_type,
                        "model": member.agent.model,
                        "permission_mode": member.agent.permission_mode,
                        "auto_approve": member.agent.auto_approve,
                        "state": row.and_then(|row| row["state"].as_str()).unwrap_or("idle"),
                        "connection_id": row.map(|row| row["connection_id"].clone()),
                        "usage": aggregate_usage(&member_delegations),
                    })
                })
                .collect::<Vec<_>>();
            summary["members"] = json!(members);
        }
        summary["usage"] = aggregate_usage(&delegations);
        summary["delegations"] = json!(delegations);

        Ok(Some(json!({"summary": summary, "turns": turns})))
    }

    pub fn set_external_id(&self, conversation_id: &str, external_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET external_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![external_id, now(), conversation_id],
        )?;
        Ok(())
    }

    pub fn delete_conversation(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET deleted_at = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        Ok(())
    }

    pub fn append_turn(&self, conversation_id: &str, role: &str, blocks: &Value) -> Result<()> {
        self.append_turn_with_speaker(conversation_id, role, None, blocks)
    }

    pub fn append_turn_with_speaker(
        &self,
        conversation_id: &str,
        role: &str,
        speaker_role_id: Option<&str>,
        blocks: &Value,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO turns
             (id, conversation_id, role, speaker_role_id, blocks_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                uuid::Uuid::new_v4().to_string(),
                conversation_id,
                role,
                speaker_role_id,
                serde_json::to_string(blocks)?,
                now()
            ],
        )?;
        conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now(), conversation_id],
        )?;
        Ok(())
    }
}

fn self_delegations(conn: &rusqlite::Connection, conversation_id: &str) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, leader_role_id, member_role_id,
                child_conversation_id, task, context, expected_output,
                status, result_json, usage_json, created_at, updated_at
         FROM delegations WHERE conversation_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![conversation_id], |row| {
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
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn conversation_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let leader_role_id = row.get::<_, Option<String>>(7)?;
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "folder_id": row.get::<_, Option<String>>(1)?,
        "title": row.get::<_, String>(2)?,
        "agent_type": row.get::<_, String>(3)?,
        "status": row.get::<_, String>(4)?,
        "updated_at": row.get::<_, String>(5)?,
        "kind": row.get::<_, String>(6)?,
        "leader_role_id": leader_role_id,
        "external_id": row.get::<_, Option<String>>(9)?,
        "team": leader_role_id.map(|id| json!({"leader_role_id": id})),
        "message_count": 0,
        "child_count": 0,
    }))
}
