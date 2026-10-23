use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use super::{ImportReport, Store};
use crate::config::now;

impl Store {
    /// One-time import from codeg's SQLite. Old transcripts are not parsed;
    /// folders, conversations and the roster are enough to keep the workspace
    /// navigable while `cli-layer` takes over new sessions.
    pub fn import_codeg(&self, path: &Path) -> Result<ImportReport> {
        let codeg = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open {}", path.display()))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut report = ImportReport::default();

        {
            let mut stmt = codeg.prepare(
                "SELECT id, path, name, alias, sort_order, is_open FROM folder
                 WHERE deleted_at IS NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)? != 0,
                ))
            })?;
            for row in rows {
                let (id, path, name, alias, sort_order, is_open) = row?;
                tx.execute(
                    "INSERT OR REPLACE INTO folders
                     (id, path, name, alias, sort_order, is_open)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        id.to_string(),
                        path,
                        name,
                        alias.unwrap_or_default(),
                        sort_order,
                        is_open as i64
                    ],
                )?;
                report.folders += 1;
            }
        }

        {
            let mut stmt = codeg.prepare(
                "SELECT id, folder_id, title, agent_type, status, external_id,
                        created_at, updated_at
                 FROM conversation WHERE deleted_at IS NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })?;
            for row in rows {
                let (id, folder_id, title, agent_type, status, external_id, created, updated) =
                    row?;
                tx.execute(
                    "INSERT OR REPLACE INTO conversations
                     (id, folder_id, title, agent_type, status, external_id,
                      created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id.to_string(),
                        folder_id.to_string(),
                        title.unwrap_or_else(|| "Conversation".to_string()),
                        agent_type,
                        status,
                        external_id,
                        created,
                        updated
                    ],
                )?;
                report.conversations += 1;
            }
        }

        {
            let mut stmt = codeg.prepare(
                "SELECT id, name, instructions, rules, body, brain_model, brain_provider,
                        skills_json, mcp_json, permission_mode, auto_approve
                 FROM custom_agent WHERE kind = 'role'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)? != 0,
                ))
            })?;
            for row in rows {
                let (
                    id,
                    name,
                    instructions,
                    rules,
                    body,
                    model,
                    provider,
                    skills,
                    mcp,
                    permission,
                    auto,
                ) = row?;
                let agent_type = if body.trim().is_empty() {
                    "claude_code".to_string()
                } else {
                    body
                };
                let timestamp = now();
                let permission = if permission.trim().is_empty() {
                    "default".to_string()
                } else {
                    permission
                };
                tx.execute(
                    "INSERT OR REPLACE INTO agent_roles
                     (id, name, soul, rules, agent_type, model, model_provider,
                      skills_json, mcp_json, permission_mode, auto_approve,
                      created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                    params![
                        format!("legacy-role-{id}"),
                        name,
                        instructions,
                        rules,
                        agent_type,
                        model,
                        provider,
                        skills,
                        mcp,
                        permission,
                        auto as i64,
                        timestamp
                    ],
                )?;
                report.roles += 1;
            }
        }

        tx.commit()?;
        Ok(report)
    }
}
