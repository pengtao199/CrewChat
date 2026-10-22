//! SQLite store for projects, conversations, turns, roster and event replay.
//!
//! The schema is intentionally small: it is the `cli-layer` replacement for
//! codeg's much larger `db/` (folders + conversations + roster + usage).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::config::now;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS folders (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                alias TEXT NOT NULL DEFAULT '',
                sort_order INTEGER NOT NULL DEFAULT 0,
                is_open INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                folder_id TEXT,
                title TEXT NOT NULL,
                agent_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                external_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE TABLE IF NOT EXISTS turns (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                role TEXT NOT NULL,
                blocks_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_roles (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                soul TEXT NOT NULL DEFAULT '',
                rules TEXT NOT NULL DEFAULT '',
                agent_type TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                model_provider TEXT NOT NULL DEFAULT '',
                skills_json TEXT NOT NULL DEFAULT '[]',
                mcp_json TEXT NOT NULL DEFAULT '[]',
                permission_mode TEXT NOT NULL DEFAULT 'default',
                auto_approve INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                connection_id TEXT NOT NULL,
                conversation_id TEXT,
                envelope_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- folders

    pub fn list_folders(&self) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, path, name, alias, sort_order FROM folders
             WHERE is_open = 1 ORDER BY sort_order, name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "path": row.get::<_, String>(1)?,
                    "name": row.get::<_, String>(2)?,
                    "alias": row.get::<_, String>(3)?,
                    "sort_order": row.get::<_, i64>(4)?,
                    "kind": "regular",
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn open_folder(&self, path: &str) -> Result<Value> {
        let path = path.trim();
        if path.is_empty() {
            anyhow::bail!("path is required");
        }
        let name = Path::new(path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Workspace")
            .to_string();
        let conn = self.conn.lock().unwrap();
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM folders WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            conn.execute("UPDATE folders SET is_open = 1 WHERE id = ?1", params![id])?;
            return Ok(json!({
                "id": id, "path": path, "name": name, "alias": "",
                "sort_order": 0, "kind": "regular"
            }));
        }
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO folders (id, path, name, alias, sort_order, is_open)
             VALUES (?1, ?2, ?3, '', 0, 1)",
            params![id, path, name],
        )?;
        Ok(json!({
            "id": id, "path": path, "name": name, "alias": "",
            "sort_order": 0, "kind": "regular"
        }))
    }

    pub fn remove_folder(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE folders SET is_open = 0 WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ----------------------------------------------------------- conversations

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

    // ----------------------------------------------------------------- roster

    pub fn list_roles(&self) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, soul, rules, agent_type, model, model_provider,
                    skills_json, mcp_json, permission_mode, auto_approve
             FROM agent_roles ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "soul": row.get::<_, String>(2)?,
                    "rules": row.get::<_, String>(3)?,
                    "agent_type": row.get::<_, String>(4)?,
                    "model": row.get::<_, String>(5)?,
                    "model_provider": row.get::<_, String>(6)?,
                    "skills": serde_json::from_str::<Value>(&row.get::<_, String>(7)?)
                        .unwrap_or(Value::Null),
                    "mcp": serde_json::from_str::<Value>(&row.get::<_, String>(8)?)
                        .unwrap_or(Value::Null),
                    "permission_mode": row.get::<_, String>(9)?,
                    "auto_approve": row.get::<_, i64>(10)? != 0,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn role(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT id, name, soul, rules, agent_type, model, model_provider,
                    skills_json, mcp_json, permission_mode, auto_approve
             FROM agent_roles WHERE id = ?1",
                params![id],
                |row| {
                    Ok(json!({
                        "id": row.get::<_, String>(0)?,
                        "name": row.get::<_, String>(1)?,
                        "soul": row.get::<_, String>(2)?,
                        "rules": row.get::<_, String>(3)?,
                        "agent_type": row.get::<_, String>(4)?,
                        "model": row.get::<_, String>(5)?,
                        "model_provider": row.get::<_, String>(6)?,
                        "skills": serde_json::from_str::<Value>(&row.get::<_, String>(7)?)
                            .unwrap_or(Value::Null),
                        "mcp": serde_json::from_str::<Value>(&row.get::<_, String>(8)?)
                            .unwrap_or(Value::Null),
                        "permission_mode": row.get::<_, String>(9)?,
                        "auto_approve": row.get::<_, i64>(10)? != 0,
                    }))
                },
            )
            .optional()?)
    }

    pub fn save_role(&self, role: &Value) -> Result<Value> {
        let id = role
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let timestamp = now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_roles
             (id, name, soul, rules, agent_type, model, model_provider,
              skills_json, mcp_json, permission_mode, auto_approve, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                soul = excluded.soul,
                rules = excluded.rules,
                agent_type = excluded.agent_type,
                model = excluded.model,
                model_provider = excluded.model_provider,
                skills_json = excluded.skills_json,
                mcp_json = excluded.mcp_json,
                permission_mode = excluded.permission_mode,
                auto_approve = excluded.auto_approve,
                updated_at = excluded.updated_at",
            params![
                id,
                role.get("name").and_then(Value::as_str).unwrap_or("Agent"),
                role.get("soul").and_then(Value::as_str).unwrap_or(""),
                role.get("rules").and_then(Value::as_str).unwrap_or(""),
                role.get("agent_type")
                    .and_then(Value::as_str)
                    .unwrap_or("claude_code"),
                role.get("model").and_then(Value::as_str).unwrap_or(""),
                role.get("model_provider")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                serde_json::to_string(&role.get("skills").cloned().unwrap_or_else(|| json!([])),)?,
                serde_json::to_string(&role.get("mcp").cloned().unwrap_or_else(|| json!([])),)?,
                role.get("permission_mode")
                    .and_then(Value::as_str)
                    .unwrap_or("default"),
                role.get("auto_approve")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) as i64,
                timestamp
            ],
        )?;
        let mut saved = role.clone();
        if !saved.is_object() {
            saved = json!({});
        }
        saved["id"] = json!(id);
        saved["created_at"] = json!(timestamp);
        saved["updated_at"] = json!(timestamp);
        Ok(saved)
    }

    pub fn delete_role(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM agent_roles WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ----------------------------------------------------------------- events

    pub fn append_event(
        &self,
        connection_id: &str,
        conversation_id: Option<&str>,
        envelope: &Value,
    ) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events (connection_id, conversation_id, envelope_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                connection_id,
                conversation_id,
                serde_json::to_string(envelope)?,
                now()
            ],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    pub fn events_since(&self, connection_id: &str, since_seq: u64) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT envelope_json FROM events
             WHERE connection_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![connection_id, since_seq as i64], |row| {
                let raw: String = row.get(0)?;
                Ok(serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("test.db")).unwrap();
        store.init_schema().unwrap();
        (dir, store)
    }

    #[test]
    fn folders_and_conversations_round_trip() {
        let (_dir, store) = store();
        let folder = store.open_folder("/tmp/workspace").unwrap();
        let folder_id = folder["id"].as_str().unwrap();
        let conversation = store
            .create_conversation(Some(folder_id), "claude_code", Some("Hello"))
            .unwrap();
        let rows = store.list_conversations(&[folder_id.to_string()]).unwrap();
        assert_eq!(rows.len(), 1);
        let detail = store.get_conversation(&conversation).unwrap().unwrap();
        assert_eq!(detail["summary"]["title"], "Hello");
    }

    #[test]
    fn roles_round_trip() {
        let (_dir, store) = store();
        let role = json!({
            "name": "Builder",
            "agent_type": "claude_code",
            "model": "mimo-v2.6-flash",
            "model_provider": "opencode-go",
            "skills": ["codegraph"],
            "mcp": ["xcode"]
        });
        let saved = store.save_role(&role).unwrap();
        assert_eq!(saved["name"], "Builder");
        let rows = store.list_roles().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["skills"][0], "codegraph");
        let id = rows[0]["id"].as_str().unwrap();
        assert!(store.role(id).unwrap().is_some());
        store.delete_role(id).unwrap();
        assert_eq!(store.list_roles().unwrap().len(), 0);
    }

    #[test]
    fn events_are_replayable() {
        let (_dir, store) = store();
        store
            .append_event("c1", None, &json!({"type": "content_delta", "seq": 1}))
            .unwrap();
        store
            .append_event("c1", None, &json!({"type": "turn_complete", "seq": 2}))
            .unwrap();
        let rows = store.events_since("c1", 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["type"], "turn_complete");
    }
}
