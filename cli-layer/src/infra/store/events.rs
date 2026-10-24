use anyhow::Result;
use rusqlite::params;
use serde_json::Value;

use super::Store;
use crate::config::now;

impl Store {
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

    pub fn events_for_conversation(
        &self,
        conversation_id: &str,
        since_seq: u64,
    ) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT envelope_json FROM events
             WHERE conversation_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![conversation_id, since_seq as i64], |row| {
                let raw: String = row.get(0)?;
                Ok(serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
