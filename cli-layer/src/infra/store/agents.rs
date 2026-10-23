use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::Store;
use crate::config::now;

impl Store {
    pub fn list_roles(&self) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, soul, rules, agent_type, model, model_provider,
                    skills_json, mcp_json, permission_mode, auto_approve, version
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
                    "version": row.get::<_, i64>(11)?,
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
                    skills_json, mcp_json, permission_mode, auto_approve, version
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
                        "version": row.get::<_, i64>(11)?,
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
        let current_version: Option<i64> = conn
            .query_row(
                "SELECT version FROM agent_roles WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let version = current_version.map(|value| value + 1).unwrap_or(1);
        conn.execute(
            "INSERT INTO agent_roles
             (id, name, soul, rules, agent_type, model, model_provider,
              skills_json, mcp_json, permission_mode, auto_approve, version,
              created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
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
                version = excluded.version,
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
                version,
                timestamp
            ],
        )?;
        let mut saved = role.clone();
        if !saved.is_object() {
            saved = json!({});
        }
        saved["id"] = json!(id);
        saved["version"] = json!(version);
        saved["created_at"] = json!(timestamp);
        saved["updated_at"] = json!(timestamp);
        Ok(saved)
    }

    pub fn delete_role(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM agent_roles WHERE id = ?1", params![id])?;
        Ok(())
    }
}
