use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSpec {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(
        default,
        alias = "bearer_token",
        skip_serializing_if = "Option::is_none"
    )]
    pub bearer_token: Option<String>,
    #[serde(
        default,
        alias = "session_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_mode: Option<String>,
}

fn default_transport() -> String {
    "stdio".to_string()
}

fn default_enabled() -> bool {
    true
}

impl ServerSpec {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        if self.name.trim().is_empty() {
            bail!("server name is required");
        }
        match self.transport.as_str() {
            "stdio" => {
                if self
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    bail!("stdio server requires command");
                }
            }
            "http" | "streamable_http" | "sse" => {
                if self
                    .url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    bail!("remote server requires url");
                }
            }
            other => bail!("unsupported transport: {other}"),
        }
        if let Some(mode) = self.session_mode.as_deref() {
            if !matches!(mode, "stateless" | "stateful") {
                bail!("sessionMode must be stateless or stateful");
            }
        }
        Ok(())
    }

    pub fn gateway_transport(&self) -> &str {
        match self.transport.as_str() {
            "http" | "streamable_http" => "streamable_http",
            other => other,
        }
    }

    pub fn gateway_payload(&self) -> Value {
        let mut payload = json!({
            "name": self.id,
            "transport": self.gateway_transport(),
            "description": self.description,
        });
        match self.transport.as_str() {
            "stdio" => {
                payload["command"] = json!(self.command.clone().unwrap_or_default());
                payload["args"] = json!(self.args);
                payload["env"] = json!(self.env);
            }
            _ => {
                payload["url"] = json!(self.url.clone().unwrap_or_default());
                if !self.headers.is_empty() {
                    payload["headers"] = json!(self.headers);
                }
                if let Some(token) = &self.bearer_token {
                    payload["bearer_token"] = json!(token);
                }
            }
        }
        if let Some(mode) = &self.session_mode {
            payload["session_mode"] = json!(mode);
        }
        payload
    }

    pub fn hash(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRecord {
    pub id: String,
    pub spec: ServerSpec,
    pub enabled: bool,
    pub synced: bool,
    pub hash: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRecord {
    pub agent_id: String,
    pub client_name: String,
    pub token: String,
    pub server_ids: Vec<String>,
    pub updated_at: String,
}

pub struct Registry {
    conn: Connection,
}

impl Registry {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open mcp registry {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let registry = Self { conn };
        registry.init()?;
        Ok(registry)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS servers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                transport TEXT NOT NULL,
                spec_json TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                synced INTEGER NOT NULL DEFAULT 0,
                hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agents (
                agent_id TEXT PRIMARY KEY,
                client_name TEXT NOT NULL UNIQUE,
                token TEXT NOT NULL,
                server_ids_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn list_servers(&self) -> Result<Vec<ServerRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, spec_json, enabled, synced, hash, created_at, updated_at
             FROM servers ORDER BY id",
        )?;
        let rows = stmt.query_map([], map_server)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn get_server(&self, id: &str) -> Result<Option<ServerRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, spec_json, enabled, synced, hash, created_at, updated_at
             FROM servers WHERE id = ?1",
        )?;
        Ok(stmt.query_row(params![id], map_server).optional()?)
    }

    pub fn upsert_server(&mut self, spec: &ServerSpec) -> Result<ServerRecord> {
        spec.validate()?;
        let now = now_rfc3339();
        let hash = spec.hash()?;
        let existing = self.get_server(&spec.id)?;
        let created_at = existing
            .as_ref()
            .map(|record| record.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let spec_json = serde_json::to_string_pretty(spec)?;
        self.conn.execute(
            r#"
            INSERT INTO servers (id, name, transport, spec_json, enabled, synced, hash, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                transport = excluded.transport,
                spec_json = excluded.spec_json,
                enabled = excluded.enabled,
                hash = excluded.hash,
                updated_at = excluded.updated_at
            "#,
            params![
                spec.id,
                spec.name,
                spec.transport,
                spec_json,
                i64::from(spec.enabled),
                hash,
                created_at,
                now,
            ],
        )?;
        self.get_server(&spec.id)?
            .ok_or_else(|| anyhow::anyhow!("server disappeared after upsert"))
    }

    pub fn set_synced(&mut self, id: &str, synced: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE servers SET synced = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, i64::from(synced), now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE servers SET enabled = ?2, synced = 0, updated_at = ?3 WHERE id = ?1",
            params![id, i64::from(enabled), now_rfc3339()],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_server(&mut self, id: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM servers WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT agent_id, client_name, token, server_ids_json, updated_at
             FROM agents ORDER BY agent_id",
        )?;
        let rows = stmt.query_map([], map_agent)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn get_agent(&self, agent_id: &str) -> Result<Option<AgentRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT agent_id, client_name, token, server_ids_json, updated_at
             FROM agents WHERE agent_id = ?1",
        )?;
        Ok(stmt.query_row(params![agent_id], map_agent).optional()?)
    }

    pub fn upsert_agent(
        &mut self,
        agent_id: &str,
        client_name: &str,
        token: &str,
        server_ids: &[String],
    ) -> Result<AgentRecord> {
        let now = now_rfc3339();
        let ids = serde_json::to_string(server_ids)?;
        self.conn.execute(
            r#"
            INSERT INTO agents (agent_id, client_name, token, server_ids_json, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(agent_id) DO UPDATE SET
                client_name = excluded.client_name,
                token = excluded.token,
                server_ids_json = excluded.server_ids_json,
                updated_at = excluded.updated_at
            "#,
            params![agent_id, client_name, token, ids, now],
        )?;
        self.get_agent(agent_id)?
            .ok_or_else(|| anyhow::anyhow!("agent disappeared after upsert"))
    }

    pub fn delete_agent(&mut self, agent_id: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM agents WHERE agent_id = ?1", params![agent_id])?;
        Ok(changed > 0)
    }

    /// Remove deleted servers from every Agent allow-list.
    pub fn remove_server_from_agents(&mut self, server_id: &str) -> Result<()> {
        let agents = self.list_agents()?;
        for mut agent in agents {
            if agent.server_ids.iter().any(|id| id == server_id) {
                agent.server_ids.retain(|id| id != server_id);
                self.upsert_agent(
                    &agent.agent_id,
                    &agent.client_name,
                    &agent.token,
                    &agent.server_ids,
                )?;
            }
        }
        Ok(())
    }
}

fn map_server(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServerRecord> {
    let spec_json: String = row.get(1)?;
    let spec: ServerSpec = serde_json::from_str(&spec_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ServerRecord {
        id: row.get(0)?,
        spec,
        enabled: row.get::<_, i64>(2)? != 0,
        synced: row.get::<_, i64>(3)? != 0,
        hash: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn map_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
    let ids_json: String = row.get(3)?;
    let server_ids: Vec<String> = serde_json::from_str(&ids_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(AgentRecord {
        agent_id: row.get(0)?,
        client_name: row.get(1)?,
        token: row.get(2)?,
        server_ids,
        updated_at: row.get(4)?,
    })
}

pub fn write_server_file(dir: &Path, spec: &ServerSpec) -> Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", spec.id));
    let bytes = serde_json::to_vec_pretty(spec)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn remove_server_file(dir: &Path, id: &str) -> Result<()> {
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 80
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        bail!("invalid server id: {id}");
    }
    Ok(())
}

pub fn sanitize_agent_id(agent_id: &str) -> Result<String> {
    if agent_id.trim().is_empty() {
        bail!("invalid agent id: {agent_id}");
    }
    let safe: String = agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if safe.is_empty() {
        bail!("invalid agent id: {agent_id}");
    }
    Ok(format!("agent-{safe}"))
}

pub fn generate_agent_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
