use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::Store;
use crate::config::now;

impl Store {
    pub fn list_conversations(&self, folder_ids: &[String]) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        if folder_ids.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, title, agent_type, status, updated_at
                 FROM conversations WHERE deleted_at IS NULL ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([], conversation_json)?;
            for row in rows {
                out.push(row?);
            }
            return Ok(out);
        }
        for folder_id in folder_ids {
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, title, agent_type, status, updated_at
                 FROM conversations
                 WHERE folder_id = ?1 AND deleted_at IS NULL
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map(params![folder_id], conversation_json)?;
            for row in rows {
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
                "SELECT id, folder_id, title, agent_type, status, updated_at
                 FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                conversation_json,
            )
            .optional()?;
        let Some(summary) = summary else {
            return Ok(None);
        };
        let mut stmt = conn.prepare(
            "SELECT id, role, blocks_json, created_at FROM turns
             WHERE conversation_id = ?1 ORDER BY created_at",
        )?;
        let turns = stmt
            .query_map(params![id], |row| {
                let blocks: String = row.get(2)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "role": row.get::<_, String>(1)?,
                    "timestamp": row.get::<_, String>(3)?,
                    "blocks": serde_json::from_str::<Value>(&blocks).unwrap_or(Value::Null),
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(json!({"summary": summary, "turns": turns})))
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO turns (id, conversation_id, role, blocks_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                conversation_id,
                role,
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

fn conversation_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "folder_id": row.get::<_, Option<String>>(1)?,
        "title": row.get::<_, String>(2)?,
        "agent_type": row.get::<_, String>(3)?,
        "status": row.get::<_, String>(4)?,
        "updated_at": row.get::<_, String>(5)?,
        "message_count": 0,
        "child_count": 0,
    }))
}
