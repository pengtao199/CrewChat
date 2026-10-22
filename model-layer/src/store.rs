//! SQLite store for providers, credentials, models and caller keys.
//!
//! Raw provider JSON is kept verbatim (`providers.raw_json`) so fields the UI
//! does not know about (`account`, `capabilities`, `modelDisplayNames`, …)
//! survive a round trip. Normalized columns exist only for routing and the
//! model list.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::keystore::{self, KeyStore};
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
    keystore: Arc<dyn KeyStore>,
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
        Ok(Self::with_keystore(conn, keystore::from_env()))
    }

    fn with_keystore(conn: Connection, keystore: Arc<dyn KeyStore>) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            keystore,
        }
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self::with_keystore(conn, Arc::new(keystore::InlineStore));
        store.init_schema()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn open_in_memory_with(keystore: Arc<dyn KeyStore>) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self::with_keystore(conn, keystore);
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
                api_key TEXT NOT NULL DEFAULT '',
                keychain_ref TEXT,
                priority INTEGER NOT NULL DEFAULT 0,
                weight INTEGER NOT NULL DEFAULT 1,
                enabled INTEGER NOT NULL DEFAULT 1,
                limits_json TEXT,
                position INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS caller_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL DEFAULT '',
                keychain_ref TEXT,
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
        ensure_column(&conn, "credentials", "keychain_ref", "TEXT")?;
        ensure_column(&conn, "caller_keys", "keychain_ref", "TEXT")?;
        Ok(())
    }

    // ---------------------------------------------------------------- import

    /// Replace the catalog with one CCR config. Full replace keeps the store
    /// and CCR from disagreeing; `legacy_config.raw` preserves unknown fields.
    pub fn apply_config(&self, config: &Value) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing_credentials = snapshot_secrets(&tx, "credentials", "api_key")?;
        let existing_callers = snapshot_secrets(&tx, "caller_keys", "key")?;
        let existing_one_million = snapshot_one_million(&tx)?;

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
            let mut object = provider.as_object().cloned().unwrap_or_default();
            let name = str_field(&object, &["name"]).unwrap_or_else(|| "Provider".to_string());
            let id = str_field(&object, &["id"]).unwrap_or_else(|| slug(&name));
            let kind = str_field(&object, &["type"])
                .unwrap_or_else(|| "openai_chat_completions".to_string());
            let base_url = str_field(&object, &["api_base_url", "baseUrl"]).unwrap_or_default();
            let enabled = object
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);

            // Provider-level legacy keys must not stay in raw_json. If there
            // is no credential pool, turn the key into the pool's first entry.
            let legacy_key = take_secret(&mut object);
            let has_credentials = object
                .get("credentials")
                .and_then(Value::as_array)
                .map(|list| !list.is_empty())
                .unwrap_or(false);
            if let Some(key) = &legacy_key {
                if !has_credentials {
                    object.insert(
                        "credentials".into(),
                        json!([{
                            "id": format!("{id}:legacy"),
                            "name": "default",
                            "api_key": key,
                            "enabled": true,
                        }]),
                    );
                }
            }

            tx.execute(
                "INSERT INTO providers (id, name, kind, base_url, raw_json, enabled, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    name,
                    kind,
                    base_url,
                    serde_json::to_string(&Value::Object(object.clone()))?,
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
                    let incoming =
                        str_field(credential, &["api_key", "apiKey", "apikey"]).unwrap_or_default();
                    let incoming = if looks_masked(&incoming) {
                        String::new()
                    } else {
                        incoming
                    };
                    let account = format!("provider:{id}:{credential_id}");
                    let (existing_inline, existing_ref) = existing_credentials
                        .get(&credential_id)
                        .cloned()
                        .unwrap_or_default();
                    let (api_key, keychain_ref) = if !incoming.is_empty() {
                        self.protect(&account, &incoming)
                    } else if existing_ref.is_some() {
                        (String::new(), existing_ref)
                    } else if !existing_inline.is_empty() {
                        self.protect(&account, &existing_inline)
                    } else {
                        (String::new(), None)
                    };
                    tx.execute(
                        "INSERT INTO credentials
                         (id, provider_id, name, api_key, keychain_ref, priority, weight, enabled, limits_json, position)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            credential_id,
                            id,
                            credential_name,
                            api_key,
                            keychain_ref,
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
                let id =
                    str_field(key, &["id"]).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let incoming = str_field(key, &["key", "api_key", "apiKey"]).unwrap_or_default();
                let incoming = if looks_masked(&incoming) {
                    String::new()
                } else {
                    incoming
                };
                let account = format!("caller:{id}");
                let (existing_inline, existing_ref) =
                    existing_callers.get(&id).cloned().unwrap_or_default();
                let (value, keychain_ref) = if !incoming.is_empty() {
                    self.protect(&account, &incoming)
                } else if existing_ref.is_some() {
                    (String::new(), existing_ref)
                } else if !existing_inline.is_empty() {
                    self.protect(&account, &existing_inline)
                } else {
                    (String::new(), None)
                };
                tx.execute(
                    "INSERT INTO caller_keys (id, name, key, keychain_ref, limits_json, enabled)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        id,
                        str_field(key, &["name", "label"]).unwrap_or_default(),
                        value,
                        keychain_ref,
                        key.get("limits").map(serde_json::to_string).transpose()?,
                        key.get("enabled").and_then(Value::as_bool).unwrap_or(true) as i64
                    ],
                )?;
            }
        }

        // The provider JSON has no `[1m]` flag; a re-import (e.g. from SQLite
        // while CCR is offline) must not forget it.
        for (provider_id, model) in existing_one_million {
            tx.execute(
                "UPDATE provider_models SET one_million = 1
                 WHERE provider_id = ?1 AND model = ?2",
                params![provider_id, model],
            )?;
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

    /// Move existing plaintext rows into the KeyStore. New writes already go
    /// through `protect`; this catches databases created before the switch.
    pub fn migrate_keys(&self) -> Result<()> {
        if !self.keystore.is_persistent() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut migrated = 0usize;

        let credentials: Vec<(String, String, String, Option<String>)> = {
            let mut stmt =
                tx.prepare("SELECT id, provider_id, api_key, keychain_ref FROM credentials")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, provider_id, inline, reference) in credentials {
            if reference.is_some() || inline.is_empty() {
                continue;
            }
            let account = format!("provider:{provider_id}:{id}");
            match self.keystore.put(&account, &inline) {
                Ok(()) => {
                    tx.execute(
                        "UPDATE credentials SET api_key = '', keychain_ref = ?1 WHERE id = ?2",
                        params![account, id],
                    )?;
                    migrated += 1;
                }
                Err(error) => tracing::warn!("keychain migration failed for {account}: {error}"),
            }
        }

        let callers: Vec<(String, String, Option<String>)> = {
            let mut stmt = tx.prepare("SELECT id, key, keychain_ref FROM caller_keys")?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, inline, reference) in callers {
            if reference.is_some() || inline.is_empty() {
                continue;
            }
            let account = format!("caller:{id}");
            match self.keystore.put(&account, &inline) {
                Ok(()) => {
                    tx.execute(
                        "UPDATE caller_keys SET key = '', keychain_ref = ?1 WHERE id = ?2",
                        params![account, id],
                    )?;
                    migrated += 1;
                }
                Err(error) => tracing::warn!("keychain migration failed for {account}: {error}"),
            }
        }

        let providers: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT id, raw_json FROM providers")?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, raw) in providers {
            let mut value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let Some(object) = value.as_object_mut() else {
                continue;
            };
            let Some(key) = take_secret(object) else {
                continue;
            };
            let has_credentials = object
                .get("credentials")
                .and_then(Value::as_array)
                .map(|list| !list.is_empty())
                .unwrap_or(false);
            if !has_credentials {
                let credential_id = format!("{id}:legacy");
                let account = format!("provider:{id}:{credential_id}");
                if let Err(error) = self.keystore.put(&account, &key) {
                    tracing::warn!("keychain migration failed for {account}: {error}");
                    continue;
                }
                tx.execute(
                    "INSERT OR IGNORE INTO credentials
                     (id, provider_id, name, api_key, keychain_ref, priority, weight, enabled, position)
                     VALUES (?1, ?2, 'default', '', ?3, 0, 1, 1, 0)",
                    params![credential_id, id, account],
                )?;
            }
            tx.execute(
                "UPDATE providers SET raw_json = ?1 WHERE id = ?2",
                params![serde_json::to_string(&value)?, id],
            )?;
            migrated += 1;
        }

        tx.commit()?;
        if migrated > 0 {
            tracing::info!("migrated {migrated} secrets into the keystore");
        }
        Ok(())
    }

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
    /// The catalog projection: only routable models. This is what
    /// `/v1/models` advertises, so Claude's 38-id list stays the same.
    pub fn provider_views(&self) -> Result<Vec<ProviderView>> {
        self.provider_views_with(false)
    }

    /// The routing projection: routable + auto-fetch. Codex/OpenCode are
    /// offered both arrays by `body_model_face`, so the gateway must resolve a
    /// model the user picked from the auto-fetch catalog.
    pub fn routing_views(&self) -> Result<Vec<ProviderView>> {
        self.provider_views_with(true)
    }

    fn provider_views_with(&self, include_auto: bool) -> Result<Vec<ProviderView>> {
        let providers = self.providers()?;
        let conn = self.conn.lock().unwrap();
        let mut views = Vec::with_capacity(providers.len());
        let condition = if include_auto {
            "(routable = 1 OR auto_fetch = 1)"
        } else {
            "routable = 1"
        };
        for provider in providers.into_iter().filter(|p| p.enabled) {
            let mut stmt = conn.prepare(&format!(
                "SELECT model, one_million FROM provider_models
                 WHERE provider_id = ?1 AND enabled = 1 AND {condition}
                 ORDER BY position",
            ))?;
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
            "SELECT id, provider_id, name, api_key, keychain_ref, priority, weight
             FROM credentials
             WHERE provider_id = ?1 AND enabled = 1
             ORDER BY priority, position",
        )?;
        let keystore = self.keystore.clone();
        let rows = stmt
            .query_map(params![provider_id], |row| {
                let inline: String = row.get(3)?;
                let reference: Option<String> = row.get(4)?;
                Ok(CredentialRow {
                    id: row.get(0)?,
                    provider_id: row.get(1)?,
                    name: row.get(2)?,
                    api_key: resolve_secret_with(keystore.as_ref(), &inline, reference.as_deref()),
                    priority: row.get(5)?,
                    weight: row.get(6)?,
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
        let key = key.trim();
        if key.is_empty() {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT key, keychain_ref FROM caller_keys WHERE enabled = 1")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let keystore = self.keystore.clone();
        for (inline, reference) in rows {
            if resolve_secret_with(keystore.as_ref(), &inline, reference.as_deref()) == key {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Move one secret behind the KeyStore. On failure keep it inline instead
    /// of failing the whole import/save.
    fn protect(&self, account: &str, secret: &str) -> (String, Option<String>) {
        if secret.is_empty() {
            return (String::new(), None);
        }
        if !self.keystore.is_persistent() {
            return (secret.to_string(), None);
        }
        match self.keystore.put(account, secret) {
            Ok(()) => (String::new(), Some(account.to_string())),
            Err(error) => {
                tracing::warn!("keychain put failed for {account}: {error}; keeping key in DB");
                (secret.to_string(), None)
            }
        }
    }

    fn resolve_secret(&self, inline: &str, reference: Option<&str>) -> String {
        resolve_secret_with(self.keystore.as_ref(), inline, reference)
    }

    pub fn local_gateway_key(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT key, keychain_ref FROM caller_keys
                 WHERE id = 'local-gateway' AND enabled = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(inline, reference)| {
            let key = self.resolve_secret(&inline, reference.as_deref());
            (!key.is_empty()).then_some(key)
        }))
    }

    pub fn uses_keychain(&self) -> bool {
        self.keystore.is_persistent()
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
            "SELECT id, name, key, keychain_ref, limits_json, enabled
             FROM caller_keys ORDER BY rowid",
        )?;
        let keystore = self.keystore.clone();
        let rows = stmt
            .query_map([], |row| {
                let inline: String = row.get(2)?;
                let reference: Option<String> = row.get(3)?;
                let key = resolve_secret_with(keystore.as_ref(), &inline, reference.as_deref());
                let limits: Option<String> = row.get(4)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "key": key,
                    "limits": limits.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
                    "enabled": row.get::<_, i64>(5)? != 0,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn quota_snapshot(&self, credential_id: &str) -> Result<Option<(Value, String)>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT meters_json, fetched_at FROM quota_snapshots WHERE credential_id = ?1",
                params![credential_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(meters, fetched_at)| {
            serde_json::from_str::<Value>(&meters)
                .ok()
                .map(|value| (value, fetched_at))
        }))
    }

    pub fn save_quota_snapshot(&self, credential_id: &str, meters: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO quota_snapshots (credential_id, meters_json, fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(credential_id) DO UPDATE SET
                meters_json = excluded.meters_json,
                fetched_at = excluded.fetched_at",
            params![
                credential_id,
                serde_json::to_string(meters)?,
                crate::config::now()
            ],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
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
            "SELECT id, name, api_key, keychain_ref, priority, weight, enabled, limits_json
             FROM credentials WHERE provider_id = ?1 ORDER BY position",
        )?;
        let keystore = self.keystore.clone();
        let credentials = stmt
            .query_map(params![provider_id], |row| {
                let inline: String = row.get(2)?;
                let reference: Option<String> = row.get(3)?;
                let api_key = resolve_secret_with(keystore.as_ref(), &inline, reference.as_deref());
                let limits: Option<String> = row.get(7)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "api_key": api_key,
                    "priority": row.get::<_, i64>(4)?,
                    "weight": row.get::<_, i64>(5)?,
                    "enabled": row.get::<_, i64>(6)? != 0,
                    "limits": limits.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok((models, auto, disabled, credentials))
    }
}

fn resolve_secret_with(keystore: &dyn KeyStore, inline: &str, reference: Option<&str>) -> String {
    if let Some(reference) = reference.filter(|value| !value.is_empty()) {
        match keystore.get(reference) {
            Ok(Some(secret)) => return secret,
            Ok(None) => tracing::warn!("keychain item {reference} is missing"),
            Err(error) => tracing::warn!("keychain get failed for {reference}: {error}"),
        }
    }
    inline.to_string()
}

/// Remove a provider-level legacy key from the raw JSON. The key moves behind
/// the KeyStore; `raw_json` must never carry it again.
fn take_secret(object: &mut Map<String, Value>) -> Option<String> {
    let mut found = None;
    for key in ["api_key", "apiKey", "apikey"] {
        if let Some(Value::String(value)) = object.remove(key) {
            let value = value.trim().to_string();
            if !value.is_empty() && !looks_masked(&value) {
                found = Some(value);
            }
        }
    }
    found
}

/// A masked key from the UI must not overwrite the stored one.
fn looks_masked(key: &str) -> bool {
    key.contains('\u{2022}') || key.contains("****")
}

fn snapshot_one_million(tx: &rusqlite::Transaction<'_>) -> Result<HashSet<(String, String)>> {
    let mut stmt =
        tx.prepare("SELECT provider_id, model FROM provider_models WHERE one_million = 1")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

fn snapshot_secrets(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
) -> Result<HashMap<String, (String, Option<String>)>> {
    let mut stmt = tx.prepare(&format!("SELECT id, {column}, keychain_ref FROM {table}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, inline, reference)| (id, (inline, reference)))
        .collect())
}

fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !names.iter().any(|name| name == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
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
    fn routing_views_include_auto_fetch_models() {
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&config()).unwrap();
        assert_eq!(store.provider_views().unwrap()[0].models.len(), 2);
        let routing = store.routing_views().unwrap();
        assert!(routing[0].models.contains(&"extra-model".to_string()));
    }

    #[test]
    fn secrets_go_to_the_keystore() {
        let keystore = Arc::new(crate::keystore::MemoryKeyStore::default());
        let store = Store::open_in_memory_with(keystore.clone()).unwrap();
        store.apply_config(&config()).unwrap();
        let creds = store.credentials_for("opencode-go").unwrap();
        assert_eq!(creds[0].api_key, "sk-a");
        {
            let conn = store.conn.lock().unwrap();
            let inline: String = conn
                .query_row(
                    "SELECT api_key FROM credentials WHERE id = 'c1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(inline.is_empty());
        }
        assert!(store.caller_key_valid("sk-local").unwrap());
    }

    #[test]
    fn migrate_keys_moves_plaintext_out_of_sqlite() {
        let keystore = Arc::new(crate::keystore::MemoryKeyStore::default());
        let store = Store::open_in_memory_with(keystore).unwrap();
        store.apply_config(&config()).unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE credentials SET api_key = 'sk-legacy', keychain_ref = NULL WHERE id = 'c1'",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE caller_keys SET key = 'sk-caller', keychain_ref = NULL
                 WHERE id = 'local-gateway'",
                [],
            )
            .unwrap();
        }
        store.migrate_keys().unwrap();
        let creds = store.credentials_for("opencode-go").unwrap();
        assert_eq!(creds[0].api_key, "sk-legacy");
        assert!(store.caller_key_valid("sk-caller").unwrap());
        let conn = store.conn.lock().unwrap();
        let inline: String = conn
            .query_row(
                "SELECT api_key FROM credentials WHERE id = 'c1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(inline.is_empty());
    }

    #[test]
    fn reimport_keeps_one_million_flags() {
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&config()).unwrap();
        let id = format!(
            "anthropic/claude-ccr-h{}[1m]",
            crate::model_id::virtual_hex("OpenCode Go", "mimo-v2.6-flash")
        );
        store.mark_one_million(&[id]).unwrap();
        store.apply_config(&config()).unwrap();
        let views = store.provider_views().unwrap();
        assert!(views[0]
            .one_million
            .contains(&"mimo-v2.6-flash".to_string()));
    }

    #[test]
    fn caller_keys_are_checked_exactly() {
        let store = Store::open_in_memory().unwrap();
        store.apply_config(&config()).unwrap();
        assert!(store.caller_key_valid("sk-local").unwrap());
        assert!(!store.caller_key_valid("sk-other").unwrap());
    }
}
