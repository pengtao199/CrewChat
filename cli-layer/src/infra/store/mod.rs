//! SQLite store for projects, conversations, turns, roster and event replay.
//!
//! The schema is intentionally small: it is the `cli-layer` replacement for
//! the old core's much larger `db/` (folders + conversations + roster + usage).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

pub use crate::domain::engine::EnginePreferences;

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
                kind TEXT NOT NULL DEFAULT 'single',
                leader_role_id TEXT,
                team_json TEXT,
                external_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE TABLE IF NOT EXISTS turns (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                role TEXT NOT NULL,
                speaker_role_id TEXT,
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
                permission_mode TEXT NOT NULL DEFAULT '',
                auto_approve INTEGER NOT NULL DEFAULT 0,
                version INTEGER NOT NULL DEFAULT 1,
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
            CREATE TABLE IF NOT EXISTS conversation_agents (
                conversation_id TEXT NOT NULL,
                role_id TEXT NOT NULL,
                seat TEXT NOT NULL,
                state TEXT NOT NULL,
                connection_id TEXT,
                joined_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (conversation_id, role_id)
            );
            CREATE TABLE IF NOT EXISTS delegations (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                leader_role_id TEXT NOT NULL,
                member_role_id TEXT NOT NULL,
                child_conversation_id TEXT,
                task TEXT NOT NULL,
                context TEXT,
                expected_output TEXT,
                status TEXT NOT NULL,
                result_json TEXT,
                usage_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_delegations_conversation
                ON delegations(conversation_id, created_at);
            "#,
        )?;
        ensure_column(
            &conn,
            "conversations",
            "kind",
            "TEXT NOT NULL DEFAULT 'single'",
        )?;
        ensure_column(&conn, "conversations", "leader_role_id", "TEXT")?;
        ensure_column(&conn, "conversations", "team_json", "TEXT")?;
        ensure_column(&conn, "turns", "speaker_role_id", "TEXT")?;
        ensure_column(
            &conn,
            "agent_roles",
            "version",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        // Canonical engine-default mode is the empty string.
        conn.execute(
            "UPDATE agent_roles SET permission_mode = '' WHERE permission_mode = 'default'",
            [],
        )?;
        Ok(())
    }
}

fn ensure_column(conn: &Connection, table: &str, column: &str, declaration: &str) -> Result<()> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !names.iter().any(|name| name == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )?;
    }
    Ok(())
}

mod agents;
mod conversations;
mod engine_settings;
mod events;
mod folders;
mod repositories;
#[allow(dead_code)]
mod team;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::agent::Agent;
    use crate::domain::team::{Delegation, TeamSnapshot};
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
    fn team_conversation_freezes_snapshot_and_tracks_members() {
        let (_dir, store) = store();
        let leader = Agent {
            id: "leader".to_string(),
            name: "Manager".to_string(),
            agent_type: "claude_code".to_string(),
            version: 3,
            ..Agent::default()
        };
        let member = Agent {
            id: "member".to_string(),
            name: "Builder".to_string(),
            agent_type: "codex".to_string(),
            version: 2,
            ..Agent::default()
        };
        let snapshot =
            TeamSnapshot::new(&leader, std::slice::from_ref(&member), crate::config::now());
        let id = store
            .create_team_conversation(None, "claude_code", Some("Team"), &snapshot)
            .unwrap();

        let detail = store.get_conversation(&id).unwrap().unwrap();
        assert_eq!(detail["summary"]["kind"], "team");
        assert_eq!(detail["summary"]["team"]["leader_role_id"], "leader");
        assert_eq!(detail["summary"]["members"][0]["role_id"], "member");
        assert_eq!(detail["summary"]["members"][0]["state"], "idle");
        assert_eq!(detail["summary"]["team"]["members"][0]["version"], 2);

        store
            .append_turn_with_speaker(
                &id,
                "assistant",
                Some("member"),
                &json!([{"type": "text", "text": "done"}]),
            )
            .unwrap();
        let detail = store.get_conversation(&id).unwrap().unwrap();
        assert_eq!(detail["turns"][0]["speaker_role_id"], "member");

        let delegation = Delegation::new(
            &id,
            "leader",
            "member",
            "build it",
            None,
            None,
            crate::config::now(),
        );
        store.create_delegation(&delegation).unwrap();
        store
            .update_delegation(
                &delegation.id,
                "done",
                Some(&json!({"summary": "ok"})),
                Some(&json!({"input": 1})),
                None,
            )
            .unwrap();
        let saved = store.delegation(&delegation.id).unwrap().unwrap();
        assert_eq!(saved["status"], "done");
        assert_eq!(saved["result"]["summary"], "ok");
        assert_eq!(store.delegations(&id).unwrap().len(), 1);
    }

    #[test]
    fn saving_a_role_bumps_its_version() {
        let (_dir, store) = store();
        let first = store
            .save_role(&json!({"name": "A", "agent_type": "claude_code"}))
            .unwrap();
        assert_eq!(first["version"], 1);
        let mut second = first.clone();
        second["name"] = json!("B");
        let saved = store.save_role(&second).unwrap();
        assert_eq!(saved["version"], 2);
    }

    #[test]
    fn events_are_replayable_by_connection_and_conversation() {
        let (_dir, store) = store();
        store
            .append_event(
                "c1",
                Some("conv"),
                &json!({"type": "content_delta", "seq": 1}),
            )
            .unwrap();
        store
            .append_event(
                "c1",
                Some("conv"),
                &json!({"type": "turn_complete", "seq": 2}),
            )
            .unwrap();
        let rows = store.events_since("c1", 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["type"], "turn_complete");
        assert_eq!(store.events_for_conversation("conv", 0).unwrap().len(), 2);
        assert!(store
            .events_for_conversation("other", 0)
            .unwrap()
            .is_empty());
    }
}
