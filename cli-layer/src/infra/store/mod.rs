//! SQLite store for projects, conversations, turns, roster and event replay.
//!
//! The schema is intentionally small: it is the `cli-layer` replacement for
//! codeg's much larger `db/` (folders + conversations + roster + usage).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

pub use crate::domain::engine::EnginePreferences;

#[derive(Debug, Default, Clone, Copy)]
pub struct ImportReport {
    pub folders: usize,
    pub conversations: usize,
    pub roles: usize,
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
            CREATE TABLE IF NOT EXISTS engine_settings (
                agent_type TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 1,
                env_json TEXT NOT NULL DEFAULT '{}',
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
}

mod agents;
mod conversations;
mod engine_settings;
mod events;
mod folders;
mod import;
mod repositories;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use serde_json::json;

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
    fn engine_preferences_round_trip() {
        let (_dir, store) = store();
        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        store.save_engine_preferences("codex", false, &env).unwrap();
        let prefs = store.engine_preferences("codex").unwrap();
        assert!(!prefs.enabled);
        assert_eq!(prefs.env.get("FOO").map(String::as_str), Some("bar"));
        let defaults = store.engine_preferences("claude_code").unwrap();
        assert!(defaults.enabled);
        assert!(defaults.env.is_empty());
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
