use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub current_hash: String,
    pub license: Option<String>,
    pub source_kind: String,
    pub source_url: Option<String>,
    pub source_ref: Option<String>,
    pub source_subdir: Option<String>,
    pub has_scripts: bool,
    pub size_bytes: u64,
    pub installed_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub description: String,
    pub skill_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalLink {
    pub skill_id: String,
    pub agent_type: String,
    pub path: String,
    pub state: String,
    pub linked_at: String,
}

#[derive(Debug, Clone)]
pub struct SkillRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub current_hash: String,
    pub license: Option<String>,
    pub source_kind: String,
    pub source_url: Option<String>,
    pub source_ref: Option<String>,
    pub source_subdir: Option<String>,
    pub has_scripts: bool,
    pub size_bytes: u64,
    pub installed_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct VersionRow {
    pub skill_id: String,
    pub hash: String,
    pub path: String,
    pub source_kind: String,
    pub source_url: Option<String>,
    pub source_ref: Option<String>,
    pub source_subdir: Option<String>,
    pub license: Option<String>,
    pub created_at: String,
}

pub struct Registry {
    conn: Connection,
}

impl Registry {
    pub fn open(path: &Path) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("open registry {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let registry = Self { conn };
        registry.init()?;
        Ok(registry)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS skills (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                license TEXT,
                current_hash TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_url TEXT,
                source_ref TEXT,
                source_subdir TEXT,
                has_scripts INTEGER NOT NULL DEFAULT 0,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                installed_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS skill_versions (
                skill_id TEXT NOT NULL,
                hash TEXT NOT NULL,
                path TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_url TEXT,
                source_ref TEXT,
                source_subdir TEXT,
                license TEXT,
                created_at TEXT NOT NULL,
                PRIMARY KEY (skill_id, hash),
                FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS collections (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS collection_skills (
                collection_id TEXT NOT NULL,
                skill_id TEXT NOT NULL,
                PRIMARY KEY (collection_id, skill_id),
                FOREIGN KEY (collection_id) REFERENCES collections(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS global_links (
                skill_id TEXT NOT NULL,
                agent_type TEXT NOT NULL,
                path TEXT NOT NULL,
                state TEXT NOT NULL,
                linked_at TEXT NOT NULL,
                PRIMARY KEY (skill_id, agent_type),
                FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
            );
            "#,
        )?;

        // Upgrade databases created by the first cut.
        self.ensure_column("skills", "source_ref", "TEXT")?;
        self.ensure_column("skills", "source_subdir", "TEXT")?;
        self.ensure_column("skill_versions", "source_ref", "TEXT")?;
        self.ensure_column("skill_versions", "source_subdir", "TEXT")?;
        Ok(())
    }

    fn ensure_column(&self, table: &str, column: &str, declaration: &str) -> Result<()> {
        let mut statement = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !names.iter().any(|name| name == column) {
            self.conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
        Ok(())
    }

    pub fn upsert_skill(&mut self, row: &SkillRow) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO skills (
                id, name, description, license, current_hash, source_kind,
                source_url, source_ref, source_subdir, has_scripts, size_bytes,
                installed_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = excluded.description,
                license = excluded.license,
                current_hash = excluded.current_hash,
                source_kind = excluded.source_kind,
                source_url = excluded.source_url,
                source_ref = excluded.source_ref,
                source_subdir = excluded.source_subdir,
                has_scripts = excluded.has_scripts,
                size_bytes = excluded.size_bytes,
                updated_at = excluded.updated_at
            "#,
            params![
                row.id,
                row.name,
                row.description,
                row.license,
                row.current_hash,
                row.source_kind,
                row.source_url,
                row.source_ref,
                row.source_subdir,
                i64::from(row.has_scripts),
                row.size_bytes as i64,
                row.installed_at,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn add_version(&mut self, row: &VersionRow) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO skill_versions (
                skill_id, hash, path, source_kind, source_url, source_ref,
                source_subdir, license, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                row.skill_id,
                row.hash,
                row.path,
                row.source_kind,
                row.source_url,
                row.source_ref,
                row.source_subdir,
                row.license,
                row.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<SkillSummary>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, name, description, current_hash, license, source_kind,
                   source_url, source_ref, source_subdir, has_scripts, size_bytes,
                   installed_at, updated_at
            FROM skills ORDER BY name COLLATE NOCASE
            "#,
        )?;
        let rows = stmt.query_map([], map_summary)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn get(&self, id: &str) -> Result<Option<SkillSummary>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, name, description, current_hash, license, source_kind,
                   source_url, source_ref, source_subdir, has_scripts, size_bytes,
                   installed_at, updated_at
            FROM skills WHERE id = ?1
            "#,
        )?;
        let row = stmt.query_row(params![id], map_summary).optional()?;
        Ok(row)
    }

    pub fn current_hash(&self, id: &str) -> Result<Option<String>> {
        let hash = self
            .conn
            .query_row(
                "SELECT current_hash FROM skills WHERE id = ?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(hash)
    }

    pub fn create_collection(
        &mut self,
        name: &str,
        description: &str,
        skill_ids: &[String],
    ) -> Result<Collection> {
        let name = name.trim();
        if name.is_empty() {
            bail!("collection name is required");
        }

        let tx = self.conn.transaction()?;
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO collections (id, name, description) VALUES (?1, ?2, ?3)",
            params![id, name, description.trim()],
        )?;

        for skill_id in skill_ids {
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM skills WHERE id = ?1",
                    params![skill_id],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                bail!("unknown skill id in collection: {skill_id}");
            }
            tx.execute(
                "INSERT OR IGNORE INTO collection_skills (collection_id, skill_id) VALUES (?1, ?2)",
                params![id, skill_id],
            )?;
        }
        tx.commit()?;

        Ok(Collection {
            id,
            name: name.to_string(),
            description: description.trim().to_string(),
            skill_ids: skill_ids.to_vec(),
        })
    }

    pub fn list_collections(&self) -> Result<Vec<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description FROM collections ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut result = Vec::new();
        for row in rows {
            let (id, name, description) = row?;
            let mut skills = self.conn.prepare(
                "SELECT skill_id FROM collection_skills WHERE collection_id = ?1 ORDER BY skill_id",
            )?;
            let skill_ids = skills
                .query_map(params![id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            result.push(Collection {
                id,
                name,
                description,
                skill_ids,
            });
        }
        Ok(result)
    }

    pub fn delete_collection(&mut self, id: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM collections WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn delete_skill(&mut self, id: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM collection_skills WHERE skill_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM global_links WHERE skill_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM skill_versions WHERE skill_id = ?1",
            params![id],
        )?;
        let changed = tx.execute("DELETE FROM skills WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn upsert_global_link(&mut self, link: &GlobalLink) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO global_links (skill_id, agent_type, path, state, linked_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(skill_id, agent_type) DO UPDATE SET
                path = excluded.path,
                state = excluded.state,
                linked_at = excluded.linked_at
            "#,
            params![
                link.skill_id,
                link.agent_type,
                link.path,
                link.state,
                link.linked_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_global_links(&self) -> Result<Vec<GlobalLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT skill_id, agent_type, path, state, linked_at FROM global_links ORDER BY agent_type, skill_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(GlobalLink {
                skill_id: row.get(0)?,
                agent_type: row.get(1)?,
                path: row.get(2)?,
                state: row.get(3)?,
                linked_at: row.get(4)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }
}

fn map_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillSummary> {
    Ok(SkillSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        current_hash: row.get(3)?,
        license: row.get(4)?,
        source_kind: row.get(5)?,
        source_url: row.get(6)?,
        source_ref: row.get(7)?,
        source_subdir: row.get(8)?,
        has_scripts: row.get::<_, i64>(9)? != 0,
        size_bytes: row.get::<_, i64>(10)?.max(0) as u64,
        installed_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}
