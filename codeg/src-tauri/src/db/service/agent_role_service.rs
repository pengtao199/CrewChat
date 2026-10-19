//! [crewchat] Roster roles on the custom-agent registry (Phase 2.1).
//!
//! A role lives in the `custom_agent` table as a `kind = 'role'` row — same
//! table, same single writer as the launchable ACP agents. The launcher never
//! sees them (`custom_agent_service::list_launchable_defs` filters), the
//! Agents settings page and every team surface read them through
//! [`AgentRole`], and the pre-2.1 stores (`agent-roles.json` file, the frozen
//! Python roster under `.data/agents`) are imported once by
//! [`migrate_legacy_rosters`] and never read again.
//!
//! Column mapping (see `m20261012_000002_custom_agent_role_fields`):
//! `soul` ⇄ `instructions`, `agent_type` ⇄ `body`, `model` ⇄ `brain_model`,
//! `model_provider` ⇄ `brain_provider`, `skills`/`mcp` ⇄ the JSON columns.
//! `distribution_kind`/`spec_json` keep placeholders ('role' / '{}') because
//! a role is never launched as its own ACP server.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, NotSet,
    QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};

use crate::db::entities::custom_agent;
use crate::db::error::DbError;
use crate::models::agent_role::AgentRole;

pub use crate::db::entities::custom_agent::{KIND_ACP, KIND_ROLE};

/// `app_metadata` key marking the one-time legacy-roster import as done.
pub const MIGRATION_MARKER: &str = "crewchat.agent_roles_migrated";

const LEGACY_ROSTER_FILE: &str = "agent-roles.json";
const LEGACY_BACKUP_SUFFIX: &str = ".pre-phase2.bak";
const PYTHON_ROSTER_DIR: &str = ".data/agents";

// ---------------------------------------------------------------- CRUD

/// Map a `kind = 'role'` row onto the wire type. Corrupt list columns degrade
/// to empty rather than failing the whole listing — a hand-edited DB must not
/// brick the Agents page.
fn row_to_role(row: &custom_agent::Model) -> AgentRole {
    fn names(raw: &str) -> Vec<String> {
        serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
    }
    AgentRole {
        id: row.registry_id.clone(),
        name: row.name.clone(),
        soul: row.instructions.clone(),
        rules: row.rules.clone(),
        agent_type: row.body.clone().unwrap_or_default(),
        model: row.brain_model.clone(),
        model_provider: row.brain_provider.clone(),
        skills: names(&row.skills_json),
        mcp: names(&row.mcp_json),
        permission_mode: row.permission_mode.clone(),
        auto_approve: row.auto_approve,
        created_at: Some(row.created_at.to_rfc3339()),
        updated_at: Some(row.updated_at.to_rfc3339()),
    }
}

/// Every role, oldest first (the order the roster was built in).
pub async fn list_roles(conn: &DatabaseConnection) -> Result<Vec<AgentRole>, DbError> {
    let rows = custom_agent::Entity::find()
        .filter(custom_agent::Column::Kind.eq(KIND_ROLE))
        .order_by_asc(custom_agent::Column::CreatedAt)
        .order_by_asc(custom_agent::Column::Id)
        .all(conn)
        .await?;
    Ok(rows.iter().map(row_to_role).collect())
}

/// One role by id, `None` when the id is unknown or belongs to a launchable
/// ACP agent. Used by the persona frame's resolution (Phase 2.2).
pub async fn get_role(conn: &DatabaseConnection, id: &str) -> Result<Option<AgentRole>, DbError> {
    if id.trim().is_empty() {
        return Ok(None);
    }
    let row = custom_agent::Entity::find()
        .filter(custom_agent::Column::RegistryId.eq(id))
        .filter(custom_agent::Column::Kind.eq(KIND_ROLE))
        .one(conn)
        .await?;
    Ok(row.as_ref().map(row_to_role))
}

/// Insert or update a role. An id that already belongs to a launchable ACP
/// agent is refused — the two kinds share one id space on purpose, and a
/// collision would make one of them unreachable.
pub async fn save_role(
    conn: &DatabaseConnection,
    mut role: AgentRole,
) -> Result<AgentRole, DbError> {
    role.name = role.name.trim().to_string();
    role.agent_type = role.agent_type.trim().to_string();
    if role.name.is_empty() {
        return Err(DbError::Migration("role name is required".into()));
    }
    if role.agent_type.is_empty() {
        return Err(DbError::Migration(
            "role body (agent_type) is required".into(),
        ));
    }
    role.id = role.id.trim().to_string();
    if role.id.is_empty() {
        role.id = uuid::Uuid::new_v4().to_string();
    }
    if !crate::models::agent::is_valid_custom_agent_id(&role.id) {
        return Err(DbError::Migration(format!("invalid role id: {}", role.id)));
    }
    let skills_json =
        serde_json::to_string(&role.skills).map_err(|e| DbError::Migration(e.to_string()))?;
    let mcp_json =
        serde_json::to_string(&role.mcp).map_err(|e| DbError::Migration(e.to_string()))?;
    let now = Utc::now();

    let existing = custom_agent::Entity::find()
        .filter(custom_agent::Column::RegistryId.eq(role.id.clone()))
        .one(conn)
        .await?;
    match existing {
        Some(row) => {
            if row.kind != KIND_ROLE {
                return Err(DbError::Migration(format!(
                    "id {} already belongs to a launchable custom agent",
                    role.id
                )));
            }
            let created_at = row.created_at;
            let mut active = row.into_active_model();
            active.name = Set(role.name.clone());
            active.body = Set(Some(role.agent_type.clone()));
            active.instructions = Set(role.soul.clone());
            active.rules = Set(role.rules.clone());
            active.brain_model = Set(role.model.clone());
            active.brain_provider = Set(role.model_provider.clone());
            active.skills_json = Set(skills_json);
            active.mcp_json = Set(mcp_json);
            active.permission_mode = Set(role.permission_mode.clone());
            active.auto_approve = Set(role.auto_approve);
            active.updated_at = Set(now);
            active.update(conn).await?;
            role.created_at = Some(created_at.to_rfc3339());
            role.updated_at = Some(now.to_rfc3339());
        }
        None => {
            custom_agent::ActiveModel {
                id: NotSet,
                registry_id: Set(role.id.clone()),
                name: Set(role.name.clone()),
                description: Set(String::new()),
                version: Set("role".to_string()),
                distribution_kind: Set(KIND_ROLE.to_string()),
                spec_json: Set("{}".to_string()),
                icon_url: Set(None),
                skills_shared_store: Set(false),
                skills_dir: Set(None),
                source: Set("manual".to_string()),
                version_probe: Set(None),
                supports_mcp: Set(true),
                kind: Set(KIND_ROLE.to_string()),
                body: Set(Some(role.agent_type.clone())),
                instructions: Set(role.soul.clone()),
                rules: Set(role.rules.clone()),
                brain_model: Set(role.model.clone()),
                brain_provider: Set(role.model_provider.clone()),
                skills_json: Set(skills_json),
                mcp_json: Set(mcp_json),
                permission_mode: Set(role.permission_mode.clone()),
                auto_approve: Set(role.auto_approve),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(conn)
            .await?;
            role.created_at = Some(now.to_rfc3339());
            role.updated_at = Some(now.to_rfc3339());
        }
    }
    Ok(role)
}

/// Delete a role. `false` when the id is unknown or belongs to a launchable
/// ACP agent — deleting must never reach across kinds.
pub async fn delete_role(conn: &DatabaseConnection, id: &str) -> Result<bool, DbError> {
    let result = custom_agent::Entity::delete_many()
        .filter(custom_agent::Column::RegistryId.eq(id))
        .filter(custom_agent::Column::Kind.eq(KIND_ROLE))
        .exec(conn)
        .await?;
    Ok(result.rows_affected > 0)
}

pub async fn role_names(conn: &DatabaseConnection) -> Result<BTreeSet<String>, DbError> {
    let rows = custom_agent::Entity::find()
        .filter(custom_agent::Column::Kind.eq(KIND_ROLE))
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(|row| row.name).collect())
}

// ---------------------------------------------------------------- legacy migration

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RosterMigrationReport {
    /// Roles imported from the pre-2.1 `agent-roles.json`.
    pub imported_codeg: usize,
    /// Roles imported from the frozen Python roster (`.data/agents/*.jsonc`).
    pub imported_python: usize,
    /// Legacy entries skipped because a role with the same name already
    /// existed (the Codeg roster wins) — recorded so the import stays
    /// auditable after the fact.
    pub skipped_duplicate_names: Vec<String>,
    /// Files that failed to parse, with the error, for the same reason.
    pub failed_files: Vec<String>,
}

impl RosterMigrationReport {
    pub fn is_empty(&self) -> bool {
        self.imported_codeg == 0
            && self.imported_python == 0
            && self.skipped_duplicate_names.is_empty()
            && self.failed_files.is_empty()
    }
}

/// One-time import of the pre-2.1 stores, run at startup. Resolves the legacy
/// paths and delegates to [`migrate_once`]; the sources are the only thing
/// this adds over it.
pub async fn migrate_legacy_rosters(
    conn: &DatabaseConnection,
) -> Result<Option<RosterMigrationReport>, DbError> {
    let codeg = legacy_codeg_roster_path().filter(|path| path.is_file());
    let python = legacy_python_roster_dir().filter(|dir| dir.is_dir());
    migrate_once(conn, codeg.as_deref(), python.as_deref()).await
}

/// Marker-guarded import with explicit sources (tests drive this directly).
/// `None` means "nothing to do": the marker is already set, or neither legacy
/// store exists. Non-destructive: the original files stay in place, and the
/// Codeg roster is additionally copied to `<name>.pre-phase2.bak` before the
/// import.
pub async fn migrate_once(
    conn: &DatabaseConnection,
    codeg_roster: Option<&Path>,
    python_agents_dir: Option<&Path>,
) -> Result<Option<RosterMigrationReport>, DbError> {
    if crate::db::service::app_metadata_service::get_value(conn, MIGRATION_MARKER)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    if codeg_roster.is_none() && python_agents_dir.is_none() {
        return Ok(None);
    }
    let report = migrate_legacy_rosters_from(conn, codeg_roster, python_agents_dir).await?;
    let summary = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());
    crate::db::service::app_metadata_service::upsert_value(conn, MIGRATION_MARKER, &summary)
        .await?;
    Ok(Some(report))
}

/// The path form of [`migrate_legacy_rosters`] — explicit sources so tests
/// never touch the process environment or the real home directory.
pub async fn migrate_legacy_rosters_from(
    conn: &DatabaseConnection,
    codeg_roster: Option<&Path>,
    python_agents_dir: Option<&Path>,
) -> Result<RosterMigrationReport, DbError> {
    let mut report = RosterMigrationReport::default();
    let mut known_names = role_names(conn).await?;

    if let Some(path) = codeg_roster {
        // Keep a real backup before touching anything (rollback story: delete
        // the imported rows and restore from this copy).
        let backup = path.with_file_name(format!(
            "{}{LEGACY_BACKUP_SUFFIX}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(LEGACY_ROSTER_FILE)
        ));
        if !backup.exists() {
            if let Err(error) = std::fs::copy(path, &backup) {
                tracing::warn!(
                    "[crewchat][roles] could not back up {} to {}: {error}",
                    path.display(),
                    backup.display()
                );
            }
        }
        match read_codeg_roster(path) {
            Ok(roles) => {
                for role in roles {
                    if !known_names.insert(role.name.clone()) {
                        report.skipped_duplicate_names.push(role.name);
                        continue;
                    }
                    match save_role(conn, role).await {
                        Ok(_) => report.imported_codeg += 1,
                        Err(error) => {
                            report
                                .failed_files
                                .push(format!("{}: {error}", path.display()));
                        }
                    }
                }
            }
            Err(error) => report
                .failed_files
                .push(format!("{}: {error}", path.display())),
        }
    }

    if let Some(dir) = python_agents_dir {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|error| DbError::Migration(format!("read {}: {error}", dir.display())))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension().and_then(|ext| ext.to_str()) == Some("jsonc")
                    || path.extension().and_then(|ext| ext.to_str()) == Some("json")
            })
            .collect();
        files.sort();
        for path in files {
            match read_python_roster(&path) {
                Ok(Some(role)) => {
                    if !known_names.insert(role.name.clone()) {
                        report.skipped_duplicate_names.push(role.name);
                        continue;
                    }
                    match save_role(conn, role).await {
                        Ok(_) => report.imported_python += 1,
                        Err(error) => {
                            report
                                .failed_files
                                .push(format!("{}: {error}", path.display()));
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => report
                    .failed_files
                    .push(format!("{}: {error}", path.display())),
            }
        }
    }
    Ok(report)
}

/// `$CODEG_HOME` → `$CODEG_DATA_DIR` → `~/.codeg`, joined with the legacy file
/// name — the exact resolution the pre-2.1 store used.
pub fn legacy_codeg_roster_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEG_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home).join(LEGACY_ROSTER_FILE));
    }
    if let Some(data) = std::env::var_os("CODEG_DATA_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(data).join(LEGACY_ROSTER_FILE));
    }
    dirs::home_dir().map(|home| home.join(".codeg").join(LEGACY_ROSTER_FILE))
}

/// The frozen Python roster. Resolution: `$CREWCHAT_AGENTS_DIR`, then
/// `<cwd>/.data/agents`, then `<cwd>/../.data/agents` (the dev-server cwd is
/// `codeg/`, the repo's `.data` is its parent). Desktop installs have none of
/// these and simply skip this source.
pub fn legacy_python_roster_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CREWCHAT_AGENTS_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(PYTHON_ROSTER_DIR));
        candidates.push(cwd.join("..").join(PYTHON_ROSTER_DIR));
    }
    candidates.into_iter().find(|path| path.is_dir())
}

#[derive(Debug, Deserialize)]
struct LegacyRosterFile {
    #[serde(default)]
    roles: Vec<AgentRole>,
}

fn read_codeg_roster(path: &Path) -> Result<Vec<AgentRole>, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let parsed: LegacyRosterFile = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    Ok(parsed
        .roles
        .into_iter()
        .filter(|role| !role.name.trim().is_empty() && !role.agent_type.trim().is_empty())
        .collect())
}

/// The frozen Python roster file shape (`AgentSpec` from the Python side).
#[derive(Debug, Deserialize)]
struct LegacyPythonAgent {
    #[serde(default)]
    name: String,
    #[serde(default)]
    brain: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    backstory: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcps: Vec<String>,
}

fn read_python_roster(path: &Path) -> Result<Option<AgentRole>, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let stripped = strip_jsonc_comments(&raw);
    let Ok(agent) = serde_json::from_str::<LegacyPythonAgent>(&stripped) else {
        return Ok(None);
    };
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();
    let name = if agent.name.trim().is_empty() {
        stem.clone()
    } else {
        agent.name.trim().to_string()
    };
    if name.is_empty() {
        return Ok(None);
    }
    let (provider, model) = match agent.brain.split_once('/') {
        Some((provider, model)) => (provider.trim().to_string(), model.trim().to_string()),
        None => (String::new(), agent.brain.trim().to_string()),
    };
    let instructions = [agent.role.trim(), agent.goal.trim(), agent.backstory.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(Some(AgentRole {
        id: stem,
        name,
        soul: instructions,
        rules: String::new(),
        agent_type: map_python_body(&agent.body),
        model,
        model_provider: provider,
        skills: agent.skills,
        mcp: agent.mcps,
        // The legacy Python roster never carried permissions: the conversation
        // asked instead. Imported roles therefore start with "no override".
        permission_mode: String::new(),
        auto_approve: false,
        created_at: None,
        updated_at: None,
    }))
}

/// Python-side body ids → codeg wire slugs. Unknown values pass through
/// unchanged; [`save_role`] only requires a non-empty body.
fn map_python_body(body: &str) -> String {
    match body.trim() {
        "claude" | "claude_code" => "claude_code".to_string(),
        "opencode" | "open_code" => "open_code".to_string(),
        other => other.to_string(),
    }
}

/// Remove `//` line comments and `/* … */` block comments, string-aware, so
/// the frozen Python roster's JSONC parses with plain `serde_json`.
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut prev = '\0';
            for next in chars.by_ref() {
                if prev == '*' && next == '/' {
                    break;
                }
                prev = next;
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::fresh_in_memory_db;

    fn role(id: &str, name: &str) -> AgentRole {
        AgentRole {
            id: id.into(),
            name: name.into(),
            soul: "you are a tester".into(),
            rules: "keep it short".into(),
            agent_type: "codex".into(),
            model: "mimo-v2.6-flash".into(),
            model_provider: "prov-1".into(),
            skills: vec!["frontend-design".into()],
            mcp: vec!["playwright".into()],
            permission_mode: String::new(),
            auto_approve: false,
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn save_list_delete_round_trips_through_rows() {
        let db = fresh_in_memory_db().await;
        let saved = save_role(&db.conn, role("", "前端工程师")).await.unwrap();
        assert!(!saved.id.is_empty(), "server generates the id");

        let mut updated = saved.clone();
        updated.skills = vec!["a".into(), "b".into()];
        save_role(&db.conn, updated).await.unwrap();

        let roles = list_roles(&db.conn).await.unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, "前端工程师");
        assert_eq!(roles[0].soul, "you are a tester");
        assert_eq!(roles[0].agent_type, "codex");
        assert_eq!(roles[0].skills, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(roles[0].created_at, saved.created_at);

        assert!(delete_role(&db.conn, &saved.id).await.unwrap());
        assert!(list_roles(&db.conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn role_ids_cannot_collide_with_a_launchable_agent() {
        let db = fresh_in_memory_db().await;
        // A launchable row with the same id space.
        custom_agent::ActiveModel {
            id: NotSet,
            registry_id: Set("shared-id".into()),
            name: Set("Acme".into()),
            description: Set(String::new()),
            version: Set("1".into()),
            distribution_kind: Set("npx".into()),
            spec_json: Set("{\"npx\":{\"package\":\"acme\"}}".into()),
            icon_url: Set(None),
            skills_shared_store: Set(false),
            skills_dir: Set(None),
            source: Set("manual".into()),
            version_probe: Set(None),
            supports_mcp: Set(true),
            kind: Set(KIND_ACP.into()),
            body: Set(None),
            instructions: Set(String::new()),
            rules: Set(String::new()),
            brain_model: Set(String::new()),
            brain_provider: Set(String::new()),
            skills_json: Set("[]".into()),
            mcp_json: Set("[]".into()),
            permission_mode: Set(String::new()),
            auto_approve: Set(false),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let error = save_role(&db.conn, role("shared-id", "Clashing")).await;
        assert!(error.is_err(), "a role may not shadow a launchable agent");
        assert!(!delete_role(&db.conn, "shared-id").await.unwrap());
    }

    #[tokio::test]
    async fn launchable_defs_skip_role_rows() {
        let db = fresh_in_memory_db().await;
        save_role(&db.conn, role("role-1", "角色")).await.unwrap();
        assert!(
            crate::db::service::custom_agent_service::list_launchable_defs(&db.conn)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(list_roles(&db.conn).await.unwrap().len(), 1);
    }

    #[test]
    fn jsonc_stripper_keeps_strings_intact() {
        let input = r#"{
  // line comment
  "url": "https://example.com/*not a comment*/",
  /* block
     comment */
  "name": "with // slashes"
}"#;
        let parsed: serde_json::Value = serde_json::from_str(&strip_jsonc_comments(input)).unwrap();
        assert_eq!(parsed["url"], "https://example.com/*not a comment*/");
        assert_eq!(parsed["name"], "with // slashes");
    }

    #[test]
    fn python_body_ids_map_to_wire_slugs() {
        assert_eq!(map_python_body("claude"), "claude_code");
        assert_eq!(map_python_body("opencode"), "open_code");
        assert_eq!(map_python_body("codex"), "codex");
        assert_eq!(map_python_body("something-else"), "something-else");
    }

    #[tokio::test]
    async fn migration_imports_both_legacy_stores_and_skips_duplicates() {
        let db = fresh_in_memory_db().await;
        let dir = tempfile::tempdir().unwrap();
        let codeg_path = dir.path().join(LEGACY_ROSTER_FILE);
        std::fs::write(
            &codeg_path,
            r#"{"version":1,"roles":[{"id":"r-1","name":"管理员","soul":"lead","agent_type":"claude_code"}]}"#,
        )
        .unwrap();
        let python_dir = dir.path().join("agents");
        std::fs::create_dir_all(&python_dir).unwrap();
        // Duplicate name (skipped) + a fresh one (imported), JSONC comments.
        std::fs::write(
            python_dir.join("crewchat-manager.jsonc"),
            r#"{ /* the frozen manager */
  "name": "管理员",
  "brain": "prov/deepseek-flash",
  "body": "codex",
  "role": "拆解任务",
  "goal": "汇总",
  "backstory": "只规划"
}"#,
        )
        .unwrap();
        std::fs::write(
            python_dir.join("frontend-expert.jsonc"),
            r#"{
  // frontend persona
  "name": "前端专家",
  "brain": "prov/mimo-v2.6-flash",
  "body": "opencode",
  "role": "做界面",
  "goal": "按项目约定改",
  "backstory": "",
  "skills": ["frontend-design"],
  "mcps": ["playwright"]
}"#,
        )
        .unwrap();

        let report = migrate_legacy_rosters_from(&db.conn, Some(&codeg_path), Some(&python_dir))
            .await
            .unwrap();
        assert_eq!(report.imported_codeg, 1);
        assert_eq!(report.imported_python, 1);
        assert_eq!(report.skipped_duplicate_names, vec!["管理员".to_string()]);
        assert!(report.failed_files.is_empty(), "{:?}", report.failed_files);

        let roles = list_roles(&db.conn).await.unwrap();
        assert_eq!(roles.len(), 2);
        let frontend = roles.iter().find(|r| r.name == "前端专家").unwrap();
        assert_eq!(frontend.id, "frontend-expert");
        assert_eq!(frontend.agent_type, "open_code");
        assert_eq!(frontend.model_provider, "prov");
        assert_eq!(frontend.model, "mimo-v2.6-flash");
        assert_eq!(frontend.soul, "做界面\n\n按项目约定改");
        assert_eq!(frontend.skills, vec!["frontend-design".to_string()]);
        assert_eq!(frontend.mcp, vec!["playwright".to_string()]);
        // The pre-import copy exists for rollback.
        assert!(dir
            .path()
            .join(format!("{LEGACY_ROSTER_FILE}{LEGACY_BACKUP_SUFFIX}"))
            .exists());
    }

    #[tokio::test]
    async fn the_marker_prevents_a_second_import() {
        let db = fresh_in_memory_db().await;
        let dir = tempfile::tempdir().unwrap();
        let codeg_path = dir.path().join(LEGACY_ROSTER_FILE);
        std::fs::write(
            &codeg_path,
            r#"{"roles":[{"id":"r-1","name":"角色一","agent_type":"codex"}]}"#,
        )
        .unwrap();
        let first = migrate_once(&db.conn, Some(&codeg_path), None)
            .await
            .unwrap()
            .expect("first run reports");
        assert_eq!(first.imported_codeg, 1);

        // Even after deleting the imported row, the marker stops a re-run —
        // the import is a one-shot, not a recurring sync.
        assert!(delete_role(&db.conn, "r-1").await.unwrap());
        let second = migrate_once(&db.conn, Some(&codeg_path), None)
            .await
            .unwrap();
        assert!(second.is_none());
        assert!(list_roles(&db.conn).await.unwrap().is_empty());
    }
}
