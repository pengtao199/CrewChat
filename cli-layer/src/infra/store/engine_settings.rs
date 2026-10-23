use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use super::{EnginePreferences, Store};
use crate::config::now;

impl Store {
    pub fn engine_preferences(&self, agent_type: &str) -> Result<EnginePreferences> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT enabled, env_json FROM engine_settings WHERE agent_type = ?1",
                params![agent_type],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((enabled, raw)) => EnginePreferences {
                enabled: enabled != 0,
                env: serde_json::from_str(&raw).unwrap_or_default(),
            },
            None => EnginePreferences::default(),
        })
    }

    pub fn save_engine_preferences(
        &self,
        agent_type: &str,
        enabled: bool,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO engine_settings (agent_type, enabled, env_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_type) DO UPDATE SET
                enabled = excluded.enabled,
                env_json = excluded.env_json,
                updated_at = excluded.updated_at",
            params![
                agent_type,
                enabled as i64,
                serde_json::to_string(env)?,
                now()
            ],
        )?;
        Ok(())
    }
}
