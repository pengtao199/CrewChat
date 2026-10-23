use std::path::Path;

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::Store;

impl Store {
    pub fn list_folders(&self) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, path, name, alias, sort_order FROM folders
             WHERE is_open = 1 ORDER BY sort_order, name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "path": row.get::<_, String>(1)?,
                    "name": row.get::<_, String>(2)?,
                    "alias": row.get::<_, String>(3)?,
                    "sort_order": row.get::<_, i64>(4)?,
                    "kind": "regular",
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn open_folder(&self, path: &str) -> Result<Value> {
        let path = path.trim();
        if path.is_empty() {
            anyhow::bail!("path is required");
        }
        let name = Path::new(path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Workspace")
            .to_string();
        let conn = self.conn.lock().unwrap();
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM folders WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            conn.execute("UPDATE folders SET is_open = 1 WHERE id = ?1", params![id])?;
            return Ok(json!({
                "id": id, "path": path, "name": name, "alias": "",
                "sort_order": 0, "kind": "regular"
            }));
        }
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO folders (id, path, name, alias, sort_order, is_open)
             VALUES (?1, ?2, ?3, '', 0, 1)",
            params![id, path, name],
        )?;
        Ok(json!({
            "id": id, "path": path, "name": name, "alias": "",
            "sort_order": 0, "kind": "regular"
        }))
    }

    pub fn remove_folder(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE folders SET is_open = 0 WHERE id = ?1", params![id])?;
        Ok(())
    }
}
