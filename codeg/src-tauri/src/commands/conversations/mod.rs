use std::collections::{HashMap, HashSet};

#[cfg(feature = "tauri-runtime")]
use tauri::Manager;

use crate::app_error::AppCommandError;
use crate::db::entities::conversation;
use crate::db::entities::folder::FolderKind;
use crate::db::service::{conversation_service, folder_service, import_service, tab_service};
#[cfg(feature = "tauri-runtime")]
use crate::db::AppDatabase;
use crate::models::*;
// Concrete parser type only for `load_thread_name_index`, which is codex's own
// index reader and not part of the `AgentParser` trait. Every history read goes
// through `build_agent_parser`.
use crate::parsers::codex::CodexParser;
use crate::parsers::{
    build_agent_parser, folder_name_from_path, normalize_path_for_matching, path_eq_for_matching,
    AgentParser, ParseError,
};
use crate::web::event_bridge::{
    emit_event, ConversationChange, ConversationsBulkChanged, EventEmitter, ImportScanProgress,
    TabsChanged, CONVERSATIONS_BULK_CHANGED_EVENT, CONVERSATION_CHANGED_EVENT,
    IMPORT_SCAN_PROGRESS_EVENT, TABS_CHANGED_EVENT,
};

#[derive(Default)]
pub(crate) struct ListAllConversationsOptions {
    pub(crate) folder_ids: Option<Vec<i32>>,
    pub(crate) agent_type: Option<AgentType>,
    pub(crate) search: Option<String>,
    pub(crate) sort_by: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) include_children: bool,
}

pub(crate) async fn list_all_conversations_core(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    options: ListAllConversationsOptions,
) -> Result<Vec<DbConversationSummary>, AppCommandError> {
    let codex_titles = match tokio::task::spawn_blocking(|| {
        CodexParser::new().load_thread_name_index()
    })
    .await
    {
        Ok(titles) => titles,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "conversation list: failed to load Codex session titles; continuing without refresh"
            );
            HashMap::new()
        }
    };
    list_all_conversations_core_with_codex_titles(
        conn,
        emitter,
        chat_channel_manager,
        options,
        &codex_titles,
    )
    .await
}

async fn list_all_conversations_core_with_codex_titles(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    options: ListAllConversationsOptions,
    codex_titles: &HashMap<String, String>,
) -> Result<Vec<DbConversationSummary>, AppCommandError> {
    // Synchronize before `list_all` builds any folder/agent/search/status
    // filters so a freshly generated Codex title is visible on this same call.
    let refreshed_ids = conversation_service::refresh_codex_auto_titles(conn, codex_titles).await;
    // Detached on purpose — see `notify_conversation_title_updates`. The list
    // must not wait on Telegram.
    drop(
        notify_conversation_title_updates(conn, emitter, chat_channel_manager, refreshed_ids).await,
    );
    let ListAllConversationsOptions {
        folder_ids,
        agent_type,
        search,
        sort_by,
        status,
        include_children,
    } = options;
    conversation_service::list_all(
        conn,
        folder_ids,
        agent_type,
        search,
        sort_by,
        status,
        include_children,
    )
    .await
    .map_err(AppCommandError::from)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn list_all_conversations(
    app: tauri::AppHandle,
    folder_ids: Option<Vec<i32>>,
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    status: Option<String>,
    include_children: Option<bool>,
) -> Result<Vec<DbConversationSummary>, AppCommandError> {
    let emitter = EventEmitter::Tauri(app.clone());
    let db = app.state::<AppDatabase>();
    let chat_channel_manager =
        app.state::<crate::chat_channel::manager::ChatChannelManager>();
    list_all_conversations_core(
        &db.conn,
        &emitter,
        &chat_channel_manager,
        ListAllConversationsOptions {
            folder_ids,
            agent_type,
            search,
            sort_by,
            status,
            include_children: include_children.unwrap_or(false),
        },
    )
    .await
}

pub async fn list_child_conversations_core(
    conn: &sea_orm::DatabaseConnection,
    parent_conversation_id: i32,
) -> Result<Vec<DbConversationSummary>, AppCommandError> {
    conversation_service::list_children(conn, parent_conversation_id)
        .await
        .map_err(AppCommandError::from)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn list_child_conversations(
    db: tauri::State<'_, AppDatabase>,
    parent_conversation_id: i32,
) -> Result<Vec<DbConversationSummary>, AppCommandError> {
    list_child_conversations_core(&db.conn, parent_conversation_id).await
}

pub async fn list_opened_tabs_core(
    conn: &sea_orm::DatabaseConnection,
) -> Result<OpenedTabsSnapshot, AppCommandError> {
    // Single-transaction snapshot: reading tabs and version separately could
    // tear under a concurrent save (old tabs stamped with the new version).
    let (items, version) = tab_service::snapshot_tabs(conn)
        .await
        .map_err(AppCommandError::from)?;
    Ok(OpenedTabsSnapshot { items, version })
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn list_opened_tabs(
    db: tauri::State<'_, AppDatabase>,
) -> Result<OpenedTabsSnapshot, AppCommandError> {
    list_opened_tabs_core(&db.conn).await
}

/// Persist the open-tab set with compare-and-set on the workspace tab version,
/// then broadcast the new set on `tabs://changed` (echoing `origin` so the
/// originating client ignores its own change). A stale save (version mismatch —
/// another client committed first) is rejected without writing or emitting; the
/// caller gets `accepted: false` plus the current truth to reconcile.
pub async fn save_opened_tabs_core(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    items: Vec<OpenedTab>,
    expected_version: i64,
    origin: String,
) -> Result<SaveTabsOutcome, AppCommandError> {
    let outcome = tab_service::save_all_tabs_cas(conn, items, expected_version)
        .await
        .map_err(AppCommandError::from)?;

    if outcome.accepted {
        emit_tabs_changed(emitter, outcome.version, outcome.tabs.clone(), origin);
    }

    Ok(SaveTabsOutcome {
        accepted: outcome.accepted,
        version: outcome.version,
        tabs: outcome.tabs,
    })
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn save_opened_tabs(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    items: Vec<OpenedTab>,
    expected_version: i64,
    origin: String,
) -> Result<SaveTabsOutcome, AppCommandError> {
    save_opened_tabs_core(
        &db.conn,
        &EventEmitter::Tauri(app),
        items,
        expected_version,
        origin,
    )
    .await
}

/// Synchronous implementation shared by list_conversations, list_folders, and get_stats.
fn list_conversations_sync(
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    folder_path: Option<String>,
) -> Vec<ConversationSummary> {
    let mut all_conversations = Vec::new();
    let mut seen_keys = HashSet::new();

    // BUILTIN_AGENT_TYPES, not `registry::builtin_acp_agents()`: the two differ
    // in ORDER, and this list's order is the sidebar's tie-break for two
    // conversations that compare equal on the active sort.
    let mut parsers: Vec<(AgentType, Box<dyn AgentParser>)> =
        crate::models::agent::BUILTIN_AGENT_TYPES
            .iter()
            .map(|&at| (at, build_agent_parser(at)))
            .collect();
    // Registered custom agents read back from codeg's own ACP transcripts, so
    // their sessions participate in folder grouping and stats like any other.
    for custom in crate::acp::custom_registry::all() {
        parsers.push((custom, build_agent_parser(custom)));
    }

    for (at, parser) in &parsers {
        if let Some(ref filter) = agent_type {
            if filter != at {
                continue;
            }
        }
        match parser.list_conversations() {
            Ok(conversations) => {
                // Deduplicate conversations based on (agent_type, id) combination
                for conversation in conversations {
                    let key = format!("{:?}-{}", conversation.agent_type, conversation.id);
                    if seen_keys.insert(key) {
                        all_conversations.push(conversation);
                    }
                }
            }
            Err(e) => {
                tracing::error!("Error listing {} conversations: {}", at, e);
            }
        }
    }

    // Apply search filter
    if let Some(ref query) = search {
        let query_lower = query.to_lowercase();
        all_conversations.retain(|s| {
            s.title
                .as_ref()
                .map(|t| t.to_lowercase().contains(&query_lower))
                .unwrap_or(false)
                || s.folder_name
                    .as_ref()
                    .map(|p| p.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
                || s.folder_path
                    .as_ref()
                    .map(|p| p.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
                || s.git_branch
                    .as_ref()
                    .map(|b| b.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
                || s.model
                    .as_ref()
                    .map(|m| m.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
        });
    }

    // Apply folder path filter
    if let Some(ref fp) = folder_path {
        all_conversations.retain(|s| {
            s.folder_path
                .as_deref()
                .map(|p| path_eq_for_matching(p, fp.as_str()))
                .unwrap_or(false)
        });
    }

    // Apply sorting
    match sort_by.as_deref() {
        Some("oldest") => all_conversations.sort_by_key(|a| a.started_at),
        Some("messages") => {
            all_conversations.sort_by_key(|b| std::cmp::Reverse(b.message_count));
        }
        _ => all_conversations.sort_by_key(|b| std::cmp::Reverse(b.started_at)), // default: newest first
    }

    all_conversations
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn list_conversations(
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    folder_path: Option<String>,
) -> Result<Vec<ConversationSummary>, AppCommandError> {
    tokio::task::spawn_blocking(move || {
        list_conversations_sync(agent_type, search, sort_by, folder_path)
    })
    .await
    .map_err(|e| {
        AppCommandError::task_execution_failed("Failed to list conversations")
            .with_detail(e.to_string())
    })
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_conversation(
    agent_type: AgentType,
    conversation_id: String,
) -> Result<ConversationDetail, AppCommandError> {
    tokio::task::spawn_blocking(move || -> Result<ConversationDetail, AppCommandError> {
        build_agent_parser(agent_type)
            .get_conversation(&conversation_id)
            .map_err(parse_error_to_app_error)
    })
    .await
    .map_err(|e| {
        AppCommandError::task_execution_failed("Failed to load conversation")
            .with_detail(e.to_string())
    })?
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn list_folders() -> Result<Vec<FolderInfo>, AppCommandError> {
    tokio::task::spawn_blocking(move || -> Result<Vec<FolderInfo>, AppCommandError> {
        let all_conversations = list_conversations_sync(None, None, None, None);
        Ok(compute_folders(&all_conversations))
    })
    .await
    .map_err(|e| {
        AppCommandError::task_execution_failed("Failed to list folders").with_detail(e.to_string())
    })?
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_stats() -> Result<AgentStats, AppCommandError> {
    tokio::task::spawn_blocking(move || -> Result<AgentStats, AppCommandError> {
        let all_conversations = list_conversations_sync(None, None, None, None);
        Ok(compute_stats(&all_conversations))
    })
    .await
    .map_err(|e| {
        AppCommandError::task_execution_failed("Failed to compute conversation stats")
            .with_detail(e.to_string())
    })?
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_sidebar_data() -> Result<SidebarData, AppCommandError> {
    tokio::task::spawn_blocking(move || -> Result<SidebarData, AppCommandError> {
        let all_conversations = list_conversations_sync(None, None, None, None);
        let folders = compute_folders(&all_conversations);
        let stats = compute_stats(&all_conversations);
        Ok(SidebarData { folders, stats })
    })
    .await
    .map_err(|e| {
        AppCommandError::task_execution_failed("Failed to build sidebar data")
            .with_detail(e.to_string())
    })?
}

fn compute_folders(all_conversations: &[ConversationSummary]) -> Vec<FolderInfo> {
    let mut folder_map: HashMap<String, FolderInfo> = HashMap::new();

    for conversation in all_conversations {
        let path = conversation
            .folder_path
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let name = conversation
            .folder_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        let entry = folder_map
            .entry(path.clone())
            .or_insert_with(|| FolderInfo {
                path: path.clone(),
                name,
                agent_types: Vec::new(),
                conversation_count: 0,
            });

        entry.conversation_count += 1;
        if !entry.agent_types.contains(&conversation.agent_type) {
            entry.agent_types.push(conversation.agent_type);
        }
    }

    let mut folders: Vec<FolderInfo> = folder_map.into_values().collect();
    folders.sort_by_key(|b| std::cmp::Reverse(b.conversation_count));
    folders
}

pub async fn import_local_conversations_core(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    folder_id: i32,
) -> Result<ImportResult, AppCommandError> {
    // Share IMPORT_GUARD with the batch importer: `(external_id, agent_type)`
    // has no DB unique index, so this legacy path racing a batch import (or a
    // second legacy call) could double-insert. try_lock rejects the overlap
    // rather than queueing — matching `import_selected_sessions_core`. (No UI
    // still calls this command; it is kept only for API/back-compat.)
    let _guard = IMPORT_GUARD
        .try_lock()
        .map_err(|_| AppCommandError::invalid_input("An import is already in progress"))?;

    let folder = folder_service::get_folder_by_id(conn, folder_id)
        .await
        .map_err(AppCommandError::from)?
        .ok_or_else(|| {
            AppCommandError::not_found("Folder not found")
                .with_detail(format!("folder_id={folder_id}"))
        })?;

    let (result, updated_ids) =
        import_service::import_local_conversations(conn, folder_id, &folder.path)
            .await
            .map_err(AppCommandError::from)?;

    // Broadcast a sidebar upsert for every title refreshed in place, so other
    // windows and web clients converge live, and propagate the new name to any
    // bound chat thread — the same treatment the scan and list paths give a
    // title discovered outside codeg. The importing client refetches the list
    // itself, which also covers the newly imported rows.
    drop(
        notify_conversation_title_updates(conn, emitter, chat_channel_manager, updated_ids).await,
    );

    Ok(result)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn import_local_conversations(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    chat_channel_manager: tauri::State<'_, crate::chat_channel::manager::ChatChannelManager>,
    folder_id: i32,
) -> Result<ImportResult, AppCommandError> {
    import_local_conversations_core(
        &db.conn,
        &EventEmitter::Tauri(app),
        &chat_channel_manager,
        folder_id,
    )
    .await
}

/// Serializes concurrent batch imports: `(external_id, agent_type)` has no DB
/// unique index (and adding one now could fail on historical duplicates), so
/// two overlapping imports could double-insert the same session. `try_lock`
/// instead of queueing — a second import racing the first is a user mistake to
/// surface, not work to serialize.
static IMPORT_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The DB's stored string for an [`AgentType`] (its snake_case serde name) —
/// the same conversion `import_one` uses for the `agent_type` column.
fn agent_type_db_str(at: &AgentType) -> String {
    serde_json::to_value(at)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Minimal projection of a `folder` row for scan/import reconciliation (keeps
/// `build_scan_result` constructible in tests without full SeaORM models).
struct ScanFolderRow {
    id: i32,
    path: String,
    name: String,
    deleted: bool,
    /// The root folder this row was registered under, when it is a worktree
    /// child. Import reconciliation uses it to flatten and validate new parent
    /// relationships.
    parent_id: Option<i32>,
}

async fn load_folder_rows(
    conn: &sea_orm::DatabaseConnection,
) -> Result<Vec<ScanFolderRow>, AppCommandError> {
    use sea_orm::EntityTrait;
    let rows = crate::db::entities::folder::Entity::find()
        .all(conn)
        .await
        .map_err(crate::db::error::DbError::from)
        .map_err(AppCommandError::from)?;
    Ok(rows
        .into_iter()
        .map(|f| ScanFolderRow {
            id: f.id,
            path: f.path,
            name: f.name,
            deleted: f.deleted_at.is_some(),
            parent_id: f.parent_id,
        })
        .collect())
}

/// Normalized path → folder row, preferring a live row when a soft-deleted
/// variant of the same normalized path also exists (both can coexist since
/// `UNIQUE(path)` is on the raw string).
fn index_folder_rows(rows: &[ScanFolderRow]) -> HashMap<String, &ScanFolderRow> {
    let mut index: HashMap<String, &ScanFolderRow> = HashMap::new();
    for row in rows {
        let slot = index.entry(normalize_path_for_matching(&row.path)).or_insert(row);
        if slot.deleted && !row.deleted {
            *slot = row;
        }
    }
    index
}

/// The main working tree an imported session's cwd belongs to, as a
/// [`normalize_path_for_matching`] key — `None` when the cwd is not a linked git
/// worktree (a plain repo, a submodule, a directory that is gone), which is
/// top-level by definition.
///
/// Resolving the ROOT rather than a folder id keeps the "is this a worktree"
/// question separate from "is that repo a folder codeg has", which the importer
/// answers later and against folders it may not have created yet.
fn worktree_root_key(path: &str) -> Option<String> {
    let root = crate::git_repo::main_worktree_root(std::path::Path::new(path))?;
    let key = normalize_path_for_matching(&root.to_string_lossy());
    // Nothing may be its own parent: the sidebar drops a self-parented folder
    // from the top level and then renders it under itself, i.e. nowhere. Only a
    // corrupt `commondir` can land here, and the guard costs one compare.
    (key != normalize_path_for_matching(path)).then_some(key)
}

/// Normalized path → the candidate root folder id a linked worktree of that
/// path should hang off. Seeded from the folder rows on disk; the importer adds
/// the folders it creates as it goes. The write side separately verifies that
/// the candidate is still a live top-level row.
///
/// The id is FLATTENED the way `open_worktree_folder_core` flattens: a repo
/// folder that is itself recorded as somebody's worktree child hands down its
/// own root. A two-level chain would be worse than no grouping at all — the
/// sidebar's worktree merge is single-level, so a grandchild's conversations
/// bucket under a folder that is itself merged away and stop rendering.
///
/// Soft-deleted rows are left out: a deleted repo renders nowhere, and a child
/// of it would render nowhere either, which is worse than the top-level folder
/// the user gets today.
fn worktree_parent_folder_ids(
    folder_index: &HashMap<String, &ScanFolderRow>,
) -> HashMap<String, i32> {
    folder_index
        .iter()
        .filter(|(_, row)| !row.deleted)
        .map(|(key, row)| (key.clone(), row.parent_id.unwrap_or(row.id)))
        .collect()
}

/// Pure grouping/reconciliation for the import-picker scan.
/// `imported_index` maps `(agent_type_db_str, external_id)` → "a live row
/// exists" (false = only soft-deleted rows).
fn build_scan_result(
    summaries: Vec<(AgentType, ConversationSummary)>,
    imported_index: &HashMap<(String, String), bool>,
    folder_rows: &[ScanFolderRow],
) -> ScanResult {
    struct GroupAcc {
        path: String,
        name: String,
        exists_in_codeg: bool,
        folder_id: Option<i32>,
        agent_types: Vec<AgentType>,
        sessions: Vec<ScanSession>,
    }

    let folder_index = index_folder_rows(folder_rows);
    let mut groups: HashMap<String, GroupAcc> = HashMap::new();
    let mut no_folder_count = 0u32;

    for (at, summary) in summaries {
        let raw_path = match summary.folder_path.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                no_folder_count += 1;
                continue;
            }
        };
        let key = normalize_path_for_matching(&raw_path);
        let entry = groups.entry(key.clone()).or_insert_with(|| {
            let row = folder_index.get(&key).copied();
            GroupAcc {
                // Reuse the stored row's exact path string so the import-side
                // add_folder upsert hits the same UNIQUE(path) key instead of
                // minting a near-duplicate from a trailing-slash/case variant.
                path: row.map(|r| r.path.clone()).unwrap_or_else(|| raw_path.clone()),
                name: row
                    .map(|r| r.name.clone())
                    .or_else(|| summary.folder_name.clone())
                    .unwrap_or_else(|| folder_name_from_path(&raw_path)),
                exists_in_codeg: row.map(|r| !r.deleted).unwrap_or(false),
                folder_id: row.map(|r| r.id),
                agent_types: Vec::new(),
                sessions: Vec::new(),
            }
        });
        if !entry.agent_types.contains(&at) {
            entry.agent_types.push(at);
        }
        let status = match imported_index.get(&(agent_type_db_str(&at), summary.id.clone())) {
            None => ScanSessionStatus::New,
            Some(true) => ScanSessionStatus::Imported,
            Some(false) => ScanSessionStatus::Deleted,
        };
        entry.sessions.push(ScanSession {
            external_id: summary.id,
            agent_type: at,
            title: summary.title,
            started_at: summary.started_at,
            ended_at: summary.ended_at,
            message_count: summary.message_count,
            model: summary.model,
            git_branch: summary.git_branch,
            status,
        });
    }

    let mut folders: Vec<ScanFolder> = groups
        .into_values()
        .map(|mut g| {
            g.sessions
                .sort_by_key(|s| std::cmp::Reverse(s.started_at));
            ScanFolder {
                path: g.path,
                name: g.name,
                exists_in_codeg: g.exists_in_codeg,
                folder_id: g.folder_id,
                agent_types: g.agent_types,
                sessions: g.sessions,
            }
        })
        .collect();

    fn importable(f: &ScanFolder) -> u32 {
        f.sessions
            .iter()
            .filter(|s| s.status == ScanSessionStatus::New)
            .count() as u32
    }
    folders.sort_by(|a, b| {
        importable(b)
            .cmp(&importable(a))
            .then_with(|| a.path.cmp(&b.path))
    });

    let total_sessions = folders.iter().map(|f| f.sessions.len() as u32).sum();
    let importable_count = folders.iter().map(importable).sum();

    ScanResult {
        folders,
        no_folder_count,
        total_sessions,
        importable_count,
    }
}

/// Scan every local agent's sessions and reconcile them against the DB for the
/// import-picker window. Emits [`IMPORT_SCAN_PROGRESS_EVENT`] once per parser
/// while the walk runs.
///
/// The scan also refreshes the conversations that are ALREADY imported, from
/// the same parse it just did: a title generated after the first import, and
/// the transcript's own last-activity time when the user kept working on the
/// session in the agent's own CLI (see
/// [`import_service::sync_imported_sessions`]). Without this, a re-scan can
/// only ever offer the *new* sessions — the picker does not let you re-select
/// an imported one — so an already-imported conversation would keep the
/// `updated_at` it had at import time forever, and sit in the wrong place in a
/// recency-sorted sidebar. Each refreshed row is broadcast so every window and
/// web client re-sorts live.
pub async fn scan_importable_sessions_core(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
) -> Result<ScanResult, AppCommandError> {
    let progress_emitter = emitter.clone();
    let summaries =
        import_service::collect_local_summaries(move |agent_type, done, total, session_count| {
            emit_event(
                &progress_emitter,
                IMPORT_SCAN_PROGRESS_EVENT,
                ImportScanProgress {
                    agent_type,
                    done,
                    total,
                    session_count,
                },
            );
        })
        .await;

    scan_importable_sessions_from_summaries(conn, emitter, chat_channel_manager, summaries).await
}

/// Reconcile summaries already collected by the filesystem scan. Keeping this
/// boundary separate makes the DB refresh and notification behavior testable
/// without reading the developer's real agent session directories.
async fn scan_importable_sessions_from_summaries(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    summaries: Vec<(AgentType, ConversationSummary)>,
) -> Result<ScanResult, AppCommandError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let conv_rows = conversation::Entity::find()
        .filter(conversation::Column::ExternalId.is_not_null())
        .all(conn)
        .await
        .map_err(crate::db::error::DbError::from)
        .map_err(AppCommandError::from)?;
    let mut imported_index: HashMap<(String, String), bool> = HashMap::new();
    for row in &conv_rows {
        let Some(external_id) = row.external_id.clone() else {
            continue;
        };
        let live = row.deleted_at.is_none();
        let entry = imported_index
            .entry((row.agent_type.clone(), external_id))
            .or_insert(live);
        *entry = *entry || live;
    }

    // Refresh the already-imported rows in place before answering, then
    // broadcast each one so open sidebars re-sort without a refetch. The
    // chat-channel half runs detached so the scan never waits on Telegram.
    drop(
        notify_conversation_title_updates(
            conn,
            emitter,
            chat_channel_manager,
            import_service::sync_imported_sessions(conn, &conv_rows, &summaries).await,
        )
        .await,
    );

    let folder_rows = load_folder_rows(conn).await?;
    Ok(build_scan_result(summaries, &imported_index, &folder_rows))
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn scan_importable_sessions(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    chat_channel_manager: tauri::State<'_, crate::chat_channel::manager::ChatChannelManager>,
) -> Result<ScanResult, AppCommandError> {
    scan_importable_sessions_core(&db.conn, &EventEmitter::Tauri(app), &chat_channel_manager).await
}

/// Batch-import the selected sessions, creating (or reopening) each target
/// folder as needed. Test seam for [`import_selected_sessions_core`]: takes the
/// scanned summaries as input instead of walking the filesystem.
pub(crate) async fn import_selected_from_summaries(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    summaries: Vec<(AgentType, ConversationSummary)>,
    selections: Vec<SelectedSessionKey>,
) -> Result<ImportSelectedResult, AppCommandError> {
    const MAX_ERRORS: usize = 10;

    // Defense-in-depth mirror of collect_local_summaries' child filter: a
    // delegation child must never import as a root row, so a selection key
    // pointing at one resolves to not_found.
    let mut by_key: HashMap<(AgentType, String), (AgentType, ConversationSummary)> = summaries
        .into_iter()
        .filter(|(_, c)| c.parent_id.is_none())
        .map(|(at, c)| ((at, c.id.clone()), (at, c)))
        .collect();

    // Group the resolved selections by normalized cwd. Duplicate keys in the
    // request resolve once (the map entry is consumed); a key that no longer
    // resolves — vanished from disk since the scan, cwd-less, or bogus — counts
    // as not_found.
    let mut not_found = 0u32;
    let mut groups: HashMap<String, Vec<(AgentType, ConversationSummary)>> = HashMap::new();
    let mut seen_keys: HashSet<(AgentType, String)> = HashSet::new();
    for key in selections {
        if !seen_keys.insert((key.agent_type, key.external_id.clone())) {
            continue;
        }
        let Some((at, summary)) = by_key.remove(&(key.agent_type, key.external_id)) else {
            not_found += 1;
            continue;
        };
        let raw_path = match summary.folder_path.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                not_found += 1;
                continue;
            }
        };
        groups
            .entry(normalize_path_for_matching(&raw_path))
            .or_default()
            .push((at, summary));
    }

    let folder_rows = load_folder_rows(conn).await?;
    let folder_index = index_folder_rows(&folder_rows);

    let mut result = ImportSelectedResult {
        imported: 0,
        updated: 0,
        skipped: 0,
        restored: 0,
        not_found,
        failed: 0,
        created_folders: 0,
        folders: Vec::new(),
        errors: Vec::new(),
    };
    let mut touched_folder_ids: Vec<i32> = Vec::new();

    // Resolve every group's target path and worktree root BEFORE importing any
    // of them, so the order they are imported in can depend on it.
    struct PendingImport {
        norm_key: String,
        target_path: String,
        created: bool,
        /// The main working tree of this cwd, when it is a linked worktree —
        /// a normalized path, not yet a folder id (see [`worktree_root_key`]).
        root_key: Option<String>,
        items: Vec<(AgentType, ConversationSummary)>,
    }

    let mut ordered: Vec<PendingImport> = groups
        .into_iter()
        .map(|(norm_key, items)| {
            let row = folder_index.get(&norm_key).copied();
            // Import into the stored row's exact path when one normalize-matches
            // (see build_scan_result); otherwise the parser cwd creates the folder.
            let target_path = row.map(|r| r.path.clone()).unwrap_or_else(|| {
                items[0]
                    .1
                    .folder_path
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string()
            });
            PendingImport {
                created: row.map(|r| r.deleted).unwrap_or(true),
                root_key: worktree_root_key(&target_path),
                norm_key,
                target_path,
                items,
            }
        })
        .collect();

    // Deterministic folder order (normalized path) so results and tests are
    // stable regardless of HashMap iteration — but every plain folder is
    // imported BEFORE any linked worktree, so a repo and a worktree of it
    // selected in the same run still group. One pass suffices, no topological
    // sort: a main working tree is never itself a linked worktree, so a repo can
    // never be waiting on another group. Without the split, whether the two end
    // up grouped would come down to which of their paths sorts first.
    ordered.sort_by(|a, b| {
        a.root_key
            .is_some()
            .cmp(&b.root_key.is_some())
            .then_with(|| a.norm_key.cmp(&b.norm_key))
    });
    let mut worktree_parents = worktree_parent_folder_ids(&folder_index);
    // Parent ids are not protected by a foreign key. Restrict new writes to
    // live top-level rows so a stale/dangling id or a pre-existing parent chain
    // cannot turn this import into a chain or cycle of its own. Plain folders
    // selected in this batch join the set after `add_folder` revives them.
    let mut top_level_parent_ids: HashSet<i32> = folder_rows
        .iter()
        .filter(|row| !row.deleted && row.parent_id.is_none())
        .map(|row| row.id)
        .collect();
    // Reparenting a folder that already owns children would make those rows
    // grandchildren. Track counts through this batch so that operation falls
    // back to Preserve as well.
    let mut folder_child_counts: HashMap<i32, usize> = HashMap::new();
    for row in &folder_rows {
        if let Some(parent_id) = row.parent_id {
            *folder_child_counts.entry(parent_id).or_default() += 1;
        }
    }

    for pending in ordered {
        let PendingImport {
            norm_key,
            target_path,
            created,
            root_key,
            items,
        } = pending;

        // `add_folder` is the only fallible step here — `import_summaries` is
        // resilient (per-row failures are counted, never aborting the group), so
        // a partial failure still commits and reports its good rows and still
        // broadcasts the folder it created.
        //
        // A cwd that is a linked worktree of a repo codeg has goes in as a CHILD
        // of that repo, the way `open_worktree_folder_core` records one. Plain
        // `add_folder` leaves `parent_id` NULL, which is exactly the sidebar's
        // test for "top-level folder", so the same worktree lands beside its
        // repo instead of under it (and the branch-label backfill, which selects
        // on `parent_id IS NOT NULL`, never reaches it). Falling back to
        // `add_folder` when nothing resolves keeps `ParentWrite::Preserve` for
        // every other case, so a reopen can never demote a folder that a
        // worktree open already parented. See issue #552.
        let target_row = folder_index.get(&norm_key).copied();
        let target_folder_id = target_row.map(|row| row.id);
        let existing_parent_id = target_row.and_then(|row| row.parent_id);
        let target_has_children = target_folder_id.is_some_and(|folder_id| {
            folder_child_counts.get(&folder_id).copied().unwrap_or(0) > 0
        });
        let parent_id = root_key
            .as_ref()
            .and_then(|key| worktree_parents.get(key).copied())
            // A historical top-level worktree can have its main-tree row
            // recorded underneath it. Flattening through that row points back
            // at the worktree's own folder id even though their PATHS differ,
            // so the path-level guard in `worktree_root_key` cannot catch it.
            .filter(|parent_id| Some(*parent_id) != target_folder_id)
            .filter(|parent_id| top_level_parent_ids.contains(parent_id))
            // Moving an existing parent under the repo would strand its current
            // children one level deeper. Rewriting the same established edge is
            // harmless; any new edge requires a childless target.
            .filter(|parent_id| existing_parent_id == Some(*parent_id) || !target_has_children);
        let add = match parent_id {
            Some(parent_id) => {
                folder_service::add_folder_with_parent(conn, &target_path, Some(parent_id)).await
            }
            None => folder_service::add_folder(conn, &target_path).await,
        };
        match add.map_err(AppCommandError::from) {
            Ok(entry) => {
                let folder_id = entry.id;
                // `add_folder_with_parent` sets the resolved parent; the
                // fallback `add_folder` preserves an existing row's parent and
                // inserts a new row at the top level. Keep the safety set in
                // step with that exact persisted state.
                let persisted_parent_id = parent_id.or(existing_parent_id);
                if persisted_parent_id != existing_parent_id {
                    if let Some(old_parent_id) = existing_parent_id {
                        if let Some(count) = folder_child_counts.get_mut(&old_parent_id) {
                            *count = count.saturating_sub(1);
                        }
                    }
                    if let Some(new_parent_id) = persisted_parent_id {
                        *folder_child_counts.entry(new_parent_id).or_default() += 1;
                    }
                }
                if persisted_parent_id.is_none() {
                    top_level_parent_ids.insert(folder_id);
                } else {
                    top_level_parent_ids.remove(&folder_id);
                }
                // This folder can now be the repo a LATER group's worktree hangs
                // off — the sort above put every plain folder ahead of every
                // worktree precisely so this lands in time. Only plain folders
                // are recorded: a linked worktree is never anyone's main working
                // tree. The candidate is flattened by the seed's rule, off the
                // parent this row actually ended up with.
                if root_key.is_none() {
                    let root = persisted_parent_id.unwrap_or(folder_id);
                    worktree_parents.insert(norm_key, root);
                }
                // `DeletedPolicy::Restore`: every item here is a session the
                // user explicitly checked in the picker, which badges a
                // soft-deleted row as such — so a deleted one in this list is a
                // deliberate "bring it back", not a sweep. (The whole-folder
                // import and the scan's drive-by refresh both stay on `Skip`.)
                let (tally, _updated_ids, failed_in_group) =
                    import_service::import_summaries_resilient(
                        conn,
                        folder_id,
                        &items,
                        import_service::DeletedPolicy::Restore,
                    )
                    .await;
                result.imported += tally.imported;
                result.updated += tally.updated;
                result.skipped += tally.skipped;
                result.restored += tally.restored;
                result.failed += failed_in_group;
                if created {
                    result.created_folders += 1;
                }
                touched_folder_ids.push(folder_id);
                result.folders.push(ImportFolderOutcome {
                    path: target_path.clone(),
                    folder_id,
                    created,
                    imported: tally.imported,
                    updated: tally.updated,
                    skipped: tally.skipped,
                    restored: tally.restored,
                });
                if failed_in_group > 0 && result.errors.len() < MAX_ERRORS {
                    result
                        .errors
                        .push(format!("{target_path}: {failed_in_group} session(s) failed"));
                }
                // Broadcast every touched folder: even a pre-existing row may
                // have flipped is_open/deleted_at in add_folder, and clients
                // need the row to place the imported conversations — so this
                // fires even when some of the group's rows failed.
                if let Ok(Some(detail)) = folder_service::get_folder_by_id(conn, folder_id).await {
                    crate::commands::folders::emit_folder_upsert(emitter, detail);
                }
            }
            // The folder itself could not be created/reopened — the whole group
            // produced nothing, so there is no folder to broadcast.
            Err(e) => {
                result.failed += items.len() as u32;
                if result.errors.len() < MAX_ERRORS {
                    result.errors.push(format!("{target_path}: {e}"));
                }
            }
        }
    }

    // One nudge instead of per-row upserts: clients answer with a single full
    // refetch, which also covers refreshed titles (see the event's doc) and the
    // rows this run brought back from a soft delete — a restore adds a row to
    // every open sidebar just like a fresh import does, so it must fire the
    // event too. (The counts stay faithful to their own tallies; subscribers
    // read only the channel and answer with a full refetch.)
    if result.imported > 0 || result.updated > 0 || result.restored > 0 {
        emit_event(
            emitter,
            CONVERSATIONS_BULK_CHANGED_EVENT,
            ConversationsBulkChanged {
                imported: result.imported,
                updated: result.updated,
                folder_ids: touched_folder_ids,
            },
        );
    }

    Ok(result)
}

/// Import the selected scanned sessions. Re-walks the parsers rather than
/// trusting client-echoed summaries — the scan is moments old and the disk is
/// the source of truth. Runs under [`IMPORT_GUARD`]; if the picker window is
/// closed mid-import the future still completes and events still broadcast.
pub async fn import_selected_sessions_core(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    selections: Vec<SelectedSessionKey>,
) -> Result<ImportSelectedResult, AppCommandError> {
    if selections.is_empty() {
        return Err(AppCommandError::invalid_input("No sessions selected"));
    }
    let _guard = IMPORT_GUARD
        .try_lock()
        .map_err(|_| AppCommandError::invalid_input("An import is already in progress"))?;

    let summaries = import_service::collect_local_summaries(|_, _, _, _| {}).await;
    import_selected_from_summaries(conn, emitter, summaries, selections).await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn import_selected_sessions(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    selections: Vec<SelectedSessionKey>,
) -> Result<ImportSelectedResult, AppCommandError> {
    import_selected_sessions_core(&db.conn, &EventEmitter::Tauri(app), selections).await
}

/// Build the `meta["codeg.delegation"]` value for a delegation child loaded
/// from the DB. Mirrors the shape produced at runtime by
/// `acp::delegation::meta_writer::build_delegation_meta`, but only includes
/// the fields the DB can vouch for: `status`, `child_conversation_id`,
/// `task_id`, `task_preview` and `agent_type`. `child_connection_id` is
/// omitted (no live connection for a historical view; the frontend's parser
/// treats it as optional).
///
/// The last three are pure FALLBACKS on the frontend
/// (`use-delegation-card-model.ts` prefers the parsed `raw_input` and the live
/// binding), so supplying them can't override a better source. They exist for
/// the cards that have no better source: a `resume_delegation` call — whose
/// arguments are only `{task_id, reason}` — and a `delegate_to_agent` call on a
/// host whose announcements never carry arguments (Cursor).
///
/// Status mapping:
///  - `in_progress` → `running` (still streaming or about to)
///  - `pending_review` → `completed` (set by `TurnComplete { stop_reason:
///    "end_turn" }` — the success path; the live broker writes `completed`
///    for this same outcome, see `acp/delegation/broker.rs` Ok arm).
///  - `completed` → `completed`
///  - `cancelled` → `failed` with NO `error_code`. The DB's `Cancelled`
///    variant covers both user-cancel and turn-failure modes (refusal,
///    max_tokens, max_turn_requests, empty, unknown — see
///    `acp/lifecycle.rs` TurnComplete branch), and the broker writes a
///    distinct `error_code` per failure mode at runtime. Since the DB
///    persists only the bucket and not the specific code, we cannot
///    truthfully label the failure here — emit `failed` without a code
///    rather than mislabel non-cancel failures as `"canceled"`.
///  - other (defensive) → `running`
fn build_historical_delegation_meta(child: &DbConversationSummary) -> serde_json::Value {
    let status: &str = match child.status.as_str() {
        "in_progress" => "running",
        "pending_review" | "completed" => "completed",
        "cancelled" => "failed",
        _ => "running",
    };
    let mut obj = serde_json::Map::new();
    obj.insert("status".into(), serde_json::Value::String(status.into()));
    obj.insert(
        "child_conversation_id".into(),
        serde_json::Value::Number(child.id.into()),
    );
    obj.insert(
        "agent_type".into(),
        serde_json::Value::String(child.agent_type.as_wire().into_owned()),
    );
    if let Some(task_id) = child.delegation_call_id.as_deref() {
        obj.insert("task_id".into(), serde_json::Value::String(task_id.into()));
    }
    // The child row's title was seeded from the original task text — the same
    // substitute the broker uses for `task_preview` when it resumes a task.
    if let Some(title) = child.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        obj.insert(
            "task_preview".into(),
            serde_json::Value::String(title.into()),
        );
    }
    // [crewchat] The member role the child runs as, off the row's own team
    // snapshot — the card's name label after a reload, when the live meta is
    // gone. Mirrors the broker's `member_role_id` meta field.
    if let Some(role_id) = child
        .team
        .as_ref()
        .map(|team| team.leader_role_id.trim())
        .filter(|role_id| !role_id.is_empty())
    {
        obj.insert(
            "member_role_id".into(),
            serde_json::Value::String(role_id.into()),
        );
    }
    serde_json::Value::Object(obj)
}

/// The broker-minted task id a `delegate_to_agent` result announces. Codex
/// persists the ack as prose (`Delegation successful. task_id=<id>. Call
/// get_delegation_status …`); other hosts return `{"task_id":"<id>"}` — both
/// are covered by reading `task_id` followed by `=` or `:`. Mirrors the
/// frontend's `parseDelegateTaskId` (`lib/delegation-card.ts`).
fn parse_delegate_task_id(output: &str) -> Option<String> {
    let at = output.find("task_id")? + "task_id".len();
    let rest = output[at..].trim_start();
    // Closing quote of a JSON key, then the separator, then the value's quote.
    let rest = rest.strip_prefix('"').unwrap_or(rest).trim_start();
    let rest = rest.strip_prefix(['=', ':'])?.trim_start();
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Descend through wrapper envelopes looking for a `task_id` string. A wrapper
/// value may itself be a JSON *string* (hosts that stringify nested arguments),
/// so re-parse those. Depth-capped like the frontend walker.
///
/// Shares `acp::lifecycle`'s key list rather than restating it: that list, its
/// frontend twins in `delegation-card.ts` / `codeg-mcp-tool.ts`, and this
/// walker must peel the same envelopes, or a card and the meta injected beneath
/// it disagree about which task a call names.
fn find_task_id_in_value(value: &serde_json::Value, depth: u8) -> Option<String> {
    use crate::acp::lifecycle::ARGS_WRAPPER_KEYS;

    if depth > 4 {
        return None;
    }
    if let Some(s) = value.as_str() {
        let nested: serde_json::Value = serde_json::from_str(s).ok()?;
        return find_task_id_in_value(&nested, depth + 1);
    }
    let obj = value.as_object()?;
    for key in ARGS_WRAPPER_KEYS {
        if let Some(inner) = obj.get(key) {
            if let Some(found) = find_task_id_in_value(inner, depth + 1) {
                return Some(found);
            }
        }
    }
    let id = obj.get("task_id")?.as_str()?.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// The `task_id` argument of a `resume_delegation` call, read off the tool
/// use's serialized arguments (`{"task_id": "...", "reason": "..."}`). Unlike
/// `delegate_to_agent` — whose id only exists on the RESULT — resume names its
/// task in the request, so this needs no cross-block correlation.
///
/// Two host realities stop a plain `from_str(input)["task_id"]` from finding
/// it, and both end the same way: no binding, so the reloaded card is stuck on
/// the `running` its own ack froze and shows no task text — the exact history
/// gap the resume card exists to close.
///   * NESTING. CodeBuddy routes MCP calls through `DeferExecuteTool` and
///     persists `{"toolName": …, "params": {…}}`, deliberately leaving the
///     wrapper on `input_preview` for readers to peel (see
///     `parsers::codebuddy::deferred_tool_name`); Antigravity wraps in
///     `{"arguments": {…}}`.
///   * TRUNCATION. `input_preview` is a *preview*: parsers cap it (500 chars
///     for OpenClaw, 2000 for Cline), so a long `reason` leaves the JSON
///     unparseable even though `task_id` — written first in practice — is
///     intact in what survived.
///
/// So: peel wrappers off well-formed JSON, else fall back to the same tolerant
/// scan `parse_delegate_task_id` already uses on this file's sibling path. A
/// scan that guesses wrong is harmless — the id simply matches no child in
/// `by_task_id` and nothing is injected.
fn parse_resume_task_id(input: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(input) {
        if let Some(id) = find_task_id_in_value(&value, 0) {
            return Some(id);
        }
    }
    parse_delegate_task_id(input)
}

/// Walk every `delegate_to_agent` / `resume_delegation` ToolUse block in
/// `turns` and, when it can be matched to a child conversation in `children`,
/// set `meta["codeg.delegation"]` to the DB-derived snapshot. Skips blocks
/// whose meta is already populated so the live-broker write (when present)
/// always wins. Tool-name match is by substring to cover the MCP-prefixed
/// (`mcp__codeg-mcp__delegate_to_agent`) and bare forms the host may have
/// emitted.
///
/// For `delegate_to_agent`, matching is by `parent_tool_use_id` first, then by
/// the broker's task id. The fallback is what covers codex: its rollout names
/// the call `call_<id>`, while the broker — which sees the call over the ACP
/// wire, where code mode renames every inner call — recorded `exec-<uuid>`. The
/// two never meet, so every codex delegation card lost its
/// `child_conversation_id` and with it the "查看会话" affordance. The task id
/// round-trips: the broker mirrors it into `delegation_call_id`, and the ack the
/// model received carries it verbatim.
///
/// For `resume_delegation` the ONLY key is the task id, taken from the call's
/// own arguments: a resume never owns a `parent_tool_use_id` (it re-binds to the
/// ORIGINAL delegate call's id, which belongs to a different block, usually in
/// an earlier turn). Without this the resumed card would be frozen at the
/// `running` its ack reported, forever — the child's real outcome landed on the
/// DB row, not on the resume result.
fn inject_delegation_meta(turns: &mut [MessageTurn], children: &[DbConversationSummary]) {
    if children.is_empty() {
        return;
    }
    let by_parent_tool_use_id: HashMap<&str, &DbConversationSummary> = children
        .iter()
        .filter_map(|c| c.parent_tool_use_id.as_deref().map(|tu| (tu, c)))
        .collect();
    let by_task_id: HashMap<&str, &DbConversationSummary> = children
        .iter()
        .filter_map(|c| c.delegation_call_id.as_deref().map(|id| (id, c)))
        .collect();

    // The task id lives on the call's RESULT, which the parsers emit as a
    // separate block (usually a later turn), so collect it up front.
    let mut task_id_by_call: HashMap<String, String> = HashMap::new();
    if !by_task_id.is_empty() {
        for turn in turns.iter() {
            for block in turn.blocks.iter() {
                if let ContentBlock::ToolResult {
                    tool_use_id: Some(tu),
                    output_preview: Some(output),
                    ..
                } = block
                {
                    if let Some(task_id) = parse_delegate_task_id(output) {
                        task_id_by_call.insert(tu.clone(), task_id);
                    }
                }
            }
        }
    }

    for turn in turns.iter_mut() {
        for block in turn.blocks.iter_mut() {
            let ContentBlock::ToolUse {
                tool_use_id,
                tool_name,
                input_preview,
                meta,
                ..
            } = block
            else {
                continue;
            };
            if meta.is_some() {
                continue;
            }
            let child: Option<&DbConversationSummary> =
                if tool_name.contains("delegate_to_agent") {
                    tool_use_id.as_deref().and_then(|tu| {
                        by_parent_tool_use_id
                            .get(tu)
                            .or_else(|| {
                                task_id_by_call
                                    .get(tu)
                                    .and_then(|task_id| by_task_id.get(task_id.as_str()))
                            })
                            .copied()
                    })
                } else if tool_name.contains("resume_delegation") {
                    input_preview
                        .as_deref()
                        .and_then(parse_resume_task_id)
                        .and_then(|task_id| by_task_id.get(task_id.as_str()).copied())
                } else {
                    continue;
                };
            if let Some(child) = child {
                *meta = Some(serde_json::json!({
                    "codeg.delegation": build_historical_delegation_meta(child),
                }));
            }
        }
    }
}

/// Core logic for loading a folder conversation with full OpenClaw fallback.
/// Shared by both the Tauri command and the web handler.
///
/// Returns the detail plus the title parsed from the session file this call
/// just read (`None` when no file matched). The live wrapper uses that title to
/// backfill the DB row's title when the user hasn't locked it — reusing this
/// already-happening per-turn parse rather than reading the file again.
pub async fn get_folder_conversation_core(
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
) -> Result<(DbConversationDetail, Option<String>), AppCommandError> {
    let summary = conversation_service::get_by_id(conn, conversation_id)
        .await
        .map_err(AppCommandError::from)?;

    let (mut turns, session_stats, resolved_ext_id, parsed_title, parsed_model, transcript_watermark) =
        if let Some(ref ext_id) = summary.external_id {
        let at = summary.agent_type;
        let eid = ext_id.clone();
        let db_created_at = summary.created_at;
        // Prefer the recorded origin cwd (set when a removed task worktree's
        // conversations were re-parented) over the current folder's path — the
        // session file still carries the ORIGINAL cwd, so matching on the new
        // parent folder would never find it.
        let folder_path_for_fallback = match summary.origin_cwd.clone() {
            Some(cwd) => Some(cwd),
            None => folder_service::get_folder_by_id(conn, summary.folder_id)
                .await
                .ok()
                .flatten()
                .map(|f| f.path),
        };
        tokio::task::spawn_blocking(move || -> Result<_, AppCommandError> {
            let parser = build_agent_parser(at);
            match parser.get_conversation(&eid) {
                Ok(d) => {
                    // Claude `/clear` (and similar on-disk id changes) make
                    // the parser resolve a different uuid than we asked for.
                    // Persist that so reopen/reconnect follow the live file.
                    let resolved = (d.summary.id != eid).then(|| d.summary.id.clone());
                    Ok((
                        d.turns,
                        d.session_stats,
                        resolved,
                        d.summary.title,
                        d.summary.model,
                        d.transcript_watermark,
                    ))
                }
                Err(crate::parsers::ParseError::ConversationNotFound(_)) => {
                    // The external_id may no longer match any local file —
                    // e.g. an ACP session UUID (OpenClaw, Cline) or a stale
                    // ID after session/new fallback overwrote the original
                    // (Gemini CLI).  Fall back to matching by folder_path
                    // and started_at from the parsed conversation list.
                    if matches!(
                        at,
                        AgentType::OpenClaw | AgentType::Cline | AgentType::Gemini
                    ) {
                        if let Ok(all) = parser.list_conversations() {
                            // Filter by folder_path first, then find the closest
                            // started_at match within 300 seconds of db_created_at.
                            let matched = all
                                .into_iter()
                                .filter(|c| {
                                    c.folder_path
                                        .as_ref()
                                        .zip(folder_path_for_fallback.as_ref())
                                        .is_some_and(|(a, b)| path_eq_for_matching(a, b))
                                })
                                .min_by_key(|c| {
                                    (c.started_at - db_created_at).num_seconds().unsigned_abs()
                                })
                                .filter(|c| {
                                    let diff =
                                        (c.started_at - db_created_at).num_seconds().unsigned_abs();
                                    diff < 300
                                });
                            if let Some(conv) = matched {
                                let new_ext_id = conv.id.clone();
                                if let Ok(d) = parser.get_conversation(&new_ext_id) {
                                    return Ok((
                                        d.turns,
                                        d.session_stats,
                                        Some(new_ext_id),
                                        d.summary.title,
                                        d.summary.model,
                                        d.transcript_watermark,
                                    ));
                                }
                            }
                        }
                    }
                    Ok((vec![], None, None, None, None, None))
                }
                Err(e) => Err(parse_error_to_app_error(e)),
            }
        })
        .await
        .map_err(|e| {
            AppCommandError::task_execution_failed(
                "Failed to read conversation turns from session file",
            )
            .with_detail(e.to_string())
        })??
    } else {
        (vec![], None, None, None, None, None)
    };

    // If we resolved a different external_id (e.g. ACP UUID → parser branch ID,
    // or a Claude `/clear` transcript rollover), update the database so future
    // lookups are direct. Also patch the summary this call returns so the
    // caller reconnects with the id that actually has the turns.
    //
    // Gemini/Cline is an ALIAS normalization — both ids denote the same
    // session — so it uses the narrow CAS rather than `bind_external_id`,
    // whose history-split would manufacture a phantom conversation for the
    // old spelling. Claude `/clear` is a real new transcript file of the
    // SAME conversation; passing the outgoing id as `continues` advances
    // in place instead of splitting a sidebar clone.
    let mut summary = summary;
    if let Some(new_ext_id) = resolved_ext_id {
        if matches!(summary.agent_type, AgentType::ClaudeCode) {
            let continues: Vec<String> = summary.external_id.iter().cloned().collect();
            // Refused when another row already holds the successor — the
            // rollover's own file can have been imported as its own
            // conversation. The summary must then keep the id this row
            // actually owns: handing the caller an id it does not hold is
            // what sends the next prompt into the HOLDER's transcript while
            // every event names this row (see `bind_external_id`).
            match conversation_service::bind_external_id(
                conn,
                conversation_id,
                &new_ext_id,
                &continues,
            )
            .await
            {
                Ok(_) => summary.external_id = Some(new_ext_id),
                Err(e) => {
                    tracing::warn!(
                        conversation_id,
                        to_session = %new_ext_id,
                        error = %e,
                        "[conversations] could not follow the transcript rollover; \
                         keeping the id this row holds"
                    );
                }
            }
        } else {
            let _ = conversation_service::renormalize_external_id_alias(
                conn,
                conversation_id,
                summary.external_id.as_deref(),
                new_ext_id.clone(),
            )
            .await;
            summary.external_id = Some(new_ext_id);
        }
    }
    summary.message_count = crate::parsers::message_turn_count(&turns);
    // The transcript is the richer source for the session's model. Codex is
    // the concrete case: an ACP-driven row is created before any
    // `turn_context` names a model, so the DB column can stay NULL forever
    // while the rollout file knows the answer.
    //
    // The parse WINS over the column rather than merely filling a hole in it.
    // `seed_model_if_empty` now persists the first model a session is seen
    // using, so "fill only when NULL" would pin this summary — and with it the
    // details dialog, which reads `summary.model` ahead of the turns — to that
    // first value for the life of the conversation, and a mid-session `/model`
    // switch would never show. The stored value stays as the fallback for a
    // transcript that names no model at all.
    if let Some(parsed) = parsed_model.filter(|m| !m.trim().is_empty()) {
        summary.model = Some(parsed);
    }

    // Historical recovery for the read-only sub-agent viewer: JSONL parsers
    // don't carry `meta["codeg.delegation"]`, so a reloaded conversation
    // can't drive the parent UI's child-conversation lookup. Join on
    // `parent_id = summary.id` to repopulate it from the DB. Failure to
    // fetch children silently degrades to "no button on the card" (the
    // pre-fix behavior), never to a failed detail load.
    let children = conversation_service::list_children(conn, conversation_id)
        .await
        .unwrap_or_default();
    inject_delegation_meta(&mut turns, &children);

    Ok((
        DbConversationDetail {
            summary,
            turns,
            session_stats,
            transcript_watermark,
            in_flight_user_turn_id: None,
            turns_offset: None,
            turns_total: None,
            assistant_turns_before_offset: None,
            prefix_hash: None,
            uncovered_prefix_max_ts: None,
        },
        parsed_title,
    ))
}

/// A normalized, comparable view of a user turn's renderable content. Used to
/// match the live in-flight prompt (`UserMessageBlock`s) against a parser-built
/// user turn (`ContentBlock`s), whose two id namespaces never line up. Mirrors
/// the frontend `userTurnContentKey`: only text and image carry identity, text
/// is compared verbatim, images by `(mime_type, data)`, and block order is
/// preserved so a rearrangement of the same pieces is not a match.
#[derive(PartialEq)]
enum UserContentSig {
    Text(String),
    Image { mime_type: String, data: String },
}

fn sig_from_user_message_blocks(
    blocks: &[crate::acp::types::UserMessageBlock],
) -> Vec<UserContentSig> {
    blocks
        .iter()
        .map(|b| match b {
            crate::acp::types::UserMessageBlock::Text { text } => {
                UserContentSig::Text(text.clone())
            }
            crate::acp::types::UserMessageBlock::Image { data, mime_type } => {
                UserContentSig::Image {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                }
            }
        })
        .collect()
}

/// `Some(sig)` only for a plain user prompt (text/image blocks). Any other block
/// type means this isn't a prompt we can match by content, so we return `None`
/// and the caller leaves the turn untouched.
fn sig_from_turn_blocks(blocks: &[ContentBlock]) -> Option<Vec<UserContentSig>> {
    let mut sig = Vec::with_capacity(blocks.len());
    for b in blocks {
        match b {
            ContentBlock::Text { text } => sig.push(UserContentSig::Text(text.clone())),
            ContentBlock::Image {
                data, mime_type, ..
            } => sig.push(UserContentSig::Image {
                mime_type: mime_type.clone(),
                data: data.clone(),
            }),
            _ => return None,
        }
    }
    Some(sig)
}

/// How many USER turns [`apply_in_flight_message_id`]'s walk will compare
/// before giving up.
///
/// A cost bound, not a correctness one — the ambiguity check inside the walk is
/// what keeps it from stamping an earlier round's prompt. `sig_from_turn_blocks`
/// copies each candidate's text and image bytes, and the walk runs on every
/// detail fetch, so a transcript whose timestamps are all parse instants (see
/// the walk) must not turn that into a scan of every prompt ever sent.
///
/// Counts USER turns only, deliberately. Counting turns would count the length
/// of the reply, which is unrelated to the hazard and routinely in the
/// hundreds: a real window holds the prompt and whatever the user sent
/// mid-turn, so 32 is never reached by a turn with usable timestamps.
const MAX_IN_FLIGHT_WALK_USER_TURNS: usize = 32;

/// Stamp the persisted in-flight user turn with the broadcast `message_id`.
///
/// A cross-client viewer renders the in-flight prompt from two sources that use
/// different ids: the live broadcast/snapshot keys it by `pending.message_id`,
/// while the reloaded transcript carries the same prompt under a parser-assigned
/// `turn-N` id. Rewriting the persisted turn's id to the broadcast id lets the
/// frontend's id-dedup collapse the two into one instead of showing the prompt
/// twice.
///
/// The in-flight prompt is located by walking back from the transcript tail
/// over the turns this turn produced — those persisted at/after `started_at`,
/// comparing at most [`MAX_IN_FLIGHT_WALK_USER_TURNS`] user turns of them — and
/// keeping the EARLIEST user turn whose content matches. The tail itself is the
/// prompt only for Claude/Codex, which write the assistant turn on completion;
/// OpenCode and Gemini persist a partial reply mid-stream (which a parser may
/// split), and a message the user sends mid-turn is written after the prompt as
/// a user turn of its own. Earliest, because the agent writes the prompt before
/// anything it produces in reply, so a mid-turn message repeating the prompt's
/// own words cannot take the stamp from it.
///
/// That "earliest" only orders the round's own turns, which presumes the walk
/// stopped at the round's start. It does when the gate below fires. When it
/// never fires — every turn in hand is at/after `started_at`, which a parser
/// stamping parse instants makes routine — the walk saw the whole transcript
/// and earliest means nothing, so a second matching copy leaves it unable to
/// say which round it is in: it then stamps nothing rather than guess.
///
/// A recency check then disambiguates: the in-flight prompt was persisted by the
/// agent CLI at/after `started_at` (the agent — a local subprocess sharing this
/// machine's clock — writes the prompt on receiving it), whereas a *prior*
/// identical prompt was persisted during an earlier turn and so predates
/// `started_at`. Without it, a repeated identical prompt whose tail is
/// `[user X, COMPLETED assistant]` (the new copy not yet persisted) would be
/// mistaken for the in-flight prompt and stamped, which — combined with the
/// frontend's keep-first user dedup — would HIDE the genuinely new prompt.
/// Neither agent exposes a per-turn "still streaming" flag in its transcript
/// (OpenCode falls back to the creation timestamp and folds completed tool
/// rows; Gemini always stamps a completion time), so this wall-clock recency is
/// the reliable signal. `started_at` is captured when the backend broadcasts the
/// `UserMessage` event — strictly before the agent request is issued — so the
/// in-flight prompt is always persisted at/after it and no backward tolerance is
/// needed; allowing one would risk mis-stamping a fast prior identical prompt
/// and hiding the new one.
///
/// The match also requires identical content, so an unrelated prompt is never
/// stamped; on no match the turns are left untouched and the viewer keeps
/// showing its synthesized copy — a recoverable transient duplicate, never a
/// hidden prompt. When `started_at` is unknown the recency check can't run, so
/// nothing is stamped (the safe, keep-visible default).
///
/// Returns the stamped turn's (new) id when a stamp is applied, so the caller can
/// surface it on the detail response as `in_flight_user_turn_id`. The frontend
/// uses that to locate the in-flight prompt and, while the live reply is in hand,
/// hide the partial assistant turn OpenCode/Gemini persist after it mid-stream
/// (which would otherwise double-render against the live reply). Returning the id
/// rather than truncating here is deliberate: removing the partial server-side
/// could hide a *completed* reply in the end-of-turn race (the agent may persist
/// the final assistant row before the backend processes `TurnComplete` and clears
/// the live state, after which an attaching client's snapshot can't recover it).
fn apply_in_flight_message_id(
    turns: &mut [MessageTurn],
    pending: &crate::acp::session_state::PendingUserMessage,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<String> {
    let n = turns.len();
    if n == 0 {
        return None;
    }
    let started_at = started_at?;
    let want = sig_from_user_message_blocks(&pending.blocks);

    // Walk back over the turns THIS turn produced and keep the EARLIEST user
    // turn whose content is the pending prompt's.
    //
    // A walk, not the tail. The prompt is the last turn only while the agent
    // has written nothing else, and what trails it is not bounded to one
    // assistant turn:
    //
    //   * a message the user sends MID-TURN (`/steering`) is written into the
    //     transcript as a USER turn after the prompt, so the tail becomes that
    //     message, whose content is not the prompt's;
    //   * OpenCode and Gemini persist the reply as it goes, and a parser that
    //     splits it leaves two or more assistant turns behind the prompt.
    //
    // Anchoring on the last one or two turns lost the stamp in the middle of
    // exactly those turns, and every consumer reads a missing stamp as "this
    // detail is settled, the turn is over": `computeTimelinePrefix` stops
    // hiding the persisted half of the reply the live stream is re-showing (so
    // the first half renders twice), the runtime store's `detailIsInFlight`
    // lets a mid-turn refetch clear `liveMessage` / `localTurns` /
    // `optimisticTurns`, and `collectInFlightPersistedToolCalls` stops marking
    // the round's unfinished tool calls, which then paint as completed.
    //
    // EARLIEST, not last: the agent writes the prompt before anything it
    // produces in reply, so within one turn the first copy of those words is
    // the prompt itself. That is what keeps a mid-turn message repeating the
    // prompt's own text ("continue" twice) from taking the stamp off it.
    //
    // Recency gate, which is both what makes the walk safe and what bounds it.
    // `started_at` is recorded when the backend broadcasts the `UserMessage`
    // event, which happens *before* the agent request is issued (see
    // `connection.rs`), so the agent — a local subprocess on this machine's
    // clock — necessarily persists the in-flight prompt at a wall-clock instant
    // at or after `started_at`. A *prior* identical prompt was persisted during
    // an earlier turn and is therefore strictly older. We allow no backward
    // tolerance: any window before `started_at` could admit a fast prior
    // identical prompt (a turn can complete and be re-sent in well under a
    // second), and stamping it would HIDE the genuinely new prompt via the
    // frontend's keep-first user dedup. Erring the other way only ever yields a
    // recoverable visible duplicate, so the strict bound is the safe one.
    //
    // Turns are in transcript order, so the first one older than the start ends
    // the search — the walk never reads past the running turn, and an
    // out-of-order timestamp can only cut it short, which stamps nothing.
    //
    // …unless the timestamps are not the agent's at all. Two shipping parsers
    // fall back to the PARSE INSTANT when a record carries no usable time:
    // `cline.rs` seeds `last_ts` from `Utc::now()` when the manifest has no
    // `started_at` and hands it to every message with `ts` missing or 0, and
    // `antigravity.rs` writes `ts.unwrap_or_else(Utc::now)` per step. A parse
    // instant is by construction at/after `started_at`, so there the gate never
    // fires, the "window" is the whole transcript, and "earliest match" could
    // reach an identical prompt from an earlier round — stamping THAT makes
    // `visiblePersistedTurns` hide every assistant turn after it.
    //
    // `gate_fired` is what distinguishes the two. It is false in exactly two
    // shapes: the transcript holds nothing older than this turn (a first turn —
    // there is no earlier round to confuse anything with), or the timestamps are
    // parse instants (there may be). Both are covered by asking whether the
    // decision was AMBIGUOUS: one matching user turn in the whole transcript is
    // the prompt whatever the clocks say, because the text is then unique to it.
    // Two or more, with nothing proving where this turn began, is a guess — and
    // guessing wrong here hides turns, so it refuses instead.
    //
    // NOT a cap on turns walked. The obvious bound — stop after N turns — counts
    // the LENGTH OF THE REPLY, which is the one quantity that has nothing to do
    // with the hazard: every mid-turn-persisting parser emits one assistant turn
    // per assistant record (`claude.rs`, `opencode.rs`, `gemini.rs` all keep
    // turns small for virtualization), so a measured Claude round runs to a
    // median of 44 and a p90 of 317. Any N small enough to bound the hazard
    // voids the stamp part-way through most ordinary rounds, mid-turn, which is
    // the exact failure this walk exists to remove.
    let mut target_idx: Option<usize> = None;
    let mut matched = 0usize;
    let mut user_turns_seen = 0usize;
    let mut gate_fired = false;
    for i in (0..n).rev() {
        if turns[i].timestamp < started_at {
            gate_fired = true;
            break;
        }
        if !matches!(turns[i].role, TurnRole::User) {
            continue;
        }
        // A cost bound, not a correctness one — `sig_from_turn_blocks` copies
        // each turn's text and image bytes, and this runs on every detail
        // fetch. Only reachable when the gate never fires (a real window holds
        // the prompt plus the handful of messages sent mid-turn), and refusing
        // there is the same safe direction as the ambiguity check below.
        user_turns_seen += 1;
        if user_turns_seen > MAX_IN_FLIGHT_WALK_USER_TURNS {
            return None;
        }
        if sig_from_turn_blocks(&turns[i].blocks).as_ref() == Some(&want) {
            matched += 1;
            target_idx = Some(i);
        }
    }
    if !gate_fired && matched > 1 {
        return None;
    }
    let target_idx = target_idx?;

    // Never create a duplicate id. The broadcast id is normally disjoint from
    // parser `turn-N` ids (and `is_reserved_turn_id` in the manager rejects a
    // client id of that shape), but defend the invariant here too: if the id
    // already exists on another turn, stamping would make two turns share an
    // id and the frontend's id-keyed dedup could hide one. Leave the turn
    // under its parser id — a recoverable visible duplicate, never a hidden
    // prompt — and report nothing.
    let collides = turns
        .iter()
        .enumerate()
        .any(|(i, t)| i != target_idx && t.id == pending.message_id);
    if collides {
        return None;
    }
    turns[target_idx].id = pending.message_id.clone();
    Some(pending.message_id.clone())
}

/// Resolve the raw `tailTurns` / `fromIndex` request fields into a window
/// selector. `None` when neither is present (legacy full response); an error
/// when both are (the two coordinate systems are mutually exclusive).
pub fn resolve_turn_window_req(
    tail_turns: Option<usize>,
    from_index: Option<usize>,
) -> Result<Option<crate::commands::turn_window::TurnWindowReq>, AppCommandError> {
    use crate::commands::turn_window::TurnWindowReq;
    match (tail_turns, from_index) {
        (Some(_), Some(_)) => Err(AppCommandError::invalid_input(
            "tailTurns and fromIndex are mutually exclusive",
        )),
        (Some(n), None) => Ok(Some(TurnWindowReq::Tail(n))),
        (None, Some(k)) => Ok(Some(TurnWindowReq::FromIndex(k))),
        (None, None) => Ok(None),
    }
}

/// Slice a fully post-processed detail down to the requested window and stamp
/// the window metadata. MUST run after every pass that inspects or mutates the
/// full turn list (delegation meta, auto-title, in-flight stamping) — slicing
/// is strictly a serialization concern, so the windowed `turns` are identical
/// to the corresponding region of the full response.
fn apply_turn_window(
    detail: &mut DbConversationDetail,
    req: crate::commands::turn_window::TurnWindowReq,
) {
    use crate::commands::turn_window;
    let offset = turn_window::resolve_window_offset(&detail.turns, req);
    let meta = turn_window::window_meta(&detail.turns, offset);
    detail.turns.drain(..offset);
    detail.turns_offset = Some(meta.offset);
    detail.turns_total = Some(meta.total);
    detail.assistant_turns_before_offset = Some(meta.assistant_before);
    detail.prefix_hash = Some(meta.prefix_hash);
    detail.uncovered_prefix_max_ts = meta.uncovered_prefix_max_ts;
}

/// `get_folder_conversation_core` plus live in-flight correlation: when a turn is
/// currently running on the conversation's connection, stamp the persisted
/// in-flight user turn with the broadcast `message_id` so a cross-client viewer
/// dedups it against its synthesized copy, and report that turn's id on the detail
/// as `in_flight_user_turn_id` so the frontend can hide the partial assistant
/// reply persisted after it mid-stream. A no-op (one cheap lock pass) when no turn
/// is in flight. Shared by the Tauri command and the web handler.
///
/// `window`: when set, the response's `turns` are sliced to the requested
/// window AFTER all full-list post-processing (the summary counts, stats and
/// watermark keep describing the full transcript).
pub async fn get_folder_conversation_with_live_core(
    conn: &sea_orm::DatabaseConnection,
    manager: &crate::acp::manager::ConnectionManager,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    emitter: &EventEmitter,
    conversation_id: i32,
    window: Option<crate::commands::turn_window::TurnWindowReq>,
) -> Result<DbConversationDetail, AppCommandError> {
    let (mut detail, parsed_title) = get_folder_conversation_core(conn, conversation_id).await?;

    // Per-turn auto-title backfill. The parse `get_folder_conversation_core`
    // just did already produced the session-file title; adopt it (and broadcast
    // a sidebar upsert) whenever the user hasn't renamed this conversation by
    // hand. `refresh_auto_title` re-checks the lock and equality, so once the
    // title converges this becomes a cheap no-op on every later turn. The
    // pre-check here just avoids the extra DB round-trip in the common case.
    //
    // One upsert for the whole fetch: the title and the model can both land on
    // the same open, and the sidebar has no use for two broadcasts of the same
    // row a microsecond apart.
    let mut upserted = false;
    if !detail.summary.title_locked {
        if let Some(parsed) = parsed_title.as_deref().map(str::trim) {
            if !parsed.is_empty() && detail.summary.title.as_deref() != Some(parsed) {
                match conversation_service::refresh_auto_title(
                    conn,
                    conversation_id,
                    parsed.to_string(),
                )
                .await
                {
                    Ok(true) => {
                        detail.summary.title = Some(parsed.to_string());
                        upserted = true;
                        chat_channel_manager
                            .sync_conversation_title(conn, conversation_id, parsed)
                            .await;
                    }
                    Ok(false) => {}
                    Err(e) => tracing::error!(
                        "[conversations] auto-title refresh failed for {conversation_id}: {e}"
                    ),
                }
            }
        }
    }

    // Session-model backfill, the sibling of the auto-title above and for the
    // same reason: the row was inserted before any model was named, and the
    // sidebar reads the row rather than the transcript this parse just walked.
    // `seed_model_if_empty` re-checks emptiness in SQL, so once a session has a
    // model this is a no-op that writes nothing.
    if let Some(model) = detail.summary.model.clone() {
        match conversation_service::seed_model_if_empty(conn, conversation_id, &model).await {
            Ok(true) => upserted = true,
            Ok(false) => {}
            Err(e) => tracing::error!(
                "[conversations] session-model backfill failed for {conversation_id}: {e}"
            ),
        }
    }

    if upserted {
        emit_conversation_upsert(emitter, conn, conversation_id).await;
    }

    if let Some((pending, started_at)) = manager
        .pending_user_message_for_conversation(conversation_id)
        .await
    {
        detail.in_flight_user_turn_id =
            apply_in_flight_message_id(&mut detail.turns, &pending, started_at);
    }
    if let Some(req) = window {
        apply_turn_window(&mut detail, req);
    }
    Ok(detail)
}

/// One page of older history for the reverse-infinite-scroll path. Light
/// variant of the detail fetch: full parse + delegation-meta injection (both
/// happen inside `get_folder_conversation_core`), then a pure slice — no
/// auto-title refresh, no live correlation, no sidebar events.
pub async fn get_folder_conversation_turns_core(
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
    before_index: usize,
    limit: usize,
) -> Result<ConversationTurnsPage, AppCommandError> {
    use crate::commands::turn_window;
    let (detail, _parsed_title) = get_folder_conversation_core(conn, conversation_id).await?;
    let turns = detail.turns;
    let (start, end) = turn_window::resolve_page_bounds(&turns, before_index, limit);
    let meta = turn_window::window_meta(&turns, start);
    let seam = turn_window::window_meta(&turns, before_index.min(turns.len()));
    Ok(ConversationTurnsPage {
        turns: turns[start..end].to_vec(),
        turns_offset: meta.offset,
        turns_total: meta.total,
        assistant_turns_before_offset: meta.assistant_before,
        prefix_hash: meta.prefix_hash,
        prefix_hash_before_index: seam.prefix_hash,
        uncovered_prefix_max_ts: meta.uncovered_prefix_max_ts,
    })
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_folder_conversation(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    manager: tauri::State<'_, crate::acp::manager::ConnectionManager>,
    chat_channel_manager: tauri::State<'_, crate::chat_channel::manager::ChatChannelManager>,
    conversation_id: i32,
    tail_turns: Option<usize>,
    from_index: Option<usize>,
) -> Result<DbConversationDetail, AppCommandError> {
    let window = resolve_turn_window_req(tail_turns, from_index)?;
    get_folder_conversation_with_live_core(
        &db.conn,
        &manager,
        &chat_channel_manager,
        &EventEmitter::Tauri(app),
        conversation_id,
        window,
    )
    .await
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_folder_conversation_turns(
    db: tauri::State<'_, AppDatabase>,
    conversation_id: i32,
    before_index: usize,
    limit: usize,
) -> Result<ConversationTurnsPage, AppCommandError> {
    get_folder_conversation_turns_core(&db.conn, conversation_id, before_index, limit).await
}

/// Emit a `conversation://changed` Upsert for `conversation_id` so every
/// client's sidebar inserts-or-replaces the row in real time. Re-fetches the
/// fresh summary via `get_by_id`, which filters out soft-deleted rows — so an
/// upsert racing a delete is silently dropped (no row resurrection).
/// Best-effort: the DB write already succeeded; on fetch failure clients
/// reconcile on the next refresh / WS reconnect.
///
/// Lives at the wrapper layer (not inside the `_core` fns) so the many
/// internal/test callers of `create_conversation_core` don't fire sidebar
/// events, and so `_core` stays a pure DB primitive.
pub(crate) async fn emit_conversation_upsert(
    emitter: &EventEmitter,
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
) {
    match conversation_service::get_by_id(conn, conversation_id).await {
        Ok(summary) => {
            // Broadcast EVERY conversation, root or delegation child. The
            // sidebar's root list still drops children (the frontend keeps
            // `parent_id != null` out of its root array via `applyConversationUpsert`);
            // a separate subscriber routes child upserts into the expanded
            // sub-session subtree by `parent_id`. The summary carries `parent_id`
            // (serialized for children only) and a fresh `child_count`, so a
            // newly-spawned child can appear live and bump its parent's chevron.
            emit_event(
                emitter,
                CONVERSATION_CHANGED_EVENT,
                ConversationChange::Upsert {
                    summary: Box::new(summary),
                },
            )
        }
        Err(e) => tracing::warn!(
            "[conversations] upsert emit skipped (get_by_id {conversation_id} failed): {e}"
        ),
    }
}

/// Broadcast the row [`conversation_service::bind_external_id`] created to
/// preserve a session that was about to be orphaned.
///
/// Every caller of `bind_external_id` MUST route its `Some(..)` through this
/// (or emit an equivalent upsert itself). Creating the row is only half the
/// fix: the sidebar learns about a conversation it has never seen ONLY from a
/// `conversation://changed` upsert — otherwise not until a full reload. A
/// preserved row that is never broadcast therefore still looks, to the user,
/// exactly like the conversation vanishing, which is the bug being fixed.
///
/// `None` is the common case (an ordinary bind) and is a no-op.
pub(crate) async fn emit_preserved_conversation(
    emitter: &EventEmitter,
    conn: &sea_orm::DatabaseConnection,
    preserved: Option<i32>,
) {
    if let Some(preserved_id) = preserved {
        emit_conversation_upsert(emitter, conn, preserved_id).await;
    }
}

/// Push one conversation's CURRENT title to its bound chat threads, then
/// confirm it is still current — re-sending if it is not.
///
/// The confirmation loop is what makes a detached sync safe. A provider edit is
/// a remote call that can land arbitrarily late, so two syncs for the same
/// conversation can reach Telegram out of order: a stalled auto-title edit
/// completing AFTER a manual rename would leave the thread (and the binding's
/// `display_title`) named after a title the user already replaced, and nothing
/// would ever retry. Re-reading before every attempt also means the title is
/// never a stale snapshot captured at spawn time.
///
/// Deliberately NOT capped at N attempts. The loop exits only when the title it
/// just read equals the one it last SENT, so the last value it sent is always
/// the current one; any fixed cap reintroduces exactly the bug this closes
/// (rename → stall → rename → … exhausts the cap and exits stale).
///
/// The guarantee is about what was SENT, not about what the provider ended up
/// holding. `sync_conversation_title` reports nothing back, so an edit that the
/// provider rejected still counts as sent and is not retried here — on that
/// path the thread keeps its old name. That is the intended best-effort
/// contract, not an oversight: the DB has already converged and is the source
/// of truth, the channel layer logs the failure, and retrying a provider that
/// is down would turn a detached task into an unbounded remote-call loop over a
/// cosmetic thread title. It
/// cannot spin on its own: an iteration happens only when a NEW title was
/// observed, so it terminates as soon as renames stop, and each iteration is
/// rate-limited by one provider round-trip. Two concurrent syncs for the same
/// conversation converge on the same final value for the same reason.
///
/// Serializing per conversation would be the other way to get ordering, but the
/// lock would also be taken by the INLINE rename path
/// (`sync_conversation_title_to_channels_core`), which would then block a user's
/// rename for up to Telegram's 60s timeout behind a stalled background sync —
/// reintroducing the hang this whole path exists to avoid.
async fn sync_conversation_title_until_current(
    conn: &sea_orm::DatabaseConnection,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    conversation_id: i32,
) {
    let mut sent: Option<String> = None;
    loop {
        let summary = match conversation_service::get_by_id(conn, conversation_id).await {
            Ok(summary) => summary,
            Err(e) => {
                tracing::warn!(
                    "[conversations] chat-thread title sync stopped for {conversation_id} \
                     (get_by_id failed): {e}"
                );
                return;
            }
        };
        let Some(title) = summary.title else { return };
        if sent.as_deref() == Some(title.as_str()) {
            return;
        }
        chat_channel_manager
            .sync_conversation_title(conn, conversation_id, &title)
            .await;
        sent = Some(title);
    }
}

/// Detach a chat-channel title sync so a live title write cannot sit on
/// Telegram's 60s `editForumTopic` timeout. Callers that already upserted
/// the sidebar should use this rather than awaiting `sync_conversation_title`.
pub(crate) fn spawn_sync_conversation_title_until_current(
    conn: sea_orm::DatabaseConnection,
    chat_channel_manager: crate::chat_channel::manager::ChatChannelManager,
    conversation_id: i32,
) {
    tokio::spawn(async move {
        sync_conversation_title_until_current(&conn, &chat_channel_manager, conversation_id)
            .await;
    });
}

/// Broadcast and propagate title changes discovered outside codeg (for
/// example, Codex's session index or an import scan). Both operations are
/// best-effort: the database update has already committed, so notification
/// failures must not turn the originating list/scan request into an error.
///
/// The two halves are deliberately NOT symmetric:
///
/// * The sidebar upsert is emitted inline. It is DB-only and cheap, and the
///   response the caller is about to build must not disagree with what other
///   clients were just told.
/// * Chat-channel propagation is detached onto its own task, because it ends in
///   outbound HTTP (Telegram `editForumTopic`, a 60s per-request timeout, once
///   per bound thread). `list_all_conversations` is the sidebar's primary read
///   — also driven per-keystroke by the search and manage dialogs — so it must
///   never await a remote service. A slow or unreachable Telegram now costs a
///   late topic rename, not a hung conversation list.
///
/// The detached half deliberately re-reads each title rather than carrying the
/// one this notification was raised for — see
/// [`sync_conversation_title_until_current`].
///
/// Returns the detached task's handle so tests can join it; production callers
/// drop it (the work is best-effort and already logged on failure).
async fn notify_conversation_title_updates(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    conversation_ids: Vec<i32>,
) -> tokio::task::JoinHandle<()> {
    for conversation_id in &conversation_ids {
        emit_conversation_upsert(emitter, conn, *conversation_id).await;
    }

    let conn = conn.clone();
    let chat_channel_manager = chat_channel_manager.clone_ref();
    tokio::spawn(async move {
        for conversation_id in conversation_ids {
            sync_conversation_title_until_current(&conn, &chat_channel_manager, conversation_id)
                .await;
        }
    })
}

/// Emit a `conversation://changed` Deleted for `conversation_id` so every
/// client removes the row. No re-fetch: the row is already soft-deleted.
pub(crate) fn emit_conversation_deleted(emitter: &EventEmitter, conversation_id: i32) {
    emit_event(
        emitter,
        CONVERSATION_CHANGED_EVENT,
        ConversationChange::Deleted {
            id: conversation_id,
        },
    );
}

/// Broadcast a `tabs://changed` snapshot so every client converges its open-tab
/// set. `origin` is the originating client's id (echoed so it can ignore its own
/// change) or the sentinel `"server"` for cascade-originated changes that every
/// client applies.
pub(crate) fn emit_tabs_changed(
    emitter: &EventEmitter,
    version: i64,
    tabs: Vec<OpenedTab>,
    origin: String,
) {
    emit_event(
        emitter,
        TABS_CHANGED_EVENT,
        TabsChanged {
            version,
            origin,
            tabs,
        },
    );
}

/// Invalidate any open tabs pointing at a just-deleted conversation. Conversation
/// deletion is a SOFT delete, so the FK CASCADE never removes the tab row — we do
/// it explicitly. The tab version is ALWAYS advanced as a barrier (so a
/// concurrent stale save can't re-add a tab for the deleted conversation), but we
/// only broadcast when a persisted tab actually changed — a zero-row deletion
/// needs no broadcast (an in-flight saver reconciles via its rejected CAS). Lives
/// at the wrapper layer (not in `delete_conversation_core`) so internal/test
/// callers don't fire tab events.
pub(crate) async fn cleanup_tabs_for_deleted_conversation(
    emitter: &EventEmitter,
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
) {
    match tab_service::delete_conversation_tabs_and_bump(conn, conversation_id).await {
        Ok(inv) => {
            if let Some(tabs) = inv.emit {
                emit_tabs_changed(emitter, inv.version, tabs, "server".to_string());
            }
        }
        Err(e) => tracing::error!(
            "[conversations] tab cleanup failed (delete tabs for conversation {conversation_id}): {e}"
        ),
    }
}

/// Core logic for creating a conversation with git branch detection.
/// Shared by both the Tauri command and the web handler.
pub async fn create_conversation_core(
    conn: &sea_orm::DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    team: Option<crate::models::conversation::ConversationTeam>,
) -> Result<i32, AppCommandError> {
    let git_branch = if let Some(folder) = folder_service::get_folder_by_id(conn, folder_id)
        .await
        .map_err(AppCommandError::from)?
    {
        detect_git_branch(&folder.path).await
    } else {
        None
    };

    // [crewchat] A group conversation (team picked in the new-session dialog)
    // is the same row plus one JSON snapshot.
    let model = match team {
        Some(team) => {
            conversation_service::create_with_team(
                conn, folder_id, agent_type, title, git_branch, team,
            )
            .await
        }
        None => {
            conversation_service::create(conn, folder_id, agent_type, title, git_branch)
                .await
        }
    }
    .map_err(AppCommandError::from)?;
    Ok(model.id)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn create_conversation(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    team: Option<crate::models::conversation::ConversationTeam>,
) -> Result<i32, AppCommandError> {
    let id = create_conversation_core(&db.conn, folder_id, agent_type, title, team).await?;
    emit_conversation_upsert(&EventEmitter::Tauri(app), &db.conn, id).await;
    Ok(id)
}

/// Result of [`create_chat_conversation_core`]: the new conversation id plus the
/// hidden chat folder backing it, so the frontend can drop the folder straight
/// into `allFolders` (resolving cwd / active-folder) without a refetch.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatConversationResult {
    pub conversation_id: i32,
    pub folder_id: i32,
    pub folder: FolderDetail,
}

/// Result of [`create_chat_dir`]: the freshly created scratch directory path.
/// Handed to the frontend so a chat draft can point its ACP connection at a real
/// cwd *before* any conversation row exists.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatDirResult {
    pub path: String,
}

/// Create a fresh dated scratch directory for a chat-mode conversation and
/// return its absolute path. Mirrors Codex's date-grouped session dirs:
/// `<data_dir>/chat-sessions/<YYYY-MM-DD>/<uuid>/`.
///
/// This is a pure filesystem operation — it writes NO database rows — so it can
/// run eagerly the moment the user picks "no-folder mode" (giving the ACP
/// connection a cwd to spawn in) without breaching the lazy-conversation
/// invariant. The row-creating [`create_chat_conversation_core`] later reuses
/// this directory via its `existing_dir` parameter, so the connection's cwd
/// never moves across the first send.
pub fn create_chat_dir_core(data_dir: &std::path::Path) -> Result<String, AppCommandError> {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let dir = data_dir.join("chat-sessions").join(date).join(unique);
    std::fs::create_dir_all(&dir).map_err(AppCommandError::io)?;
    Ok(dir.to_string_lossy().to_string())
}

/// How long a scratch dir must have sat untouched before the GC may reclaim it.
/// Spares a directory that an in-flight chat draft in another window just minted
/// (it has no conversation row yet, so it would otherwise look orphaned).
const CHAT_SCRATCH_STALE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Layout-invariant key for a chat scratch dir: its trailing `(<date>, <uuid>)`
/// path components. The GC matches live dirs by this tail rather than the full
/// path string, so a different *spelling* of the same data_dir (e.g. a symlinked
/// vs canonical `CODEG_DATA_DIR` naming the same storage) still matches — a live
/// dir must never be misclassified as an orphan and deleted. `<uuid>` is a v4
/// UUID (globally unique), so the tail is collision-free in practice. Returns
/// `None` if the path lacks a leaf or parent component.
fn chat_dir_key(path: &std::path::Path) -> Option<(String, String)> {
    let uuid = path.file_name()?.to_string_lossy().to_string();
    let date = path.parent()?.file_name()?.to_string_lossy().to_string();
    Some((date, uuid))
}

/// Reclaim orphaned chat scratch directories under
/// `<data_dir>/chat-sessions/<date>/<uuid>/`. A chat draft eagerly mints a
/// scratch dir (see [`create_chat_dir_core`]) the moment "no-folder mode" is
/// picked, *before* any DB row exists; quitting before the first send — or
/// deleting a chat conversation, which intentionally leaves the dir on disk —
/// orphans it forever. This startup sweep removes the leak.
///
/// A `<uuid>` dir is reclaimed iff it is NOT bound to a live chat folder AND it
/// is older than [`CHAT_SCRATCH_STALE`]. "Live" excludes both pre-send drafts
/// (no row) and post-delete dirs (soft-deleted row), so both are reclaimed while
/// bound chats are spared. Returns the number of `<uuid>` dirs removed. Never
/// fatal: every filesystem error is logged and skipped.
pub async fn gc_orphan_chat_dirs_core(
    conn: &sea_orm::DatabaseConnection,
    data_dir: &std::path::Path,
) -> Result<usize, AppCommandError> {
    gc_orphan_chat_dirs_core_with_threshold(conn, data_dir, CHAT_SCRATCH_STALE).await
}

/// [`gc_orphan_chat_dirs_core`] with the staleness threshold injected, for tests.
/// A zero `stale` forces every dir to count as stale (deterministic, independent
/// of clock/mtime resolution); the production entry point always passes
/// [`CHAT_SCRATCH_STALE`].
pub(crate) async fn gc_orphan_chat_dirs_core_with_threshold(
    conn: &sea_orm::DatabaseConnection,
    data_dir: &std::path::Path,
    stale: std::time::Duration,
) -> Result<usize, AppCommandError> {
    let root = data_dir.join("chat-sessions");
    if !root.is_dir() {
        return Ok(0);
    }

    // Dirs bound to a live chat conversation, keyed by their layout-invariant
    // `(<date>, <uuid>)` tail (see `chat_dir_key`) rather than the full path
    // string. This survives a data_dir spelled differently across runs (e.g. a
    // symlinked vs canonical `CODEG_DATA_DIR` pointing at the same storage),
    // which a full-string compare would miss — misclassifying the live dir as an
    // orphan and deleting it. We deliberately do NOT canonicalize (it fails on
    // missing paths and could itself alias two distinct dirs); keying by the tail
    // makes the worst case a missed deletion (a leak), never data loss.
    let live: HashSet<(String, String)> = folder_service::list_live_chat_folder_paths(conn)
        .await
        .map_err(AppCommandError::from)?
        .iter()
        .filter_map(|p| chat_dir_key(std::path::Path::new(p)))
        .collect();

    let now = std::time::SystemTime::now();
    let mut removed = 0usize;

    let date_dirs = match std::fs::read_dir(&root) {
        Ok(rd) => rd,
        Err(err) => {
            tracing::error!(
                "[conversations] chat-dir GC: read {} failed: {err}",
                root.display()
            );
            return Ok(0);
        }
    };

    for date_entry in date_dirs.filter_map(Result::ok) {
        let date_path = date_entry.path();
        if !date_path.is_dir() {
            continue;
        }
        let date_key = match date_path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => continue,
        };
        let uuid_dirs = match std::fs::read_dir(&date_path) {
            Ok(rd) => rd,
            Err(err) => {
                tracing::error!(
                    "[conversations] chat-dir GC: read {} failed: {err}",
                    date_path.display()
                );
                continue;
            }
        };
        for uuid_entry in uuid_dirs.filter_map(Result::ok) {
            let uuid_path = uuid_entry.path();
            if !uuid_path.is_dir() {
                continue;
            }
            // Match by the layout-invariant `(<date>, <uuid>)` tail, not the full
            // path — see the `live` set above.
            let uuid_key = uuid_entry.file_name().to_string_lossy().to_string();
            if live.contains(&(date_key.clone(), uuid_key)) {
                continue;
            }
            // Old enough to reclaim? Unknown age (mtime unreadable / in the
            // future) → treat as fresh and spare it (a GC should leak before it
            // deletes something possibly in use). A zero threshold short-circuits
            // to "always stale" so tests don't race the filesystem clock.
            let stale_enough = stale.is_zero()
                || uuid_path
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .is_some_and(|age| age >= stale);
            if !stale_enough {
                continue;
            }
            match std::fs::remove_dir_all(&uuid_path) {
                Ok(()) => removed += 1,
                Err(err) => tracing::error!(
                    "[conversations] chat-dir GC: remove {} failed: {err}",
                    uuid_path.display()
                ),
            }
        }
        // Best-effort: drop the date bucket if it is now empty (`remove_dir` only
        // succeeds on an empty dir, so this never touches a bucket with survivors).
        let _ = std::fs::remove_dir(&date_path);
    }

    Ok(removed)
}

/// Core logic for creating a folderless "chat mode" conversation. Mirrors
/// Codex's date-grouped session dirs: each chat conversation gets its own
/// scratch directory under `<data_dir>/chat-sessions/<YYYY-MM-DD>/<uuid>/` plus a
/// dedicated hidden chat folder (`folder.kind = 'chat'`) pointing at it, so the
/// NOT-NULL `folder_id` FK stays satisfied. Called lazily on first prompt send — never before — so
/// merely selecting "no-folder mode" writes nothing to the DB. Shared by the
/// Tauri command and the web handler.
///
/// `existing_dir`: when the frontend already eagerly created a scratch dir (to
/// connect ACP before sending), pass it here so this reuses it instead of
/// minting a second one — keeping the connection's cwd put across the lazy
/// create. `None` mints a fresh dir (the send-before-dir-ready fallback).
/// `create_dir_all` is idempotent, so re-ensuring an existing dir is harmless.
pub async fn create_chat_conversation_core(
    conn: &sea_orm::DatabaseConnection,
    data_dir: &std::path::Path,
    agent_type: AgentType,
    title: Option<String>,
    existing_dir: Option<&str>,
) -> Result<CreateChatConversationResult, AppCommandError> {
    let path = match existing_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(AppCommandError::io)?;
            dir.to_string()
        }
        None => create_chat_dir_core(data_dir)?,
    };

    let folder = folder_service::add_chat_folder(conn, &path)
        .await
        .map_err(AppCommandError::from)?;

    // A fresh empty scratch dir has no git repo, so skip branch detection — this
    // also keeps the composer/top-bar branch pickers hidden in chat mode. No
    // transaction spans the folder + conversation inserts (the service calls take
    // a plain connection), so if the conversation insert fails, compensate by
    // soft-deleting the just-created hidden folder — otherwise it would linger as
    // an orphan (active, conversation-less, never reached by the delete path) and
    // pollute the active-folder scope.
    let model =
        match conversation_service::create_chat(conn, folder.id, agent_type, title, None).await {
            Ok(model) => model,
            Err(create_err) => {
                if let Err(cleanup_err) = folder_service::remove_folder(conn, &folder.path).await {
                    tracing::error!(
                        "[conversations] failed to clean up orphan chat folder {} after conversation create error: {cleanup_err}",
                        folder.id
                    );
                }
                return Err(AppCommandError::from(create_err));
            }
        };

    Ok(CreateChatConversationResult {
        conversation_id: model.id,
        folder_id: folder.id,
        folder,
    })
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn create_chat_conversation(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    agent_type: AgentType,
    title: Option<String>,
    existing_dir: Option<String>,
) -> Result<CreateChatConversationResult, AppCommandError> {
    use tauri::Manager;
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| crate::paths::resolve_effective_data_dir(&p))
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let result = create_chat_conversation_core(
        &db.conn,
        &data_dir,
        agent_type,
        title,
        existing_dir.as_deref(),
    )
    .await?;
    emit_conversation_upsert(&EventEmitter::Tauri(app), &db.conn, result.conversation_id).await;
    Ok(result)
}

/// Eagerly create a chat-mode scratch directory (no DB rows) and return its
/// path, so the frontend can connect ACP at a real cwd the instant the user
/// selects "no-folder mode" — before any first prompt. The hidden folder +
/// conversation are still created lazily on first send (reusing this dir).
#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn create_chat_dir(app: tauri::AppHandle) -> Result<CreateChatDirResult, AppCommandError> {
    use tauri::Manager;
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| crate::paths::resolve_effective_data_dir(&p))
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let path = create_chat_dir_core(&data_dir)?;
    Ok(CreateChatDirResult { path })
}

async fn detect_git_branch(path: &str) -> Option<String> {
    let output = crate::process::tokio_command("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(path)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return None;
    }
    Some(branch)
}

pub async fn update_conversation_status_core(
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
    status: String,
) -> Result<(), AppCommandError> {
    let status_enum: conversation::ConversationStatus =
        serde_json::from_value(serde_json::Value::String(status)).map_err(|e| {
            AppCommandError::invalid_input("Invalid conversation status").with_detail(e.to_string())
        })?;
    conversation_service::update_status(conn, conversation_id, status_enum)
        .await
        .map_err(AppCommandError::from)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn update_conversation_status(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    conversation_id: i32,
    status: String,
) -> Result<(), AppCommandError> {
    update_conversation_status_core(&db.conn, conversation_id, status).await?;
    emit_conversation_upsert(&EventEmitter::Tauri(app), &db.conn, conversation_id).await;
    Ok(())
}

pub async fn update_conversation_title_core(
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
    title: String,
) -> Result<(), AppCommandError> {
    conversation_service::update_title(conn, conversation_id, title)
        .await
        .map_err(AppCommandError::from)
}

/// Re-read the persisted conversation title and best-effort sync it to any
/// bound chat-channel threads (e.g. Telegram forum topics). Lives in
/// `commands/` so web handlers route through a `_core` helper instead of
/// calling the db service layer directly.
pub async fn sync_conversation_title_to_channels_core(
    conn: &sea_orm::DatabaseConnection,
    chat_channel_manager: &crate::chat_channel::manager::ChatChannelManager,
    conversation_id: i32,
) {
    if let Ok(conv) = conversation_service::get_by_id(conn, conversation_id).await {
        if let Some(title) = conv.title.as_deref() {
            chat_channel_manager
                .sync_conversation_title(conn, conversation_id, title)
                .await;
        }
    }
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn update_conversation_title(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    chat_channel_manager: tauri::State<'_, crate::chat_channel::manager::ChatChannelManager>,
    conversation_id: i32,
    title: String,
) -> Result<(), AppCommandError> {
    update_conversation_title_core(&db.conn, conversation_id, title).await?;
    emit_conversation_upsert(&EventEmitter::Tauri(app), &db.conn, conversation_id).await;
    sync_conversation_title_to_channels_core(&db.conn, &chat_channel_manager, conversation_id).await;
    Ok(())
}

pub async fn update_conversation_pinned_core(
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
    pinned: bool,
) -> Result<(), AppCommandError> {
    conversation_service::update_pin(conn, conversation_id, pinned)
        .await
        .map_err(AppCommandError::from)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn update_conversation_pinned(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    conversation_id: i32,
    pinned: bool,
) -> Result<(), AppCommandError> {
    update_conversation_pinned_core(&db.conn, conversation_id, pinned).await?;
    emit_conversation_upsert(&EventEmitter::Tauri(app), &db.conn, conversation_id).await;
    Ok(())
}

pub async fn delete_conversation_core(
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
) -> Result<(), AppCommandError> {
    conversation_service::soft_delete(conn, conversation_id)
        .await
        .map_err(AppCommandError::from)
}

/// When the deleted conversation was backed by a dedicated hidden chat folder,
/// soft-delete that folder too so it stops counting toward `list_all`'s active
/// folder scope. The per-conversation scratch dir on disk is intentionally left
/// in place (symmetric with conversation soft-delete keeping session files; a
/// future GC can prune dirs whose folder is soft-deleted). Best effort —
/// failures are logged, never propagated. `folder_id` must be captured BEFORE
/// the conversation soft-delete.
pub async fn cleanup_chat_folder_for_deleted_conversation(
    conn: &sea_orm::DatabaseConnection,
    folder_id: i32,
) {
    match folder_service::get_folder_by_id(conn, folder_id).await {
        Ok(Some(folder)) if folder.kind == FolderKind::Chat => {
            // Only retire the hidden folder once it backs no remaining
            // (non-deleted) conversations, so deleting one chat conversation can
            // never hide another that happens to share the folder. (Normally a
            // chat folder backs exactly one conversation, but this keeps the
            // delete path safe regardless.)
            match conversation_service::list_by_folder(conn, folder_id, None, None, None, None).await
            {
                Ok(remaining) if remaining.is_empty() => {
                    if let Err(e) = folder_service::remove_folder(conn, &folder.path).await {
                        tracing::error!(
                            "[conversations] chat folder cleanup failed (folder {folder_id}): {e}"
                        );
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::error!(
                    "[conversations] chat folder conversation check failed (folder {folder_id}): {e}"
                ),
            }
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!("[conversations] chat folder lookup failed (folder {folder_id}): {e}")
        }
    }
}

/// Full conversation-delete orchestration shared by the Tauri command and the web
/// handler: capture the backing folder BEFORE the soft-delete (so a hidden chat
/// folder can be retired afterward), soft-delete, broadcast the deletion, then run
/// the tab + chat-folder cleanups. The thin `delete_conversation_core` primitive
/// stays event-free for internal/test callers, so the orchestration lives here.
pub async fn delete_conversation_with_cleanup_core(
    emitter: &EventEmitter,
    conn: &sea_orm::DatabaseConnection,
    conversation_id: i32,
) -> Result<(), AppCommandError> {
    // Capture the backing folder AND parent before the soft-delete: a hidden
    // chat folder is retired afterward, and a deleted delegation child must
    // re-broadcast its parent so the parent's child_count (hence its chevron)
    // converges from the DB aggregate.
    let pre = conversation_service::get_by_id(conn, conversation_id)
        .await
        .ok();
    let folder_id = pre.as_ref().map(|c| c.folder_id);
    let parent_id = pre.as_ref().and_then(|c| c.parent_id);
    delete_conversation_core(conn, conversation_id).await?;
    emit_conversation_deleted(emitter, conversation_id);
    // A removed delegation child drops its parent's child_count (→ 0 hides the
    // chevron). Re-emit the parent from the authoritative aggregate so every
    // client converges — symmetric with the create-time parent re-emit.
    if let Some(parent_id) = parent_id {
        emit_conversation_upsert(emitter, conn, parent_id).await;
    }
    cleanup_tabs_for_deleted_conversation(emitter, conn, conversation_id).await;
    // Canvas references (pinned cards, custom-region memberships) survive the
    // soft delete for the same reason tabs do — no FK cascade ever fires — so
    // they get the same explicit scrub, at the same funnel.
    crate::commands::canvas::cleanup_canvas_for_deleted_conversation(emitter, conn, conversation_id)
        .await;
    if let Some(folder_id) = folder_id {
        cleanup_chat_folder_for_deleted_conversation(conn, folder_id).await;
    }
    Ok(())
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn delete_conversation(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    conversation_id: i32,
) -> Result<(), AppCommandError> {
    let emitter = EventEmitter::Tauri(app);
    delete_conversation_with_cleanup_core(&emitter, &db.conn, conversation_id).await
}

fn compute_stats(all_conversations: &[ConversationSummary]) -> AgentStats {
    let mut total_messages: u32 = 0;
    let mut counts: HashMap<AgentType, u32> = HashMap::new();

    for conversation in all_conversations {
        total_messages += conversation.message_count;
        *counts.entry(conversation.agent_type).or_insert(0) += 1;
    }

    let mut by_agent: Vec<AgentConversationCount> = counts
        .into_iter()
        .map(|(agent_type, conversation_count)| AgentConversationCount {
            agent_type,
            conversation_count,
        })
        .collect();
    by_agent.sort_by_key(|b| std::cmp::Reverse(b.conversation_count));

    AgentStats {
        total_conversations: all_conversations.len() as u32,
        total_messages,
        by_agent,
    }
}

fn parse_error_to_app_error(error: ParseError) -> AppCommandError {
    match error {
        ParseError::ConversationNotFound(id) => {
            AppCommandError::not_found("Conversation not found").with_detail(id)
        }
        ParseError::InvalidData(message) => {
            AppCommandError::invalid_input("Invalid conversation data").with_detail(message)
        }
        ParseError::Io(err) => AppCommandError::io(err),
        ParseError::Json(err) => {
            AppCommandError::invalid_input("Failed to parse conversation file")
                .with_detail(err.to_string())
        }
        ParseError::Db(err) => AppCommandError::database_error("Database operation failed")
            .with_detail(err.to_string()),
    }
}

#[cfg(test)]
mod tests;
