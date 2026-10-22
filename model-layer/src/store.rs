//! SQLite store for providers, credentials, models and caller keys.
//!
//! Raw provider JSON is kept verbatim (`providers.raw_json`) so fields the UI
//! does not know about (`account`, `capabilities`, `modelDisplayNames`, …)
//! survive a round trip. Normalized columns exist only for routing and the
//! model list.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::model_id::ProviderView;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CredentialRow {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    pub api_key: String,
    pub priority: i64,
    pub weight: i64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProviderRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub raw_json: String,
    pub enabled: bool,
    pub position: i64,
}

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

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init_schema()?;
        Ok(store)
    }

    pub fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                base_url TEXT NOT NULL,
                raw_json TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                position INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS provider_models (
                provider_id TEXT NOT NULL,
                model TEXT NOT NULL,
                one_million INTEGER NOT NULL DEFAULT 0,
                routable INTEGER NOT NULL DEFAULT 1,
                auto_fetch INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                display_name TEXT,
                position INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (provider_id, model)
            );

            CREATE TABLE IF NOT EXISTS credentials (
                id TEXT PRIMARY KEY,
                provider_id TEXT NOT NULL,
                name TEXT NOT NULL,
                api_key TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 0,
                weight INTEGER NOT NULL DEFAULT 1,
                enabled INTEGER NOT NULL DEFAULT 1,
                limits_json TEXT,
                position INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS caller_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                limits_json TEXT,
                enabled INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS disabled_models (
                provider_id TEXT NOT NULL,
                model TEXT NOT NULL,
                PRIMARY KEY (provider_id, model)
            );

            CREATE TABLE IF NOT EXISTS legacy_config (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS quota_snapshots (
                credential_id TEXT PRIMARY KEY,
                meters_json TEXT NOT NULL,
                fetched_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- import

    /// Replace the catalog with one CCR config. Full replace keeps the store
    /// and CCR from disagreeing; `legacy_config.raw` preserves unknown fields.
    pub fn apply_config(&self, config: &Value) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        tx.execute("DELETE FROM providers", [])?;
        tx.execute("DELETE FROM provider_models", [])?;
        tx.execute("DELETE FROM credentials", [])?;
        tx.execute("DELETE FROM caller_keys", [])?;
        tx.execute("DELETE FROM disabled_models", [])?;

        let providers = config
            .get("Providers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for (position, provider) in providers.iter().enumerate() {
            let object = provider.as_object().cloned().unwrap_or_default();
            let name = str_field(&object, &["name"]).unwrap_or_else(|| "Provider".to_string());
            let id = str_field(&object, &["id"]).unwrap_or_else(|| slug(&name));
            let kind = str_field(&object, &["type"])
                .unwrap_or_else(|| "openai_chat_completions".to_string());
            let base_url = str_field(&object, &["api_base_url", "baseUrl"]).unwrap_or_default();
            let enabled = object
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);

            tx.execute(
                "INSERT INTO providers (id, name, kind, base_url, raw_json, enabled, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    name,
                    kind,
                    base_url,
                    serde_json::to_string(provider)?,
                    enabled as i64,
                    position as i64
                ],
            )?;

            let disabled: Vec<String> = object
                .get("disabledModels")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            let display_names = object.get("modelDisplayNames").and_then(Value::as_object);
            let mut model_position = 0i64;

            if let Some(models) = object.get("models").and_then(Value::as_array) {
                for entry in models {
                    let Some(raw) = entry.as_str() else { continue };
                    let (model, one_million) = strip_one_million(raw);
                    if model.is_empty() {
                        continue;
                    }
                    let is_disabled = disabled.iter().any(|m| m.eq_ignore_ascii_case(&model));
                    tx.execute(
                        "INSERT OR REPLACE INTO provider_models
                         (provider_id, model, one_million, routable, auto_fetch, enabled, display_name, position)
                         VALUES (?1, ?2, ?3, 1, 0, ?4, ?5, ?6)",
                        params![
                            id,
                            model,
                            one_million as i64,
                            (!is_disabled) as i64,
                            display_names
                                .and_then(|m| m.get(&model))
                                .and_then(Value::as_str),
                            model_position
                        ],
                    )?;
                    model_position += 1;
                }
            }

            if let Some(models) = object.get("autoFetchKnownModels").and_then(Value::as_array) {
                for entry in models {
                    let Some(raw) = entry.as_str() else { continue };
                    let (model, one_million) = strip_one_million(raw);
                    if model.is_empty() {
                        continue;
                    }
                    let is_disabled = disabled.iter().any(|m| m.eq_ignore_ascii_case(&model));
                    tx.execute(
                        "INSERT INTO provider_models
                         (provider_id, model, one_million, routable, auto_fetch, enabled, display_name, position)
                         VALUES (?1, ?2, ?3, 0, 1, ?4, ?5, ?6)
                         ON CONFLICT(provider_id, model) DO UPDATE SET
                            auto_fetch = 1,
                            one_million = MAX(one_million, excluded.one_million)",
                        params![
                            id,
                            model,
                            one_million as i64,
                            (!is_disabled) as i64,
                            display_names
                                .and_then(|m| m.get(&model))
                                .and_then(Value::as_str),
                            model_position
                        ],
                    )?;
                    model_position += 1;
                }
            }

            for (model_position, model) in disabled.iter().enumerate() {
                let (model, _) = strip_one_million(model);
                tx.execute(
                    "INSERT OR IGNORE INTO disabled_models (provider_id, model) VALUES (?1, ?2)",
                    params![id, model],
                )?;
                tx.execute(
                    "UPDATE provider_models SET enabled = 0 WHERE provider_id = ?1 AND model = ?2",
                    params![id, model],
                )?;
                let _ = model_position;
            }

            if let Some(credentials) = object.get("credentials").and_then(Value::as_array) {
                for (position, credential) in credentials.iter().enumerate() {
                    let Some(credential) = credential.as_object() else {
                        continue;
                    };
                    let Some(api_key) = str_field(credential, &["api_key", "apiKey", "apikey"])
                    else {
                        continue;
                    };
                    let credential_id = str_field(credential, &["id"])
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    let credential_name =
                        str_field(credential, &["name", "label"]).unwrap_or_default();
                    let priority = credential
                        .get("priority")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let weight = credential
                        .get("weight")
                        .and_then(Value::as_i64)
                        .unwrap_or(1)
                        .max(1);
                    let enabled = credential
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    tx.execute(
                        "INSERT INTO credentials
                         (id, provider_id, name, api_key, priority, weight, enabled, limits_json, position)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            credential_id,
                            id,
                            credential_name,
                            api_key,
                            priority,
                            weight,
                            enabled as i64,
                            credential
                                .get("limits")
                                .map(serde_json::to_string)
                                .transpose()?,
                            position as i64
                        ],
                    )?;
                }
            }
        }

        if let Some(keys) = config.get("APIKEYS").and_then(Value::as_array) {
            for key in keys {
                let Some(key) = key.as_object() else { continue };
                let Some(value) = str_field(key, &["key", "api_key", "apiKey"]) else {
                    continue;
                };
                tx.execute(
                    "INSERT INTO caller_keys (id, name, key, limits_json, enabled)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        str_field(key, &["id"]).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        str_field(key, &["name", "label"]).unwrap_or_default(),
                        value,
                        key.get("limits").map(serde_json::to_string).transpose()?,
                        key.get("enabled").and_then(Value::as_bool).unwrap_or(true) as i64
                    ],
                )?;
            }
        }

        tx.execute(
            "INSERT OR REPLACE INTO legacy_config (key, value_json) VALUES ('raw', ?1)",
            params![serde_json::to_string(config)?],
        )?;
        for key in [
            "profile",
            "Router",
            "preferredProvider",
            "gateway",
            "APIKEY",
        ] {
            if let Some(value) = config.get(key) {
                tx.execute(
                    "INSERT OR REPLACE INTO legacy_config (key, value_json) VALUES (?1, ?2)",
                    params![key, serde_json::to_string(value)?],
                )?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    // ---------------------------------------------------------------- read

    pub fn providers(&self) -> Result<Vec<ProviderRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, base_url, raw_json, enabled, position
             FROM providers ORDER BY position, name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ProviderRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    base_url: row.get(3)?,
                    raw_json: row.get(4)?,
                    enabled: row.get::<_, i64>(5)? != 0,
                    position: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Projection used by the model resolver: only enabled, routable models.
    pub fn provider_views(&self) -> Result<Vec<ProviderView>> {
        let providers = self.providers()?;
        let conn = self.conn.lock().unwrap();
        let mut views = Vec::with_capacity(providers.len());
        for provider in providers.into_iter().filter(|p| p.enabled) {
            let mut stmt = conn.prepare(
                "SELECT model, one_million FROM provider_models
                 WHERE provider_id = ?1 AND enabled = 1 AND routable = 1
                 ORDER BY position",
            )?;
            let rows = stmt
                .query_map(params![provider.id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let models = rows.iter().map(|(m, _)| m.clone()).collect();
            let one_million = rows
                .iter()
                .filter(|(_, flag)| *flag)
                .map(|(m, _)| m.clone())
                .collect();
            views.push(ProviderView {
                id: provider.id,
                name: provider.name,
                base_url: provider.base_url,
                kind: provider.kind,
                models,
                one_million,
            });
        }
        Ok(views)
    }

    /// Mark the models CCR advertises to Claude with the `[1m]` suffix. This is
    /// the only reliable source for the flag: the provider JSON does not carry
    /// it, and inventing it from a name would break Claude's catalog check.
    pub fn mark_one_million(&self, claude_ids: &[String]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        for id in claude_ids {
            let Some((provider_name, model, true)) = crate::model_id::decode_virtual(id) else {
                continue;
            };
            conn.execute(
                "UPDATE provider_models SET one_million = 1
                 WHERE model = ?1
                   AND provider_id IN (SELECT id FROM providers WHERE name = ?2 COLLATE NOCASE)",
                params![model, provider_name],
            )?;
        }
        Ok(())
    }

    pub fn credentials_for(&self, provider_id: &str) -> Result<Vec<CredentialRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, provider_id, name, api_key, priority, weight
             FROM credentials
             WHERE provider_id = ?1 AND enabled = 1
             ORDER BY priority, position",
        )?;
        let rows = stmt
            .query_map(params![provider_id], |row| {
                Ok(CredentialRow {
                    id: row.get(0)?,
                    provider_id: row.get(1)?,
                    name: row.get(2)?,
                    api_key: row.get(3)?,
                    priority: row.get(4)?,
                    weight: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Weighted choice inside the best-priority group. No mutable "current
    /// credential" state: two concurrent requests may pick differently.
    pub fn pick_credential(&self, provider_id: &str) -> Result<Option<CredentialRow>> {
        let rows = self.credentials_for(provider_id)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let best = rows.iter().map(|r| r.priority).min().unwrap_or(0);
        let candidates: Vec<&CredentialRow> = rows.iter().filter(|r| r.priority == best).collect();
        let total: i64 = candidates.iter().map(|r| r.weight.max(1)).sum();
        if total <= 1 {
            return Ok(candidates.first().map(|r| (*r).clone()));
        }
        use rand::Rng;
        let mut pick = rand::thread_rng().gen_range(0..total);
        for candidate in &candidates {
            pick -= candidate.weight.max(1);
            if pick < 0 {
                return Ok(Some((*candidate).clone()));
            }
        }
        Ok(candidates.first().map(|r| (*r).clone()))
    }

    pub fn caller_key_valid(&self, key: &str) -> Result<bool> {
        if key.trim().is_empty() {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let hit: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM caller_keys WHERE key = ?1 AND enabled = 1 LIMIT 1",
                params![key.trim()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    pub fn has_caller_keys(&self) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM caller_keys WHERE enabled = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// A provider configured with the legacy single `api_key` instead of a
    /// credential pool. Kept for imported configs and for the first run before
    /// the UI creates a pool.
    pub fn fallback_key(&self, provider_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn
            .query_row(
                "SELECT raw_json FROM providers WHERE id = ?1",
                params![provider_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        Ok(["api_key", "apiKey", "apikey"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string))
    }

    pub fn api_keys(&self) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, key, limits_json, enabled FROM caller_keys ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let limits: Option<String> = row.get(3)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "key": row.get::<_, String>(2)?,
                    "limits": limits.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
                    "enabled": row.get::<_, i64>(4)? != 0,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn quota_snapshots(&self, provider: Option<&str>) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT q.credential_id, q.meters_json, q.fetched_at, c.name, p.name
             FROM quota_snapshots q
             JOIN credentials c ON c.id = q.credential_id
             JOIN providers p ON p.id = c.provider_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::new();
        for (credential_id, meters, fetched_at, credential_name, provider_name) in rows {
            if provider
                .map(|wanted| !wanted.eq_ignore_ascii_case(&provider_name))
                .unwrap_or(false)
            {
                continue;
            }
            out.push(json!({
                "provider": provider_name,
                "credentialId": credential_id,
                "credentialLabel": credential_name,
                "status": "ok",
                "source": "cache",
                "meters": serde_json::from_str::<Value>(&meters).unwrap_or(Value::Null),
                "updatedAt": fetched_at,
            }));
        }
        Ok(out)
    }

    // ---------------------------------------------------------------- config

    pub fn current_revision(&self) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'config_revision'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.unwrap_or_else(|| "rev-0".to_string()))
    }

    pub fn bump_revision(&self) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('config_revision', 'rev-1')
             ON CONFLICT(key) DO UPDATE SET value = 'rev-' ||
                (CAST(SUBSTR(value, 5) AS INTEGER) + 1)",
            [],
        )?;
        let value: String = conn.query_row(
            "SELECT value FROM meta WHERE key = 'config_revision'",
            [],
            |row| row.get(0),
        )?;
        Ok(value)
    }

    /// Build the CCR-shaped config codeg consumes.
    pub fn build_config(&self) -> Result<Value> {
        let mut config = self.legacy_raw().unwrap_or_else(|| json!({}));
        if !config.is_object() {
            config = json!({});
        }
        let object = config.as_object_mut().unwrap();

        let mut providers = Vec::new();
        for provider in self.providers()? {
            let mut raw: Map<String, Value> =
                serde_json::from_str(&provider.raw_json).unwrap_or_default();
            raw.insert("id".into(), Value::String(provider.id.clone()));
            raw.insert("name".into(), Value::String(provider.name.clone()));
            raw.insert("type".into(), Value::String(provider.kind.clone()));
            raw.insert(
                "api_base_url".into(),
                Value::String(provider.base_url.clone()),
            );
            raw.insert("enabled".into(), Value::Bool(provider.enabled));

            let (models, auto, disabled, credentials) = self.provider_children(&provider.id)?;
            raw.insert(
                "models".into(),
                Value::Array(models.into_iter().map(Value::String).collect()),
            );
            raw.insert(
                "autoFetchKnownModels".into(),
                Value::Array(auto.into_iter().map(Value::String).collect()),
            );
            raw.insert(
                "disabledModels".into(),
                Value::Array(disabled.into_iter().map(Value::String).collect()),
            );
            raw.insert("credentials".into(), Value::Array(credentials));
            providers.push(Value::Object(raw));
        }

        object.insert("Providers".into(), Value::Array(providers));
        object.insert("APIKEYS".into(), Value::Array(self.api_keys()?));
        object.insert(
            "routerEndpoint".into(),
            Value::String("http://127.0.0.1:3456".into()),
        );
        object.insert(
            "gateway".into(),
            json!({"enabled": true, "host": "127.0.0.1", "port": 3456}),
        );
        object.insert(
            "configRevision".into(),
            Value::String(self.current_revision()?),
        );
        Ok(config)
    }

    fn legacy_raw(&self) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value_json FROM legacy_config WHERE key = 'raw'",
                [],
                |row| row.get(0),
            )
            .optional()
            .ok()?;
        serde_json::from_str(&raw?).ok()
    }

    #[allow(clippy::type_complexity)]
    fn provider_children(
        &self,
        provider_id: &str,
    ) -> Result<(Vec<String>, Vec<String>, Vec<String>, Vec<Value>)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT model, routable, auto_fetch, enabled, display_name, one_million
             FROM provider_models WHERE provider_id = ?1 ORDER BY position",
        )?;
        let rows = stmt
            .query_map(params![provider_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut models = Vec::new();
        let mut auto = Vec::new();
        let mut disabled = Vec::new();
        for (model, routable, auto_fetch, enabled, _, one_million) in rows {
            if auto_fetch {
                auto.push(model.clone());
            }
            if routable {
                if enabled {
                    models.push(if one_million {
                        format!("{model}[1m]")
                    } else {
                        model
                    });
                } else {
                    disabled.push(model);
                }
            }
        }

        let mut stmt = conn.prepare(
            "SELECT id, name, api_key, priority, weight, enabled, limits_json
             FROM credentials WHERE provider_id = ?1 ORDER BY position",
        )?;
        let credentials = stmt
            .query_map(params![provider_id], |row| {
                let limits: Option<String> = row.get(6)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "api_key": row.get::<_, String>(2)?,
                    "priority": row.get::<_, i64>(3)?,
                    "weight": row.get::<_, i64>(4)?,
                    "enabled": row.get::<_, i64>(5)? != 0,
                    "limits": limits.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok((models, auto, disabled, credentials))
    }
}

fn str_field(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = object.get(*key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn strip_one_million(value: &str) -> (String, bool) {
    match value.trim().strip_suffix("[1m]") {
        Some(rest) => (rest.trim().to_string(), true),
        None => (value.trim().to_string(), false),
    }
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Value {
        json!({
            "configRevision": "rev-1",
            "APIKEYS": [
                {"id": "local-gateway", "name": "Local Gateway", "key": "sk-local"},
                {"id": "profile:codex", "name": "Profile: Codex", "key": "sk-codex"}
            ],
            "Providers": [
                {
                    "id": "opencode-go",
                    "name": "OpenCode Go",
                    "type": "openai_responses",
                    "api_base_url": "https://opencode.ai/zen/go/v1",
                    "models": ["mimo-v2.6-flash", "deepseek-v4.1-flash"],
                    "autoFetchKnownModels": ["mimo-v2.6-flash", "extra-model"],
                    "modelDisplayNames": {"mimo-v2.6-flash": "MiMo"},
                    "credentials": [
                        {"id": "c1", "name": "main", "api_key": "sk-a", "priority": 0, "weight": 1},
                        {"id": "c2", "name": "backup", "api_key": "sk-b", "priority": 1, "weight": 1}
                    ],
                    "account": {"connectors": []},
                    "capabilities": [{"type": "openai_chat_completions"}]
                }
            ]
        })
    }

    #[test]
    fn round_trips_unknown_provider_fields() {
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&config()).unwrap();
        let built = store.build_config().unwrap();
        let provider = &built["Providers"][0];
        assert_eq!(provider["name"], "OpenCode Go");
        assert_eq!(provider["account"]["connectors"], json!([]));
        assert_eq!(
            provider["capabilities"][0]["type"],
            "openai_chat_completions"
        );
        assert_eq!(provider["modelDisplayNames"]["mimo-v2.6-flash"], "MiMo");
        assert_eq!(
            provider["models"],
            json!(["mimo-v2.6-flash", "deepseek-v4.1-flash"])
        );
        assert_eq!(built["APIKEYS"][0]["key"], "sk-local");
    }

    #[test]
    fn projects_only_routable_models() {
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&config()).unwrap();
        let views = store.provider_views().unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].models.len(), 2);
        assert!(views[0].models.contains(&"mimo-v2.6-flash".to_string()));
        assert!(!views[0].models.contains(&"extra-model".to_string()));
    }

    #[test]
    fn disabled_models_leave_the_routable_list_and_are_reported() {
        let mut value = config();
        value["Providers"][0]["disabledModels"] = json!(["deepseek-v4.1-flash"]);
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&value).unwrap();
        let built = store.build_config().unwrap();
        assert_eq!(built["Providers"][0]["models"], json!(["mimo-v2.6-flash"]));
        assert_eq!(
            built["Providers"][0]["disabledModels"],
            json!(["deepseek-v4.1-flash"])
        );
    }

    #[test]
    fn picks_credentials_without_global_state() {
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&config()).unwrap();
        let picked = store.pick_credential("opencode-go").unwrap().unwrap();
        assert_eq!(picked.id, "c1");
    }

    #[test]
    fn caller_keys_are_checked_exactly() {
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&config()).unwrap();
        assert!(store.caller_key_valid("sk-local").unwrap());
        assert!(!store.caller_key_valid("sk-other").unwrap());
    }
}
