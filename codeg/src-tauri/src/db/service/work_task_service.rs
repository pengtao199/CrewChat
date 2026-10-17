//! Work-task CRUD + state machine. Mode-agnostic: every fn takes a plain
//! `&DatabaseConnection` so Tauri commands, Axum handlers, and the task engine
//! share one code path.
//!
//! Invariants enforced here:
//! - Every status transition is a conditional UPDATE (CAS) on the expected
//!   status — and, for engine-driven transitions, the current `run_seq` — so a
//!   canceled/stale generation's late events are zero-side-effect no-ops.
//! - Every transition writes its `work_task_event` row in the SAME transaction
//!   (and the CAS UPDATE is the transaction's first statement, so SQLite takes
//!   the write lock up front — see the busy-snapshot pitfall).
//! - `done` is written only by `merge_landed` (a landed merge) and
//!   `complete_without_merge` (a reviewed task with nothing to land), and never
//!   rolls back; a failed worktree cleanup surfaces as `cleanup_state='failed'`
//!   on the done row.

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};

use crate::db::entities::work_task::WorkTaskStatus;
use crate::db::entities::{folder, work_task, work_task_event, work_task_settings, work_task_template};
use crate::db::error::DbError;
use crate::models::{
    WorkTaskConfig, WorkTaskDraft, WorkTaskEventInfo, WorkTaskFolderSettings, WorkTaskInfo,
    WorkTaskMergeState, WorkTaskQueuedMerge,
};

// `WorkTaskPreflight` is referenced via `crate::models::` in its fns to keep
// this import list stable.

pub fn status_str(s: WorkTaskStatus) -> &'static str {
    match s {
        WorkTaskStatus::Todo => "todo",
        WorkTaskStatus::Queued => "queued",
        WorkTaskStatus::Preparing => "preparing",
        WorkTaskStatus::Running => "running",
        WorkTaskStatus::AwaitingInput => "awaiting_input",
        WorkTaskStatus::Review => "review",
        WorkTaskStatus::Merging => "merging",
        WorkTaskStatus::Done => "done",
        WorkTaskStatus::Failed => "failed",
        WorkTaskStatus::Canceled => "canceled",
    }
}

/// Decode the parked merge intent of a row. Tolerant on purpose: the column
/// once held a different (long-dead) shape, and an undecodable value means
/// "nothing queued" rather than an error the board would have to render.
pub fn queued_merge(pending_merge: Option<&str>) -> Option<WorkTaskQueuedMerge> {
    pending_merge.and_then(|s| serde_json::from_str::<WorkTaskQueuedMerge>(s).ok())
}

fn to_info(m: work_task::Model) -> WorkTaskInfo {
    WorkTaskInfo {
        id: m.id,
        folder_id: m.folder_id,
        title: m.title,
        config: serde_json::from_str(&m.config).unwrap_or(serde_json::Value::Null),
        status: m.status,
        failure_reason: m.failure_reason,
        last_error: m.last_error,
        run_seq: m.run_seq,
        sort_order: m.sort_order,
        worktree_folder_id: m.worktree_folder_id,
        worktree_missing: false, // stamped by the command layer (needs disk + folder rows)
        agent_type: None,        // stamped by the command layer (needs folder settings)
        conversation_id: m.conversation_id,
        connection_id: m.connection_id,
        base_branch: m.base_branch,
        base_sha: m.base_sha,
        work_branch: m.work_branch,
        cleanup_state: m.cleanup_state,
        verdict: m.verdict,
        result_summary: m.result_summary,
        files_changed: m.files_changed,
        additions: m.additions,
        deletions: m.deletions,
        merge_commit: m.merge_commit,
        completion_kind: m.completion_kind,
        preflight: m
            .preflight
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok()),
        merge_queued: queued_merge(m.pending_merge.as_deref()),
        archived_at: m.archived_at,
        scheduled_at: m.scheduled_at,
        source_kind: m.source_kind,
        source_key: m.source_key,
        source_meta: m
            .source_meta
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        latest_progress: None,
        compacting: false,
        created_at: m.created_at,
        updated_at: m.updated_at,
        started_at: m.started_at,
        settled_at: m.settled_at,
        finished_at: m.finished_at,
    }
}

fn event_to_info(m: work_task_event::Model) -> WorkTaskEventInfo {
    WorkTaskEventInfo {
        id: m.id,
        task_id: m.task_id,
        kind: m.kind,
        actor: m.actor,
        payload: m
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok()),
        created_at: m.created_at,
    }
}

/// Append a timeline event. Callers inside a transition pass the open `txn` so
/// the event commits atomically with the state change.
pub async fn record_event<C: ConnectionTrait>(
    conn: &C,
    task_id: i32,
    kind: &str,
    actor: &str,
    payload: Option<serde_json::Value>,
) -> Result<(), DbError> {
    let active = work_task_event::ActiveModel {
        id: NotSet,
        task_id: Set(task_id),
        kind: Set(kind.to_string()),
        actor: Set(actor.to_string()),
        payload: Set(payload.map(|p| p.to_string())),
        created_at: Set(Utc::now()),
    };
    active.insert(conn).await?;
    Ok(())
}

async fn status_changed_event<C: ConnectionTrait>(
    conn: &C,
    task_id: i32,
    actor: &str,
    from: Option<WorkTaskStatus>,
    to: WorkTaskStatus,
    extra: Option<serde_json::Value>,
) -> Result<(), DbError> {
    let mut payload = serde_json::json!({ "to": status_str(to) });
    if let Some(from) = from {
        payload["from"] = serde_json::Value::String(status_str(from).to_string());
    }
    if let Some(serde_json::Value::Object(extra)) = extra {
        for (k, v) in extra {
            payload[k] = v;
        }
    }
    record_event(conn, task_id, "status_changed", actor, Some(payload)).await
}

// ── queries ─────────────────────────────────────────────────────────────────

/// Active tasks (optionally per folder), joined on the live folder so tasks of
/// a removed folder are invisible and unschedulable. Board order: sort_order.
pub async fn list(
    conn: &DatabaseConnection,
    folder_id: Option<i32>,
) -> Result<Vec<WorkTaskInfo>, DbError> {
    let mut q = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .inner_join(folder::Entity)
        .filter(folder::Column::DeletedAt.is_null());
    if let Some(fid) = folder_id {
        q = q.filter(work_task::Column::FolderId.eq(fid));
    }
    let rows = q
        .order_by_asc(work_task::Column::SortOrder)
        .order_by_asc(work_task::Column::Id)
        .all(conn)
        .await?;
    let mut infos: Vec<WorkTaskInfo> = rows.into_iter().map(to_info).collect();

    // What a live card says it is doing: the generation's latest
    // `agent_progress` milestone, and whether it is currently parked on a
    // pre-prompt context compaction. Both come from the same sweep.
    //
    // `preparing` is in the set because a round that resumes a session spends
    // that status on a real agent turn — the compaction, which on a full
    // context window runs for minutes with nothing else to show for it.
    let live_ids: Vec<i32> = infos
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                WorkTaskStatus::Preparing
                    | WorkTaskStatus::Running
                    | WorkTaskStatus::AwaitingInput
                    | WorkTaskStatus::Merging
            )
        })
        .map(|t| t.id)
        .collect();
    if !live_ids.is_empty() {
        let events = work_task_event::Entity::find()
            .filter(work_task_event::Column::TaskId.is_in(live_ids))
            .filter(
                work_task_event::Column::Kind.is_in(["agent_progress", "context_compact"]),
            )
            .order_by_asc(work_task_event::Column::Id)
            .all(conn)
            .await?;
        // Both readings are GENERATION-SCOPED, on the `run_seq` each event
        // carries. A retry, a follow-up and a merge each bump `run_seq`, and
        // without this the card kept showing the PREVIOUS round's last
        // milestone — a merge in flight narrating the work round it is landing.
        // An event from before this was stamped carries no `run_seq` and counts
        // for no generation; only currently-live tasks lose anything by that,
        // and only until their next milestone.
        let mut latest: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
        let mut compacting: std::collections::HashMap<i32, bool> =
            std::collections::HashMap::new();
        let by_id: std::collections::HashMap<i32, i32> =
            infos.iter().map(|t| (t.id, t.run_seq)).collect();
        for e in events {
            let payload = e
                .payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
            let Some(payload) = payload else { continue };
            let of_this_run = payload
                .get("run_seq")
                .and_then(serde_json::Value::as_i64)
                .zip(by_id.get(&e.task_id))
                .is_some_and(|(seq, run_seq)| seq == *run_seq as i64);
            if !of_this_run {
                continue;
            }
            match e.kind.as_str() {
                "agent_progress" => {
                    if let Some(m) = payload.get("message").and_then(|m| m.as_str()) {
                        latest.insert(e.task_id, m.to_string()); // ascending id — last wins
                    }
                }
                // A compaction announces itself when the turn goes out and
                // again when it lands, so the LAST word on this generation is
                // the answer: `started` with nothing after it is a turn still
                // running.
                //
                // Two ways to be left holding a `started` that is over, both
                // deliberately unguarded. A launch killed mid-compaction leaves
                // one, but its row leaves this set before anything reads it
                // (requeued, failed, or bounced back to review). And the events
                // are best-effort writes, so a lost outcome insert strands the
                // flag for the rest of that generation. Neither is worth a
                // guard: the cost is one italic line claiming work that has
                // finished, on a card whose status is still telling the truth.
                "context_compact" => {
                    let started =
                        payload.get("status").and_then(|s| s.as_str()) == Some("started");
                    compacting.insert(e.task_id, started);
                }
                _ => {}
            }
        }
        for t in &mut infos {
            if let Some(m) = latest.get(&t.id) {
                t.latest_progress = Some(m.clone());
            }
            t.compacting = compacting.get(&t.id).copied().unwrap_or(false);
        }
    }
    Ok(infos)
}

pub async fn get(conn: &DatabaseConnection, id: i32) -> Result<WorkTaskInfo, DbError> {
    Ok(to_info(get_model(conn, id).await?))
}

/// Raw row (includes internal fields like `connection_id` / `merge_state`) for
/// the engine. Errors on missing or soft-deleted rows.
pub async fn get_model(conn: &DatabaseConnection, id: i32) -> Result<work_task::Model, DbError> {
    let row = work_task::Entity::find_by_id(id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::NotFound(format!("work task {id}")))?;
    if row.deleted_at.is_some() {
        return Err(DbError::NotFound(format!("work task {id}")));
    }
    Ok(row)
}

pub async fn list_events(
    conn: &DatabaseConnection,
    task_id: i32,
    limit: u64,
) -> Result<Vec<WorkTaskEventInfo>, DbError> {
    let rows = work_task_event::Entity::find()
        .filter(work_task_event::Column::TaskId.eq(task_id))
        .order_by_asc(work_task_event::Column::CreatedAt)
        .order_by_asc(work_task_event::Column::Id)
        .limit(limit)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(event_to_info).collect())
}

/// The task's most recent events of the given kinds, newest first.
///
/// Two departures from `list_events`, both required by anything reasoning about
/// the latest state rather than rendering a timeline:
/// - **newest first.** `list_events` orders ASCENDING before applying its
///   limit, so past `limit` events it returns the task's OLDEST rows and stops
///   seeing recent ones entirely.
/// - **by id, not timestamp.** The log is append-only with an autoincrement
///   key, so id IS insertion order; `created_at` comes from the wall clock,
///   which can tie within a transaction and can step backwards under an NTP
///   correction.
///
/// `kinds` narrows the query rather than the result: filtering after the fact
/// would let a burst of one kind (an agent reporting progress, say) push the
/// rows the caller cares about past the limit.
pub async fn recent_events_of_kinds(
    conn: &DatabaseConnection,
    task_id: i32,
    kinds: &[&str],
    limit: u64,
) -> Result<Vec<WorkTaskEventInfo>, DbError> {
    let rows = work_task_event::Entity::find()
        .filter(work_task_event::Column::TaskId.eq(task_id))
        .filter(work_task_event::Column::Kind.is_in(kinds.iter().copied()))
        .order_by_desc(work_task_event::Column::Id)
        .limit(limit)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(event_to_info).collect())
}

/// Raw rows in the given statuses. Deliberately does NOT join the folder: the
/// reconcile sweep must also converge tasks whose folder was removed mid-run.
pub async fn list_by_status(
    conn: &DatabaseConnection,
    statuses: &[WorkTaskStatus],
) -> Result<Vec<work_task::Model>, DbError> {
    let rows = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::Status.is_in(statuses.iter().copied()))
        .all(conn)
        .await?;
    Ok(rows)
}

/// Ids of all todo tasks of a folder, in board order (for "start all").
///
/// Tasks with a planned start are left out: "process all" is a bulk shortcut,
/// and silently discarding a time the user picked for one particular task would
/// start an agent earlier than they asked. That task's own start button (or a
/// drag onto the In-progress column) still overrides the plan explicitly.
pub async fn list_todo_ids(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<Vec<i32>, DbError> {
    let rows = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::FolderId.eq(folder_id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Todo))
        .filter(work_task::Column::ScheduledAt.is_null())
        .order_by_asc(work_task::Column::SortOrder)
        .order_by_asc(work_task::Column::Id)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(|m| m.id).collect())
}

/// Next queued task of a folder in board order (the engine's launch pump picks
/// from here while under the folder's pump lock). Joined on the live folder.
pub async fn next_queued(
    conn: &DatabaseConnection,
    folder_id: i32,
    exclude: &[i32],
) -> Result<Option<work_task::Model>, DbError> {
    let mut q = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::FolderId.eq(folder_id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Queued))
        .inner_join(folder::Entity)
        .filter(folder::Column::DeletedAt.is_null());
    if !exclude.is_empty() {
        q = q.filter(work_task::Column::Id.is_not_in(exclude.iter().copied()));
    }
    Ok(q.order_by_asc(work_task::Column::SortOrder)
        .order_by_asc(work_task::Column::Id)
        .one(conn)
        .await?)
}

/// Distinct folder ids that currently have pending (todo or queued) tasks over
/// live folders — drives the reconcile tick's pump sweep. Todo is included so
/// auto_process folders get their scheduler pass even when nothing is queued;
/// the pump itself checks the folder's auto flag.
pub async fn folders_with_pending(conn: &DatabaseConnection) -> Result<Vec<i32>, DbError> {
    let rows = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(
            work_task::Column::Status.is_in([WorkTaskStatus::Todo, WorkTaskStatus::Queued]),
        )
        .inner_join(folder::Entity)
        .filter(folder::Column::DeletedAt.is_null())
        .all(conn)
        .await?;
    let mut ids: Vec<i32> = rows.into_iter().map(|m| m.folder_id).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Folders that currently hold todo tasks — the "start all" fan-out set when
/// no folder is selected.
pub async fn folders_with_todos(conn: &DatabaseConnection) -> Result<Vec<i32>, DbError> {
    let rows = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Todo))
        .inner_join(folder::Entity)
        .filter(folder::Column::DeletedAt.is_null())
        .all(conn)
        .await?;
    let mut ids: Vec<i32> = rows.into_iter().map(|m| m.folder_id).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Launched-and-active count per folder (running + awaiting_input + merging).
/// `queued` is deliberately excluded — it is the waiting room the pump drains.
pub async fn active_launched_count(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<u64, DbError> {
    let count = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::FolderId.eq(folder_id))
        .filter(work_task::Column::Status.is_in([
            WorkTaskStatus::Running,
            WorkTaskStatus::AwaitingInput,
            WorkTaskStatus::Merging,
        ]))
        .count(conn)
        .await?;
    Ok(count)
}

/// Tasks needing the user ("等你处理"): awaiting_input + review + failed, over
/// live folders. Drives the sidebar badge.
pub async fn attention_count(conn: &DatabaseConnection) -> Result<u64, DbError> {
    let count = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::Status.is_in([
            WorkTaskStatus::AwaitingInput,
            WorkTaskStatus::Review,
            WorkTaskStatus::Failed,
        ]))
        .filter(work_task::Column::ArchivedAt.is_null())
        .inner_join(folder::Entity)
        .filter(folder::Column::DeletedAt.is_null())
        .count(conn)
        .await?;
    Ok(count)
}

// ── CRUD ────────────────────────────────────────────────────────────────────

fn parse_config(config: &serde_json::Value) -> WorkTaskConfig {
    serde_json::from_value(config.clone()).unwrap_or_default()
}

fn validate_draft(draft: &WorkTaskDraft) -> Result<(), DbError> {
    if draft.title.trim().is_empty() {
        return Err(DbError::Validation("title is required".into()));
    }
    let cfg = parse_config(&draft.config);
    if cfg.display_text.trim().is_empty() && cfg.prompt_blocks.is_empty() {
        return Err(DbError::Validation("prompt is required".into()));
    }
    Ok(())
}

pub async fn create(
    conn: &DatabaseConnection,
    draft: WorkTaskDraft,
) -> Result<WorkTaskInfo, DbError> {
    validate_draft(&draft)?;
    // The target folder must exist, be live, and be a project root (a task
    // bound to a worktree folder would nest worktrees at launch).
    let folder = folder::Entity::find_by_id(draft.folder_id)
        .one(conn)
        .await?
        .filter(|f| f.deleted_at.is_none())
        .ok_or_else(|| DbError::NotFound(format!("folder {}", draft.folder_id)))?;
    if folder.parent_id.is_some() {
        return Err(DbError::Validation(
            "tasks must target a project folder, not a worktree".into(),
        ));
    }
    let config_str = serde_json::to_string(&draft.config)
        .map_err(|e| DbError::Validation(format!("config not serializable: {e}")))?;
    let now = Utc::now();
    let max_order = work_task::Entity::find()
        .filter(work_task::Column::FolderId.eq(draft.folder_id))
        .order_by_desc(work_task::Column::SortOrder)
        .one(conn)
        .await?
        .map(|m| m.sort_order)
        .unwrap_or(0);

    let txn = conn.begin().await?;
    let row = insert_todo_row(&txn, &draft, config_str, max_order, now, None).await?;
    record_event(&txn, row.id, "created", "user", None).await?;
    txn.commit().await?;
    Ok(to_info(row))
}

/// Outcome of a forge-triggered create: either the new task, or the ACTIVE
/// task that already handles the same source_key (dedup hit, not an error —
/// the UI offers "view it" / "create anyway").
#[derive(Debug)]
pub enum ForgeCreateOutcome {
    Created(WorkTaskInfo),
    Duplicate(WorkTaskInfo),
}

/// Create a forge-triggered task. This is the ONLY path that writes the
/// source columns — the public `create` (DTO-driven) cannot mint provenance,
/// so a forged client draft can never bypass the trigger command's repo
/// validation and untrusted-data envelope.
///
/// Concurrency shape (the repo's write-first idiom, same reason as
/// `auto_claim_next`): the INSERT is the transaction's FIRST statement, so the
/// SQLite write lock is taken up front and a concurrent trigger simply waits —
/// a dedup SELECT before the first write would make this a deferred
/// read-then-write transaction, and under WAL the loser of that upgrade gets
/// `database is locked` instead of the promised `Duplicate` answer. The dedup
/// check then runs AFTER the insert, inside the same lock, and rolls the fresh
/// row back on a hit. `max_order` is read outside the transaction exactly like
/// `create` does — a stale value only affects board ordering, never
/// correctness.
pub async fn create_from_forge(
    conn: &DatabaseConnection,
    draft: WorkTaskDraft,
    source: crate::models::WorkTaskSource,
    force: bool,
) -> Result<ForgeCreateOutcome, DbError> {
    validate_draft(&draft)?;
    let folder = folder::Entity::find_by_id(draft.folder_id)
        .one(conn)
        .await?
        .filter(|f| f.deleted_at.is_none())
        .ok_or_else(|| DbError::NotFound(format!("folder {}", draft.folder_id)))?;
    if folder.parent_id.is_some() {
        return Err(DbError::Validation(
            "tasks must target a project folder, not a worktree".into(),
        ));
    }
    let config_str = serde_json::to_string(&draft.config)
        .map_err(|e| DbError::Validation(format!("config not serializable: {e}")))?;
    let now = Utc::now();
    let max_order = work_task::Entity::find()
        .filter(work_task::Column::FolderId.eq(draft.folder_id))
        .order_by_desc(work_task::Column::SortOrder)
        .one(conn)
        .await?
        .map(|m| m.sort_order)
        .unwrap_or(0);

    let txn = conn.begin().await?;
    // FIRST statement: the write. See the doc comment — this is load-bearing.
    let row = insert_todo_row(&txn, &draft, config_str, max_order, now, Some(&source)).await?;
    if !force {
        if let Some(existing) =
            other_active_with_same_source(&txn, row.id, &source.key).await?
        {
            txn.rollback().await?;
            return Ok(ForgeCreateOutcome::Duplicate(to_info(existing)));
        }
    }
    record_event(&txn, row.id, "created", "user", None).await?;
    record_event(
        &txn,
        row.id,
        "forge_linked",
        "user",
        Some(serde_json::json!({ "source_key": source.key, "kind": source.kind })),
    )
    .await?;
    txn.commit().await?;
    Ok(ForgeCreateOutcome::Created(to_info(row)))
}

/// Every non-terminal status — the single definition of "active" shared by
/// the create dedup and the resurrection guards, so the two can never drift.
pub const ACTIVE_STATUSES: [WorkTaskStatus; 7] = [
    WorkTaskStatus::Todo,
    WorkTaskStatus::Queued,
    WorkTaskStatus::Preparing,
    WorkTaskStatus::Running,
    WorkTaskStatus::AwaitingInput,
    WorkTaskStatus::Review,
    WorkTaskStatus::Merging,
];

/// The wire-detectable error of a blocked resurrection: the frontend matches
/// on the `duplicate_active_source` marker to offer "re-open anyway".
fn duplicate_active_source_error(other: &work_task::Model) -> DbError {
    DbError::Validation(format!(
        "duplicate_active_source: task #{} ({}) is already active for this work item",
        other.id, other.title
    ))
}

/// Resurrection guard: does ANOTHER active task share this task's source_key?
/// Called (inside the claiming/requeue transaction) before a failed/canceled
/// forge task returns to the active set — without it, "trigger a replacement,
/// then requeue the old card" silently ends with two live tasks on one issue.
pub async fn other_active_with_same_source<C: ConnectionTrait>(
    conn: &C,
    task_id: i32,
    source_key: &str,
) -> Result<Option<work_task::Model>, DbError> {
    Ok(work_task::Entity::find()
        .filter(work_task::Column::SourceKey.eq(source_key))
        .filter(work_task::Column::Id.ne(task_id))
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::Status.is_in(ACTIVE_STATUSES))
        .order_by_desc(work_task::Column::CreatedAt)
        .one(conn)
        .await?)
}

/// Latest task row (any state, newest `created_at` per key) for each of the
/// given source keys — the workbench's reverse lookup for its visible rows.
/// One indexed query; missing keys simply have no entry.
pub async fn lookup_latest_by_source_keys(
    conn: &DatabaseConnection,
    keys: &[String],
) -> Result<Vec<(String, work_task::Model)>, DbError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let rows = work_task::Entity::find()
        .filter(work_task::Column::SourceKey.is_in(keys.iter().cloned()))
        .filter(work_task::Column::DeletedAt.is_null())
        .order_by_desc(work_task::Column::CreatedAt)
        .order_by_desc(work_task::Column::Id)
        .all(conn)
        .await?;
    let mut seen = std::collections::HashSet::new();
    Ok(rows
        .into_iter()
        .filter_map(|m| {
            let key = m.source_key.clone()?;
            seen.insert(key.clone()).then_some((key, m))
        })
        .collect())
}

async fn insert_todo_row<C: ConnectionTrait>(
    txn: &C,
    draft: &WorkTaskDraft,
    config_str: String,
    max_order: i32,
    now: chrono::DateTime<Utc>,
    source: Option<&crate::models::WorkTaskSource>,
) -> Result<work_task::Model, DbError> {
    let source_meta = source
        .map(|s| {
            serde_json::to_string(&s.meta)
                .map_err(|e| DbError::Validation(format!("source meta not serializable: {e}")))
        })
        .transpose()?;
    let active = work_task::ActiveModel {
        id: NotSet,
        folder_id: Set(draft.folder_id),
        title: Set(draft.title.trim().to_string()),
        config: Set(config_str),
        status: Set(WorkTaskStatus::Todo),
        failure_reason: Set(None),
        last_error: Set(None),
        run_seq: Set(0),
        sort_order: Set(max_order + 1),
        worktree_folder_id: Set(None),
        conversation_id: Set(None),
        connection_id: Set(None),
        base_branch: Set(None),
        base_sha: Set(None),
        work_branch: Set(None),
        merge_state: Set(None),
        pending_merge: Set(None),
        cleanup_state: Set(None),
        verdict: Set(None),
        result_summary: Set(None),
        files_changed: Set(None),
        additions: Set(None),
        deletions: Set(None),
        merge_commit: Set(None),
        completion_kind: Set(None),
        preflight: Set(None),
        archived_at: Set(None),
        scheduled_at: Set(None),
        source_kind: Set(source.map(|s| s.kind.clone())),
        source_key: Set(source.map(|s| s.key.clone())),
        source_meta: Set(source_meta),
        created_at: Set(now),
        updated_at: Set(now),
        started_at: Set(None),
        settled_at: Set(None),
        finished_at: Set(None),
        deleted_at: Set(None),
    };
    Ok(active.insert(txn).await?)
}

/// Edit title/config. Only meaningful outside an active run: allowed in
/// todo / failed / canceled. Moving to another folder is allowed only for a
/// pristine todo (no worktree minted yet).
pub async fn update(
    conn: &DatabaseConnection,
    id: i32,
    draft: WorkTaskDraft,
) -> Result<WorkTaskInfo, DbError> {
    validate_draft(&draft)?;
    let row = get_model(conn, id).await?;
    if !matches!(
        row.status,
        WorkTaskStatus::Todo | WorkTaskStatus::Failed | WorkTaskStatus::Canceled
    ) {
        return Err(DbError::Validation(
            "only todo, failed, or canceled tasks can be edited".into(),
        ));
    }
    if draft.folder_id != row.folder_id
        && (row.status != WorkTaskStatus::Todo || row.worktree_folder_id.is_some())
    {
        return Err(DbError::Validation(
            "a task that already ran cannot move to another folder".into(),
        ));
    }
    // A forge-sourced task is pinned to the folder whose git remote was
    // validated against the source repository at trigger time; moving it —
    // even as a pristine todo — would void that check and run an issue's
    // prompt against an unrelated repo. Re-trigger in the right folder instead.
    if draft.folder_id != row.folder_id && row.source_kind.is_some() {
        return Err(DbError::Validation(
            "a forge-sourced task cannot move to another folder; trigger it again from the issue instead".into(),
        ));
    }
    // `deliverable` is trigger-owned (a forge scenario stamps it; no editor
    // shows or edits it), and the dialog rebuilds the config from its own
    // fields — so an ABSENT key on an edit means "the client never knew",
    // not "clear it". Dropping it would silently restore the write licence
    // on a report task's next launch. Preserved at the JSON level: passing
    // the value through `WorkTaskConfig` here would strip every field this
    // build does not know about. An explicit `"deliverable": null` still
    // clears (that is a statement, not ignorance).
    let mut config = draft.config;
    if config.get("deliverable").is_none() {
        if let (Some(obj), Ok(stored)) = (
            config.as_object_mut(),
            serde_json::from_str::<serde_json::Value>(&row.config),
        ) {
            if let Some(deliverable) = stored.get("deliverable") {
                obj.insert("deliverable".to_string(), deliverable.clone());
            }
        }
    }
    let config_str = serde_json::to_string(&config)
        .map_err(|e| DbError::Validation(format!("config not serializable: {e}")))?;
    let mut active = row.into_active_model();
    active.folder_id = Set(draft.folder_id);
    active.title = Set(draft.title.trim().to_string());
    active.config = Set(config_str);
    active.updated_at = Set(Utc::now());
    Ok(to_info(active.update(conn).await?))
}

/// Soft-delete, guarded on the status the caller validated.
///
/// The guard is not ceremony: three different arms can claim a `todo` task
/// (the user, the folder's auto-processor, a planned start coming due), and a
/// tombstone written over a generation that just started would strand it —
/// its worktree and its agent process would outlive the row that knows about
/// them, with nothing left to reap them. Returns `false` when the row moved on;
/// the caller must then re-read, settle whatever claimed it, and try again.
pub async fn soft_delete(
    conn: &DatabaseConnection,
    id: i32,
    expected: WorkTaskStatus,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(work_task::Column::DeletedAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::Status.eq(expected))
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    record_event(
        &txn,
        id,
        "user_action",
        "user",
        Some(serde_json::json!({ "action": "delete" })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

// ── state machine (all CAS; event in the same transaction) ─────────────────

/// Claim a task for a fresh execution generation: `from` → queued with
/// `run_seq + 1`, clearing stale failure fields. Returns the NEW run_seq, or
/// `None` if the CAS lost (wrong status / deleted).
pub async fn claim_for_run(
    conn: &DatabaseConnection,
    id: i32,
    from: WorkTaskStatus,
    actor: &str,
) -> Result<Option<i32>, DbError> {
    claim_inner(conn, id, from, actor, None, false, false).await
}

/// `claim_for_run` for a BULK start ("process all"), which must leave a planned
/// task alone.
///
/// The caller's list query already filters planned tasks out, but a list is a
/// snapshot: someone scheduling a task in the moment between the list and this
/// claim would have their plan silently overridden and an agent started at once.
/// So the exclusion rides the CAS too, and losing it simply means the task is
/// skipped — exactly as if it had carried a plan when the list was taken.
pub async fn claim_unplanned_for_run(
    conn: &DatabaseConnection,
    id: i32,
    from: WorkTaskStatus,
    actor: &str,
) -> Result<Option<i32>, DbError> {
    claim_inner(conn, id, from, actor, None, true, false).await
}

/// `claim_for_run` plus a `user_action` event written in the SAME transaction
/// as the CAS.
///
/// Both halves of that atomicity matter for an instruction the user attaches to
/// the claim (review feedback, a retry note):
/// - recorded before the CAS, a claim that LOSES leaves an orphan instruction
///   in the log for some later generation to pick up;
/// - recorded after the CAS commits, the task is already `queued` and a pump
///   running concurrently can claim and launch it before the instruction is
///   readable — the generation would then run without it.
pub async fn claim_for_run_with_action(
    conn: &DatabaseConnection,
    id: i32,
    from: WorkTaskStatus,
    actor: &str,
    action: Option<serde_json::Value>,
    allow_duplicate_source: bool,
) -> Result<Option<i32>, DbError> {
    claim_inner(conn, id, from, actor, action, false, allow_duplicate_source).await
}

/// Shared body of every user-driven claim. `only_unplanned` narrows the CAS to
/// tasks without a planned start (see `claim_unplanned_for_run`); a targeted
/// start leaves it off, because pressing Start on one particular task IS the
/// instruction to override its plan. `allow_duplicate_source` waives the
/// resurrection guard (see [`resurrection_guard`]) for a claim the user
/// explicitly confirmed.
async fn claim_inner(
    conn: &DatabaseConnection,
    id: i32,
    from: WorkTaskStatus,
    actor: &str,
    action: Option<serde_json::Value>,
    only_unplanned: bool,
    allow_duplicate_source: bool,
) -> Result<Option<i32>, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let mut update = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Queued)),
        )
        .col_expr(
            work_task::Column::RunSeq,
            Expr::col(work_task::Column::RunSeq).add(1),
        )
        .col_expr(work_task::Column::FailureReason, Expr::value(None::<String>))
        .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
        // A fresh generation invalidates the previous run's self-reported
        // verdict (result_summary stays visible until the next settle).
        .col_expr(work_task::Column::Verdict, Expr::value(None::<String>))
        // A user-driven claim supersedes any auto-remerge intent and stale
        // preflight light, resurrects an archived terminal task, and consumes
        // the planned start (the task is starting now — there is no later start
        // left to plan, and a plan surviving into `canceled → todo` would fire
        // a run the user never asked for).
        .col_expr(
            work_task::Column::ScheduledAt,
            Expr::value(None::<chrono::DateTime<Utc>>),
        )
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        .col_expr(work_task::Column::Preflight, Expr::value(None::<String>))
        .col_expr(
            work_task::Column::ArchivedAt,
            Expr::value(None::<chrono::DateTime<Utc>>),
        )
        .col_expr(work_task::Column::FinishedAt, Expr::value(None::<chrono::DateTime<Utc>>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(from))
        .filter(work_task::Column::DeletedAt.is_null());
    if only_unplanned {
        update = update.filter(work_task::Column::ScheduledAt.is_null());
    }
    let res = update.exec(&txn).await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(None);
    }
    let claimed = work_task::Entity::find_by_id(id)
        .one(&txn)
        .await?
        .ok_or_else(|| DbError::NotFound(format!("work task {id}")))?;
    // Resurrection guard, inside the SAME transaction as the winning CAS: a
    // failed/canceled forge task must not come back to life while ANOTHER
    // active task already handles the same work item ("trigger a replacement,
    // then requeue the old card" would end with two live tasks on one issue).
    // The user can waive it explicitly per claim.
    if matches!(from, WorkTaskStatus::Failed | WorkTaskStatus::Canceled)
        && !allow_duplicate_source
    {
        if let Some(key) = claimed.source_key.as_deref() {
            if let Some(other) = other_active_with_same_source(&txn, id, key).await? {
                txn.rollback().await?;
                return Err(duplicate_active_source_error(&other));
            }
        }
    }
    let run_seq = claimed.run_seq;
    // The instruction lands before the status change, so a newest-first scan
    // that stops at the first user action never has to reason about ordering
    // within this transaction.
    if let Some(mut action) = action {
        if let serde_json::Value::Object(map) = &mut action {
            map.insert("run_seq".to_string(), serde_json::json!(run_seq));
        }
        record_event(&txn, id, "user_action", actor, Some(action)).await?;
    }
    status_changed_event(&txn, id, actor, Some(from), WorkTaskStatus::Queued, None).await?;
    txn.commit().await?;
    Ok(Some(run_seq))
}

/// Persist the board's new pending-column order: each id gets its index as its
/// `sort_order`. Scoped to the folder (an id from another folder simply
/// doesn't match the WHERE) — sort_order is a pure ordering field, so a task
/// that advanced mid-drag is harmless to renumber.
pub async fn reorder(
    conn: &DatabaseConnection,
    folder_id: i32,
    ordered_ids: &[i32],
) -> Result<(), DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    for (index, id) in ordered_ids.iter().enumerate() {
        work_task::Entity::update_many()
            .col_expr(work_task::Column::SortOrder, Expr::value(index as i32))
            .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
            .filter(work_task::Column::Id.eq(*id))
            .filter(work_task::Column::FolderId.eq(folder_id))
            .filter(work_task::Column::DeletedAt.is_null())
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    Ok(())
}

/// Scheduler arm of an auto_process folder: claim the head todo task (board
/// order) into the queue — todo → queued, `run_seq + 1` — but only while the
/// folder's concurrency budget has room. Unlike the launch pump's count, the
/// budget here INCLUDES queued tasks (manual or auto), so the auto arm never
/// piles up a queue beyond `max_concurrent`; the rest stay visible in todo.
///
/// Tasks with a planned start are invisible here — that plan IS their schedule,
/// and `claim_due_scheduled` owns it. The filter sits on the CAS as well as on
/// the head lookup, so a plan set between the two still wins (the retry loop
/// then simply picks the next head).
///
/// The CAS UPDATE is the transaction's first statement (write lock up front);
/// the budget is then re-checked INSIDE the same transaction and the claim is
/// rolled back when over — that in-transaction recheck is what makes the
/// reservation safe against concurrent manual starts. `max_concurrent <= 0`
/// means unlimited. Returns the claimed task id, or `None` when the folder has
/// no todo task or the budget is spent.
pub async fn auto_claim_next(
    conn: &DatabaseConnection,
    folder_id: i32,
    max_concurrent: i32,
) -> Result<Option<i32>, DbError> {
    loop {
        // Head lookup runs outside the transaction so the write stays first;
        // the CAS below re-checks the status and simply retries on a miss.
        let head = work_task::Entity::find()
            .filter(work_task::Column::DeletedAt.is_null())
            .filter(work_task::Column::FolderId.eq(folder_id))
            .filter(work_task::Column::Status.eq(WorkTaskStatus::Todo))
            .filter(work_task::Column::ScheduledAt.is_null())
            .inner_join(folder::Entity)
            .filter(folder::Column::DeletedAt.is_null())
            .order_by_asc(work_task::Column::SortOrder)
            .order_by_asc(work_task::Column::Id)
            .one(conn)
            .await?;
        let Some(head) = head else {
            return Ok(None);
        };

        let now = Utc::now();
        let txn = conn.begin().await?;
        let res = work_task::Entity::update_many()
            .col_expr(
                work_task::Column::Status,
                Expr::value(status_str(WorkTaskStatus::Queued)),
            )
            .col_expr(
                work_task::Column::RunSeq,
                Expr::col(work_task::Column::RunSeq).add(1),
            )
            .col_expr(work_task::Column::FailureReason, Expr::value(None::<String>))
            .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
            // Every claim invalidates the previous run's self-report — the
            // settle path reads `verdict` as "this generation's", so a value
            // carried in from an older run (review → cancel → requeue → todo)
            // would decide an outcome it knows nothing about.
            .col_expr(work_task::Column::Verdict, Expr::value(None::<String>))
            .col_expr(work_task::Column::FinishedAt, Expr::value(None::<chrono::DateTime<Utc>>))
            .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
            .filter(work_task::Column::Id.eq(head.id))
            .filter(work_task::Column::Status.eq(WorkTaskStatus::Todo))
            .filter(work_task::Column::ScheduledAt.is_null())
            .filter(work_task::Column::DeletedAt.is_null())
            .exec(&txn)
            .await?;
        if res.rows_affected != 1 {
            // Someone moved the head (manual start, edit, delete, a plan) —
            // retry with the fresh head.
            txn.rollback().await?;
            continue;
        }
        if max_concurrent > 0 {
            let active = work_task::Entity::find()
                .filter(work_task::Column::DeletedAt.is_null())
                .filter(work_task::Column::FolderId.eq(folder_id))
                .filter(work_task::Column::Status.is_in([
                    WorkTaskStatus::Queued,
                    WorkTaskStatus::Preparing,
                    WorkTaskStatus::Running,
                    WorkTaskStatus::AwaitingInput,
                    WorkTaskStatus::Merging,
                ]))
                .count(&txn)
                .await?;
            if active > max_concurrent as u64 {
                txn.rollback().await?;
                return Ok(None);
            }
        }
        status_changed_event(
            &txn,
            head.id,
            "engine",
            Some(WorkTaskStatus::Todo),
            WorkTaskStatus::Queued,
            Some(serde_json::json!({ "auto": true })),
        )
        .await?;
        txn.commit().await?;
        return Ok(Some(head.id));
    }
}

/// Plan (or, with `at = None`, un-plan) the start of a to-do task.
///
/// Only `todo` accepts a plan: every other status either has a run of its own
/// already or is terminal, and `scheduled_at` is read nowhere else. Returns
/// `false` when the CAS loses (wrong status / deleted), which the command layer
/// turns into a readable refusal.
pub async fn set_schedule(
    conn: &DatabaseConnection,
    id: i32,
    at: Option<chrono::DateTime<Utc>>,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(work_task::Column::ScheduledAt, Expr::value(at))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Todo))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    let payload = match at {
        Some(at) => serde_json::json!({ "action": "schedule", "scheduled_at": at }),
        None => serde_json::json!({ "action": "unschedule" }),
    };
    record_event(&txn, id, "user_action", "user", Some(payload)).await?;
    txn.commit().await?;
    Ok(true)
}

/// Claim every to-do task whose planned start has arrived: todo → queued,
/// `run_seq + 1`, plan consumed — the same transition the user's own start
/// button performs. Returns the claimed `(task_id, folder_id)` pairs so the
/// caller can nudge each folder's pump.
///
/// Deliberately NOT budget-aware, unlike `auto_claim_next`: a planned start is
/// as explicit as pressing Start, so a busy folder must park the task in the
/// queue (where the pump drains it as slots free) instead of dropping the plan
/// on the floor. Clearing `scheduled_at` inside the CAS is what makes a plan
/// fire exactly once — a second sweep, in this process or after a restart,
/// no longer matches the row.
pub async fn claim_due_scheduled(
    conn: &DatabaseConnection,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<(i32, i32)>, DbError> {
    // Live folders only: a task of a removed folder is unschedulable, exactly
    // as it is for the pump and the auto arm.
    let due = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Todo))
        .filter(work_task::Column::ScheduledAt.is_not_null())
        .filter(work_task::Column::ScheduledAt.lte(now))
        .inner_join(folder::Entity)
        .filter(folder::Column::DeletedAt.is_null())
        .order_by_asc(work_task::Column::ScheduledAt)
        .order_by_asc(work_task::Column::SortOrder)
        .order_by_asc(work_task::Column::Id)
        .all(conn)
        .await?;

    let mut claimed = Vec::new();
    for row in due {
        let txn = conn.begin().await?;
        let res = work_task::Entity::update_many()
            .col_expr(
                work_task::Column::Status,
                Expr::value(status_str(WorkTaskStatus::Queued)),
            )
            .col_expr(
                work_task::Column::RunSeq,
                Expr::col(work_task::Column::RunSeq).add(1),
            )
            .col_expr(
                work_task::Column::ScheduledAt,
                Expr::value(None::<chrono::DateTime<Utc>>),
            )
            .col_expr(work_task::Column::FailureReason, Expr::value(None::<String>))
            .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
            // Same reason as every other claim: the settle path treats a
            // present `verdict` as this generation's self-report.
            .col_expr(work_task::Column::Verdict, Expr::value(None::<String>))
            .col_expr(
                work_task::Column::FinishedAt,
                Expr::value(None::<chrono::DateTime<Utc>>),
            )
            .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(work_task::Column::Id.eq(row.id))
            .filter(work_task::Column::Status.eq(WorkTaskStatus::Todo))
            .filter(work_task::Column::ScheduledAt.is_not_null())
            .filter(work_task::Column::ScheduledAt.lte(now))
            .filter(work_task::Column::DeletedAt.is_null())
            .exec(&txn)
            .await?;
        if res.rows_affected != 1 {
            // Started by hand, re-planned, or deleted since the scan.
            txn.rollback().await?;
            continue;
        }
        status_changed_event(
            &txn,
            row.id,
            "engine",
            Some(WorkTaskStatus::Todo),
            WorkTaskStatus::Queued,
            Some(serde_json::json!({ "scheduled": true })),
        )
        .await?;
        txn.commit().await?;
        claimed.push((row.id, row.folder_id));
    }
    Ok(claimed)
}

/// canceled → todo ("requeue"): back to the board, worktree (if any) reused at
/// the next start, optionally carrying the note the user attached to the
/// requeue.
///
/// The SESSION is not reused: the conversation link is dropped so the next start
/// runs the task from the top rather than resuming the run the user canceled.
///
/// The note is written in the SAME transaction as the CAS: the moment this
/// commits the task is schedulable, and an `auto_process` folder's pump can
/// claim and launch it — a note written afterwards would lose that race.
pub async fn requeue_canceled(
    conn: &DatabaseConnection,
    id: i32,
    note: Option<&str>,
    blocks: &[serde_json::Value],
    allow_duplicate_source: bool,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Todo)),
        )
        .col_expr(work_task::Column::FailureReason, Expr::value(None::<String>))
        .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        .col_expr(
            work_task::Column::ArchivedAt,
            Expr::value(None::<chrono::DateTime<Utc>>),
        )
        .col_expr(work_task::Column::FinishedAt, Expr::value(None::<chrono::DateTime<Utc>>))
        // Drop the link to the run the user just canceled. `launch_mode_for`
        // reads exactly this column to decide `Retry` vs `Fresh`, so leaving it
        // meant the next start silently RESUMED the canceled session instead of
        // starting the task over, and handed the reconcile sweep a conversation
        // row whose `cancelled` status belongs to the previous generation. The
        // worktree is still reused; the old conversation still exists in its
        // folder. Unlike `retry` (failed -> queued), which deliberately
        // continues the same session, a requeue puts the task back on the board.
        .col_expr(work_task::Column::ConversationId, Expr::value(None::<i32>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Canceled))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    // Same resurrection guard as `claim_inner` — canceled → todo is the other
    // road back into the active set.
    if !allow_duplicate_source {
        let row = work_task::Entity::find_by_id(id)
            .one(&txn)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("work task {id}")))?;
        if let Some(key) = row.source_key.as_deref() {
            if let Some(other) = other_active_with_same_source(&txn, id, key).await? {
                txn.rollback().await?;
                return Err(duplicate_active_source_error(&other));
            }
        }
    }
    // An attachment is an instruction on its own: a screenshot with no sentence
    // still has to reach the next run, so the action is recorded whenever
    // EITHER part is present.
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if note.is_some() || !blocks.is_empty() {
        record_event(
            &txn,
            id,
            "user_action",
            "user",
            Some(serde_json::json!({
                "action": "requeue",
                "note": note.unwrap_or_default(),
                "blocks": blocks,
            })),
        )
        .await?;
    }
    status_changed_event(
        &txn,
        id,
        "user",
        Some(WorkTaskStatus::Canceled),
        WorkTaskStatus::Todo,
        None,
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Record the minted worktree + branch/base coordinates (no status change).
pub async fn attach_worktree(
    conn: &DatabaseConnection,
    id: i32,
    worktree_folder_id: i32,
    base_branch: &str,
    base_sha: &str,
    work_branch: &str,
) -> Result<(), DbError> {
    work_task::Entity::update_many()
        .col_expr(
            work_task::Column::WorktreeFolderId,
            Expr::value(Some(worktree_folder_id)),
        )
        .col_expr(
            work_task::Column::BaseBranch,
            Expr::value(Some(base_branch.to_string())),
        )
        .col_expr(
            work_task::Column::BaseSha,
            Expr::value(Some(base_sha.to_string())),
        )
        .col_expr(
            work_task::Column::WorkBranch,
            Expr::value(Some(work_branch.to_string())),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .exec(conn)
        .await?;
    Ok(())
}

/// queued → preparing for the given generation: the task leaves the queue and
/// starts its setup (worktree, init command, agent spawn). Does NOT bump
/// `run_seq` — it is the same generation, just past the wait for a slot.
/// Losing the CAS means a concurrent cancel/claim moved the task on.
pub async fn begin_setup(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
) -> Result<bool, DbError> {
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Preparing)),
        )
        // The previous generation's connection died with it, and this one has
        // not spawned yet — a column still naming the dead one is what makes a
        // `preparing` row unreadable to anything that streams from it (see
        // `mark_preparing_live`, which publishes the live id as soon as there
        // is one). `begin_merge` clears it for the same reason.
        .col_expr(work_task::Column::ConnectionId, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Queued))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(
        &txn,
        id,
        "engine",
        Some(WorkTaskStatus::Queued),
        WorkTaskStatus::Preparing,
        None,
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// preparing → queued: the reconcile sweep's escape hatch for a setup that no
/// process owns any more (its launch task died without failing the row). Back
/// in the queue, the pump simply relaunches it — the same self-healing a stuck
/// `queued` row gets today.
pub async fn abandon_setup(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
) -> Result<bool, DbError> {
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Queued)),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Preparing))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(
        &txn,
        id,
        "engine",
        Some(WorkTaskStatus::Preparing),
        WorkTaskStatus::Queued,
        None,
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Record the live connection/conversation of a generation that is still
/// SETTING UP — the status stays `preparing`, only the coordinates move.
///
/// The counterpart of [`mark_merging_live`] for every non-merge round, and it
/// exists for the same reason: between the agent spawn and the round's own
/// prompt a launch can spend minutes on the pre-prompt context compaction, and
/// for all of that time the agent is really working in a session no surface can
/// find. Publishing the pair here is what lets the board's session viewer
/// attach to (and stream) that turn instead of showing the previous round's
/// finished transcript.
///
/// Deliberately not a status change: `running` is the round's own turn, and
/// the settle paths key off it. CAS'd on (preparing, run_seq) so a cancel or a
/// newer generation that landed in between wins — a `false` return tells the
/// launch to unwind rather than compact a session nobody is waiting on.
pub async fn mark_preparing_live(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    conversation_id: i32,
    connection_id: &str,
) -> Result<bool, DbError> {
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::ConversationId,
            Expr::value(Some(conversation_id)),
        )
        .col_expr(
            work_task::Column::ConnectionId,
            Expr::value(Some(connection_id.to_string())),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Preparing))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

/// preparing → running for the given generation; binds conversation +
/// connection.
pub async fn mark_running(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    conversation_id: i32,
    connection_id: &str,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Running)),
        )
        .col_expr(
            work_task::Column::ConversationId,
            Expr::value(Some(conversation_id)),
        )
        .col_expr(
            work_task::Column::ConnectionId,
            Expr::value(Some(connection_id.to_string())),
        )
        .col_expr(work_task::Column::StartedAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Preparing))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(
        &txn,
        id,
        "engine",
        Some(WorkTaskStatus::Preparing),
        WorkTaskStatus::Running,
        None,
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Record the live connection/conversation of a MERGE generation. The status
/// stays `merging` for the whole agent turn; the CAS (merging + run_seq) makes
/// a concurrent settle or recovery win cleanly.
pub async fn mark_merging_live(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    conversation_id: i32,
    connection_id: &str,
) -> Result<bool, DbError> {
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::ConversationId,
            Expr::value(Some(conversation_id)),
        )
        .col_expr(
            work_task::Column::ConnectionId,
            Expr::value(Some(connection_id.to_string())),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Merging))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

/// `from` (any of) → failed with a reason. `run_seq: Some(n)` guards
/// engine-driven failures against stale generations; `None` is used by the
/// boot/reconcile sweeps that own the whole DB.
pub async fn fail(
    conn: &DatabaseConnection,
    id: i32,
    from: &[WorkTaskStatus],
    run_seq: Option<i32>,
    reason: &str,
    error: Option<String>,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let mut update = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Failed)),
        )
        .col_expr(
            work_task::Column::FailureReason,
            Expr::value(Some(reason.to_string())),
        )
        .col_expr(work_task::Column::LastError, Expr::value(error.clone()))
        .col_expr(work_task::Column::ConnectionId, Expr::value(None::<String>))
        // Any failure abandons a pending auto-remerge — the user restarts the
        // cycle explicitly.
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        .col_expr(work_task::Column::SettledAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.is_in(from.iter().copied()))
        .filter(work_task::Column::DeletedAt.is_null());
    if let Some(seq) = run_seq {
        update = update.filter(work_task::Column::RunSeq.eq(seq));
    }
    let res = update.exec(&txn).await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(
        &txn,
        id,
        "engine",
        None,
        WorkTaskStatus::Failed,
        Some(serde_json::json!({ "reason": reason, "error": error })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Cancel the active generation when its agent reports a cancelled turn.
///
/// Unlike [`cancel`], this is an engine-owned transition: a delayed event must
/// not overwrite a generation that has already settled for review (or a newer
/// generation of the same task).
pub async fn cancel_running_generation(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
) -> Result<bool, DbError> {
    cancel_inner(
        conn,
        id,
        &[WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput],
        Some(run_seq),
        "engine",
        None,
    )
    .await
}

/// running/awaiting_input → review for the given generation. Captures the
/// agent's summary and the diff-stat snapshot; also writes a `diff_stat` event
/// when stats are present.
/// Record the agent's self-reported verdict (`task_complete` MCP tool) on the
/// current generation while it is still executing. `result_summary` is written
/// unconditionally (NULL when the report carried none), so a present verdict
/// always means the summary column reflects THIS generation's report — the
/// settle path relies on that to prefer it over the captured assistant text.
/// Same-transaction `agent_verdict` event.
pub async fn set_verdict(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    verdict: &str,
    summary: Option<&str>,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Verdict,
            Expr::value(Some(verdict.to_string())),
        )
        .col_expr(
            work_task::Column::ResultSummary,
            Expr::value(summary.map(str::to_string)),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(
            work_task::Column::Status
                .is_in([WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput]),
        )
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    record_event(
        &txn,
        id,
        "agent_verdict",
        "agent",
        Some(serde_json::json!({ "verdict": verdict, "summary": summary })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Re-write the diff counters of a task that is ALREADY in review, without
/// touching anything else about it (no status change, no timeline entry — the
/// `diff_stat` event records what the settle measured and stays as it was).
///
/// Bound to the generation whose counters these are: a task that started
/// another round between the read and this write must not be stamped with
/// numbers taken from the previous one.
pub async fn refresh_diff_stats(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    stats: (i32, i32, i32),
) -> Result<bool, DbError> {
    let res = work_task::Entity::update_many()
        .col_expr(work_task::Column::FilesChanged, Expr::value(Some(stats.0)))
        .col_expr(work_task::Column::Additions, Expr::value(Some(stats.1)))
        .col_expr(work_task::Column::Deletions, Expr::value(Some(stats.2)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

pub async fn settle_review(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    result_summary: Option<String>,
    stats: Option<(i32, i32, i32)>,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Review)),
        )
        .col_expr(
            work_task::Column::ResultSummary,
            Expr::value(result_summary.clone()),
        )
        .col_expr(
            work_task::Column::FilesChanged,
            Expr::value(stats.map(|s| s.0)),
        )
        .col_expr(work_task::Column::Additions, Expr::value(stats.map(|s| s.1)))
        .col_expr(work_task::Column::Deletions, Expr::value(stats.map(|s| s.2)))
        .col_expr(work_task::Column::ConnectionId, Expr::value(None::<String>))
        // A fresh review starts with a fresh light; the preflight runner
        // rewrites it right after when one is configured.
        .col_expr(work_task::Column::Preflight, Expr::value(None::<String>))
        .col_expr(work_task::Column::SettledAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(
            work_task::Column::Status
                .is_in([WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput]),
        )
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(&txn, id, "engine", None, WorkTaskStatus::Review, None).await?;
    if let Some((files, adds, dels)) = stats {
        record_event(
            &txn,
            id,
            "diff_stat",
            "engine",
            Some(serde_json::json!({
                "files_changed": files,
                "additions": adds,
                "deletions": dels,
            })),
        )
        .await?;
    }
    txn.commit().await?;
    Ok(true)
}

/// running ⇄ awaiting_input for the given generation.
pub async fn flip_awaiting(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    awaiting: bool,
) -> Result<bool, DbError> {
    let (from, to) = if awaiting {
        (WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput)
    } else {
        (WorkTaskStatus::AwaitingInput, WorkTaskStatus::Running)
    };
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(work_task::Column::Status, Expr::value(status_str(to)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(from))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(&txn, id, "engine", Some(from), to, None).await?;
    txn.commit().await?;
    Ok(true)
}

/// review → merging, persisting the merge intent in the same transaction (the
/// crash-recovery anchor). Also records a `merge_attempt` event. The CAS takes
/// `expect_run_seq` — the generation the caller read and validated — so a
/// dispatch that waited out the folder lock behind another attempt cannot land
/// on a row that has since moved on. `auto` marks a merge the engine
/// dispatched on its own (the folder's auto-merge setting): same transition,
/// timeline records who pulled the trigger, and the CAS additionally refuses
/// rows carrying `last_error` — a failed merge waits for a human instead of
/// being retried unattended.
pub async fn begin_merge(
    conn: &DatabaseConnection,
    id: i32,
    state: &WorkTaskMergeState,
    expect_run_seq: i32,
    auto: bool,
    expect_pending_merge: Option<&str>,
) -> Result<Option<i32>, DbError> {
    let state_json = serde_json::to_string(state)
        .map_err(|e| DbError::Validation(format!("merge state not serializable: {e}")))?;
    let now = Utc::now();
    let txn = conn.begin().await?;
    let mut update = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Merging)),
        )
        // The merge is a fresh agent generation: bump run_seq so stale events
        // of the settled run can't touch it, and clear the run-scoped fields.
        .col_expr(
            work_task::Column::RunSeq,
            Expr::col(work_task::Column::RunSeq).add(1),
        )
        .col_expr(work_task::Column::ConnectionId, Expr::value(None::<String>))
        .col_expr(work_task::Column::Verdict, Expr::value(None::<String>))
        .col_expr(work_task::Column::MergeState, Expr::value(Some(state_json)))
        .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
        // The queued intent (if any) is being spent right here — whether this
        // dispatch came from the pump draining the queue or from a click that
        // found the slot free.
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        // Bind the dispatch to the exact review generation the caller
        // validated: a merge_task that sat out the folder lock while the row
        // moved on (another dispatch bounced, a requeue) must miss, not merge
        // a generation nobody looked at.
        .filter(work_task::Column::RunSeq.eq(expect_run_seq))
        .filter(work_task::Column::DeletedAt.is_null());
    if auto {
        // last_error is the "waits for a human" latch of the no-auto-retry
        // invariant: an unattended dispatch never clears it — only a user's
        // does (the manual path right below this filter).
        update = update.filter(work_task::Column::LastError.is_null());
        // A merge the user parked is theirs to land, with the commit message
        // and worktree choice THEY picked; the unattended dispatch carries
        // neither and would clear the intent on its way past. This is the
        // authoritative gate — the sweep's own eligibility check reads a
        // snapshot taken before the folder lock, so a merge queued while it
        // worked would otherwise slip through here.
        update = update.filter(work_task::Column::PendingMerge.is_null());
    }
    // A dispatch OF a queued merge also binds to that exact intent: run_seq
    // alone cannot tell "the merge the pump picked up" from "the one the user
    // withdrew or edited a moment later", and both leave the generation alone.
    if let Some(expected) = expect_pending_merge {
        update = update.filter(work_task::Column::PendingMerge.eq(expected));
    }
    let res = update.exec(&txn).await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(None);
    }
    let run_seq = work_task::Entity::find_by_id(id)
        .one(&txn)
        .await?
        .map(|m| m.run_seq)
        .ok_or_else(|| DbError::NotFound(format!("work task {id}")))?;
    let actor = if auto { "auto" } else { "user" };
    record_event(
        &txn,
        id,
        "merge_attempt",
        actor,
        Some(serde_json::json!({
            "strategy": state.strategy,
            "pre_merge_head": state.pre_merge_head,
            "auto": auto,
        })),
    )
    .await?;
    status_changed_event(
        &txn,
        id,
        actor,
        Some(WorkTaskStatus::Review),
        WorkTaskStatus::Merging,
        // The timeline's merging header shows this line, so an unattended
        // merge says who started it.
        auto.then(|| serde_json::json!({ "reason": "started by auto-merge" })),
    )
    .await?;
    txn.commit().await?;
    Ok(Some(run_seq))
}

/// Park a merge on a reviewed task because the folder's merge slot is busy —
/// the review→merging CAS's patient twin, and the only writer of
/// `pending_merge`. Same generation binding as [`begin_merge`] (review +
/// `expect_run_seq`), so an intent can never land on a row that moved on while
/// the caller waited for the folder lock.
///
/// Clears `last_error` like a manual dispatch does: the user re-asking for this
/// merge IS the human intervention the banner was waiting for, and leaving it
/// on would make the queued row look like it had already failed.
///
/// `expect_pending_merge` is the pump's re-queue guard: `Some(json)` demands
/// that the row still hold exactly that intent, so a re-park cannot resurrect
/// a withdrawn merge or overwrite an edit the user made while the pump was
/// working. A click passes `None` — the user's latest word always wins.
pub async fn queue_merge(
    conn: &DatabaseConnection,
    id: i32,
    intent: &WorkTaskQueuedMerge,
    expect_run_seq: i32,
    expect_pending_merge: Option<&str>,
) -> Result<bool, DbError> {
    let intent_json = serde_json::to_string(intent)
        .map_err(|e| DbError::Validation(format!("merge intent not serializable: {e}")))?;
    let now = Utc::now();
    let txn = conn.begin().await?;
    let mut update = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::PendingMerge,
            Expr::value(Some(intent_json)),
        )
        .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        .filter(work_task::Column::RunSeq.eq(expect_run_seq))
        .filter(work_task::Column::DeletedAt.is_null());
    if let Some(expected) = expect_pending_merge {
        update = update.filter(work_task::Column::PendingMerge.eq(expected));
    }
    let res = update.exec(&txn).await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    record_event(
        &txn,
        id,
        "merge_queued",
        "user",
        Some(serde_json::json!({
            "auto_message": intent.message.is_none(),
            "delete_worktree": intent.delete_worktree,
        })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// The user withdrew a queued merge (still in review, nothing dispatched yet).
/// CAS-guarded on "actually queued" so a withdrawal that races the pump's
/// dispatch reports the miss instead of silently pretending.
pub async fn unqueue_merge(conn: &DatabaseConnection, id: i32) -> Result<bool, DbError> {
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        .filter(work_task::Column::PendingMerge.is_not_null())
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    record_event(
        &txn,
        id,
        "user_action",
        "user",
        Some(serde_json::json!({ "action": "unqueue_merge" })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Drop a queued intent without a timeline entry — the engine's cleanup when a
/// queued dispatch turns out to be impossible (the reason lands on the row as
/// the error banner instead, which is what the user needs to read).
///
/// CAS'd on the exact intent being refused: by the time a dispatch fails, the
/// user may have withdrawn that merge or queued a different one, and neither
/// deserves to be swept up by the refusal of an older request. `false` = the
/// row moved on, so the caller must not banner it either.
pub async fn clear_queued_merge(
    conn: &DatabaseConnection,
    id: i32,
    expect_pending_merge: &str,
) -> Result<bool, DbError> {
    let res = work_task::Entity::update_many()
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::PendingMerge.eq(expect_pending_merge))
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

/// review → merging for a DELIVERY (`op = deliver_pr`) — the engine pushing a
/// branch and opening a pull request, with no agent generation behind it.
///
/// Mirrors [`begin_merge`]'s two protective writes for the same reasons, and
/// deliberately differs in one: `verdict` is KEPT. A merge dispatch clears it
/// because the agent is about to produce a new one; a delivery spawns nobody,
/// so clearing it would just erase the review badge the user is looking at.
pub async fn begin_delivery(
    conn: &DatabaseConnection,
    id: i32,
    state: &WorkTaskMergeState,
    expect_run_seq: i32,
) -> Result<Option<i32>, DbError> {
    let state_json = serde_json::to_string(state)
        .map_err(|e| DbError::Validation(format!("merge state not serializable: {e}")))?;
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Merging)),
        )
        // Bumped so events of the run that just settled cannot act on a row
        // that is now delivering.
        .col_expr(
            work_task::Column::RunSeq,
            Expr::col(work_task::Column::RunSeq).add(1),
        )
        // Cleared so crash recovery cannot mistake the settled run's still-open
        // session for "a live generation owns this settle" and skip a delivery
        // that died with the process. `conversation_id` (the history link the
        // UI shows) is untouched.
        .col_expr(work_task::Column::ConnectionId, Expr::value(None::<String>))
        .col_expr(work_task::Column::MergeState, Expr::value(Some(state_json)))
        .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        .filter(work_task::Column::RunSeq.eq(expect_run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(None);
    }
    let run_seq = work_task::Entity::find_by_id(id)
        .one(&txn)
        .await?
        .map(|m| m.run_seq)
        .ok_or_else(|| DbError::NotFound(format!("work task {id}")))?;
    record_event(
        &txn,
        id,
        "deliver_attempt",
        "user",
        Some(serde_json::json!({
            "remote_branch": state.remote_branch,
            "expected_head": state.expected_head,
        })),
    )
    .await?;
    status_changed_event(
        &txn,
        id,
        "user",
        Some(WorkTaskStatus::Review),
        WorkTaskStatus::Merging,
        Some(serde_json::json!({ "reason": "delivering to a pull request" })),
    )
    .await?;
    txn.commit().await?;
    Ok(Some(run_seq))
}

/// merging → done for a delivery: the pull request carrying this task is open
/// (or already merged) and anchored to the commit we pushed.
///
/// `source_meta_json` is the row's provenance snapshot with `result_pr` filled
/// in, serialized by the CALLER. That is not an accident of layering: the
/// update below has to be the transaction's FIRST statement. A deferred SQLite
/// transaction that reads before it writes cannot upgrade to a writer under
/// WAL — it fails with `database is locked` instead of waiting — so any
/// read-modify-write of `source_meta` happens outside, and the CAS filter is
/// what makes a stale read harmless.
pub async fn complete_delivered(
    conn: &DatabaseConnection,
    id: i32,
    expect_run_seq: i32,
    pr_url: &str,
    source_meta_json: &str,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Done)),
        )
        .col_expr(
            work_task::Column::CompletionKind,
            Expr::value(Some(COMPLETION_DELIVERED_PR.to_string())),
        )
        .col_expr(
            work_task::Column::SourceMeta,
            Expr::value(Some(source_meta_json.to_string())),
        )
        .col_expr(work_task::Column::MergeState, Expr::value(None::<String>))
        .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
        .col_expr(work_task::Column::FinishedAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Merging))
        .filter(work_task::Column::RunSeq.eq(expect_run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    record_event(
        &txn,
        id,
        "delivered_pr",
        "engine",
        Some(serde_json::json!({ "pr_url": pr_url })),
    )
    .await?;
    status_changed_event(
        &txn,
        id,
        "engine",
        Some(WorkTaskStatus::Merging),
        WorkTaskStatus::Done,
        Some(serde_json::json!({ "pr_url": pr_url })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// The three ways a task can be `done`, recorded in `completion_kind`.
pub const COMPLETION_MERGED: &str = "merged";
pub const COMPLETION_DELIVERED_PR: &str = "delivered_pr";
/// Deliberately an umbrella rather than `no_changes`: [`complete_without_merge`]
/// also accepts a task whose worktree is GONE, and that branch may still hold
/// commits nobody landed. Claiming "no changes" there would be false evidence;
/// the existing reason string still distinguishes the two situations.
pub const COMPLETION_ACCEPTED_WITHOUT_MERGE: &str = "accepted_without_merge";

/// merging → done. The merge path's writer of `done` (the other is
/// [`complete_without_merge`]); never rolls back. Used both by the live merge
/// path and by crash recovery back-filling a landed merge.
pub async fn merge_landed(
    conn: &DatabaseConnection,
    id: i32,
    merge_commit: &str,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Done)),
        )
        .col_expr(
            work_task::Column::MergeCommit,
            Expr::value(Some(merge_commit.to_string())),
        )
        .col_expr(
            work_task::Column::CompletionKind,
            Expr::value(Some(COMPLETION_MERGED.to_string())),
        )
        .col_expr(work_task::Column::FinishedAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Merging))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(
        &txn,
        id,
        "engine",
        Some(WorkTaskStatus::Merging),
        WorkTaskStatus::Done,
        Some(serde_json::json!({ "merge_commit": merge_commit })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// review → done for a task with no merge to run: the user accepted it
/// outright — because the change set is empty, or because the worktree is gone
/// and no merge generation could execute. The second writer of `done` (see
/// [`merge_landed`]) — `merge_commit` stays NULL, and the caller has already
/// checked git truth, so the CAS is the whole guard. `reason` is the
/// human-readable line the timeline shows under the done header.
pub async fn complete_without_merge(
    conn: &DatabaseConnection,
    id: i32,
    reason: &str,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Done)),
        )
        .col_expr(
            work_task::Column::CompletionKind,
            Expr::value(Some(COMPLETION_ACCEPTED_WITHOUT_MERGE.to_string())),
        )
        // A refused merge attempt leaves its reason on the row; the task is
        // finishing on purpose now, so that banner must not follow it.
        .col_expr(work_task::Column::LastError, Expr::value(None::<String>))
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        .col_expr(work_task::Column::FinishedAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    status_changed_event(
        &txn,
        id,
        "user",
        Some(WorkTaskStatus::Review),
        WorkTaskStatus::Done,
        Some(serde_json::json!({ "reason": reason })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Leave an error banner on a task still in review — an auto-merge dispatch
/// that was refused before the review→merging CAS (wrong base branch, staged
/// changes, …) has no status transition to carry its reason, and the manual
/// path's toast has no one to pop for. Guarded on review + run_seq; a present
/// `last_error` also excludes the row from later auto-merge attempts, so the
/// banner doubles as the retry stop.
pub async fn set_review_error(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    error: &str,
) -> Result<bool, DbError> {
    let res = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::LastError,
            Expr::value(Some(error.to_string())),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected == 1)
}

/// merging → review after a conflict / preflight failure / crash cleanup.
/// Clears the merge intent; records `merge_conflict` when files are known.
pub async fn merge_back_to_review(
    conn: &DatabaseConnection,
    id: i32,
    // `Some` binds the bounce to one generation — a delivery recovery that
    // spent time at the forge must not undo a newer attempt that started while
    // it was deciding. `None` keeps the historical merge behavior.
    expect_run_seq: Option<i32>,
    error: Option<String>,
    conflict_files: Option<Vec<String>>,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let update = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Review)),
        )
        .col_expr(work_task::Column::MergeState, Expr::value(None::<String>))
        .col_expr(work_task::Column::LastError, Expr::value(error.clone()))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Merging))
        .filter(work_task::Column::DeletedAt.is_null());
    let update = match expect_run_seq {
        Some(seq) => update.filter(work_task::Column::RunSeq.eq(seq)),
        None => update,
    };
    let res = update.exec(&txn).await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    if let Some(files) = conflict_files {
        record_event(
            &txn,
            id,
            "merge_conflict",
            "engine",
            Some(serde_json::json!({ "files": files })),
        )
        .await?;
    }
    status_changed_event(
        &txn,
        id,
        "engine",
        Some(WorkTaskStatus::Merging),
        WorkTaskStatus::Review,
        Some(serde_json::json!({ "error": error })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Write the preflight light for the CURRENT review generation. Guarded on
/// review + run_seq so a slow command finishing after the task moved on is a
/// no-op. Records a `preflight_result` event for terminal statuses.
pub async fn set_preflight(
    conn: &DatabaseConnection,
    id: i32,
    run_seq: i32,
    preflight: &crate::models::WorkTaskPreflight,
) -> Result<bool, DbError> {
    let json = serde_json::to_string(preflight)
        .map_err(|e| DbError::Validation(format!("preflight not serializable: {e}")))?;
    let now = Utc::now();
    let txn = conn.begin().await?;
    let res = work_task::Entity::update_many()
        .col_expr(work_task::Column::Preflight, Expr::value(Some(json)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.eq(WorkTaskStatus::Review))
        .filter(work_task::Column::RunSeq.eq(run_seq))
        .filter(work_task::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    if preflight.status != "running" {
        record_event(
            &txn,
            id,
            "preflight_result",
            "engine",
            Some(serde_json::json!({
                "status": preflight.status,
                "command": preflight.command,
                "exit_code": preflight.exit_code,
            })),
        )
        .await?;
    }
    txn.commit().await?;
    Ok(true)
}

/// Archive / unarchive. Only terminal tasks (done / failed / canceled) can be
/// archived — active ones stay on the board by construction; unarchiving is
/// status-agnostic. Records the user action.
pub async fn set_archived(
    conn: &DatabaseConnection,
    id: i32,
    archived: bool,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let mut update = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::ArchivedAt,
            Expr::value(archived.then_some(now)),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::DeletedAt.is_null());
    if archived {
        update = update
            .filter(work_task::Column::Status.is_in([
                WorkTaskStatus::Done,
                WorkTaskStatus::Failed,
                WorkTaskStatus::Canceled,
            ]))
            .filter(work_task::Column::ArchivedAt.is_null());
    } else {
        update = update.filter(work_task::Column::ArchivedAt.is_not_null());
    }
    let res = update.exec(&txn).await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    record_event(
        &txn,
        id,
        "user_action",
        "user",
        Some(serde_json::json!({ "action": if archived { "archive" } else { "unarchive" } })),
    )
    .await?;
    txn.commit().await?;
    Ok(true)
}

/// Any non-terminal state EXCEPT merging → canceled. Returns whether the CAS
/// won (the engine tears the connection down only when it did).
///
/// `reason` is what the user typed when stopping the task; it rides the
/// `status_changed` payload, where the drawer's timeline already renders it
/// under the phase header. It is a record for the reader, NOT an instruction:
/// `outstanding_instruction` takes instructions only from `user_action` events
/// (it reads `status_changed` purely as the review barrier), so a reason can
/// never be replayed into a later generation's prompt — a requeue carries its
/// own note for that.
pub async fn cancel(
    conn: &DatabaseConnection,
    id: i32,
    reason: Option<&str>,
) -> Result<bool, DbError> {
    cancel_inner(
        conn,
        id,
        &[
            WorkTaskStatus::Todo,
            WorkTaskStatus::Queued,
            WorkTaskStatus::Preparing,
            WorkTaskStatus::Running,
            WorkTaskStatus::AwaitingInput,
            WorkTaskStatus::Review,
            WorkTaskStatus::Failed,
        ],
        None,
        "user",
        reason,
    )
    .await
}

/// The single conditional UPDATE behind both cancels: `expected` (and, for an
/// engine-owned transition, `run_seq`) is the CAS; `actor` / `reason` is the
/// audit trail. One write, so the columns a cancel has to clear can never drift
/// between the user's stop button and the engine's own.
async fn cancel_inner(
    conn: &DatabaseConnection,
    id: i32,
    expected: &[WorkTaskStatus],
    run_seq: Option<i32>,
    actor: &str,
    reason: Option<&str>,
) -> Result<bool, DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    let mut update = work_task::Entity::update_many()
        .col_expr(
            work_task::Column::Status,
            Expr::value(status_str(WorkTaskStatus::Canceled)),
        )
        .col_expr(work_task::Column::ConnectionId, Expr::value(None::<String>))
        // A cancel mid-repair abandons the auto-remerge; a later requeue must
        // not re-fire a stale merge.
        .col_expr(work_task::Column::PendingMerge, Expr::value(None::<String>))
        // Same reasoning for a planned start: stopping a task drops its plan,
        // so a requeue days later cannot resurrect a time nobody remembers
        // setting and launch an agent unattended. (A running row carries no
        // plan — every claim consumes it — so this only bites the user path,
        // but the clear belongs to "canceled", not to one caller.)
        .col_expr(
            work_task::Column::ScheduledAt,
            Expr::value(None::<chrono::DateTime<Utc>>),
        )
        .col_expr(work_task::Column::FinishedAt, Expr::value(Some(now)))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .filter(work_task::Column::Status.is_in(expected.iter().copied()))
        .filter(work_task::Column::DeletedAt.is_null());
    if let Some(run_seq) = run_seq {
        update = update.filter(work_task::Column::RunSeq.eq(run_seq));
    }
    let res = update.exec(&txn).await?;
    if res.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(false);
    }
    let extra = reason
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| serde_json::json!({ "reason": r }));
    status_changed_event(&txn, id, actor, None, WorkTaskStatus::Canceled, extra).await?;
    txn.commit().await?;
    Ok(true)
}

/// Flag / clear the worktree-cleanup outcome. Never touches `status` — a done
/// task with a failed cleanup stays done. Writes a `cleanup_failed` event when
/// flagging.
pub async fn set_cleanup_state(
    conn: &DatabaseConnection,
    id: i32,
    failed: bool,
    error: Option<String>,
) -> Result<(), DbError> {
    let now = Utc::now();
    let txn = conn.begin().await?;
    work_task::Entity::update_many()
        .col_expr(
            work_task::Column::CleanupState,
            Expr::value(if failed { Some("failed".to_string()) } else { None }),
        )
        .col_expr(work_task::Column::UpdatedAt, Expr::value(now))
        .filter(work_task::Column::Id.eq(id))
        .exec(&txn)
        .await?;
    if failed {
        record_event(
            &txn,
            id,
            "cleanup_failed",
            "engine",
            Some(serde_json::json!({ "error": error })),
        )
        .await?;
    }
    txn.commit().await?;
    Ok(())
}

/// Detach the (now removed) worktree from the task and clear any cleanup flag.
pub async fn clear_worktree(conn: &DatabaseConnection, id: i32) -> Result<(), DbError> {
    work_task::Entity::update_many()
        .col_expr(work_task::Column::WorktreeFolderId, Expr::value(None::<i32>))
        .col_expr(work_task::Column::CleanupState, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::Id.eq(id))
        .exec(conn)
        .await?;
    Ok(())
}

/// Tasks in a status that is actively using their worktree — mid-run or
/// mid-merge. Removing such a worktree would pull the directory out from under
/// a live agent, so [`tasks_blocking_worktree_removal`] refuses on their behalf.
const WORKTREE_BUSY_STATUSES: &[WorkTaskStatus] = &[
    WorkTaskStatus::Queued,
    WorkTaskStatus::Preparing,
    WorkTaskStatus::Running,
    WorkTaskStatus::AwaitingInput,
    WorkTaskStatus::Merging,
];

/// Titles of the live tasks currently working inside `folder_id`'s worktree.
/// Non-empty means the worktree must not be removed yet — the card's own
/// "remove worktree" refuses the same statuses.
pub async fn tasks_blocking_worktree_removal(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<Vec<String>, DbError> {
    let rows = work_task::Entity::find()
        .filter(work_task::Column::DeletedAt.is_null())
        .filter(work_task::Column::WorktreeFolderId.eq(folder_id))
        .filter(work_task::Column::Status.is_in(WORKTREE_BUSY_STATUSES.iter().copied()))
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(|row| row.title).collect())
}

/// [`clear_worktree`] for every task pointing at a folder that has just been
/// removed, whoever removed it. Returns the ids it detached so the caller can
/// tell clients to refetch those cards — a task still advertising a worktree
/// that no longer exists offers cleanup and diff actions that can only fail.
pub async fn clear_worktree_by_folder(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<Vec<i32>, DbError> {
    let ids: Vec<i32> = work_task::Entity::find()
        .filter(work_task::Column::WorktreeFolderId.eq(folder_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect();
    if ids.is_empty() {
        return Ok(ids);
    }
    work_task::Entity::update_many()
        .col_expr(work_task::Column::WorktreeFolderId, Expr::value(None::<i32>))
        .col_expr(work_task::Column::CleanupState, Expr::value(None::<String>))
        .col_expr(work_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(work_task::Column::WorktreeFolderId.eq(folder_id))
        .exec(conn)
        .await?;
    Ok(ids)
}

/// Boot recovery: a fresh process has no live connections, so every
/// queued/running/awaiting_input task is an interruption → failed(interrupted)
/// (retry is idempotent and reuses the worktree). Merging tasks are NOT touched
/// here — the engine recovers them from git truth. Only sound while this
/// process provably owns the engine (the data-dir file lock).
pub async fn boot_reconcile_interrupted(conn: &DatabaseConnection) -> Result<u64, DbError> {
    let active = list_by_status(
        conn,
        &[
            WorkTaskStatus::Queued,
            WorkTaskStatus::Preparing,
            WorkTaskStatus::Running,
            WorkTaskStatus::AwaitingInput,
        ],
    )
    .await?;
    let mut n = 0;
    for t in active {
        if fail(
            conn,
            t.id,
            &[
                WorkTaskStatus::Queued,
                WorkTaskStatus::Preparing,
                WorkTaskStatus::Running,
                WorkTaskStatus::AwaitingInput,
            ],
            None,
            "interrupted",
            Some("interrupted by restart".to_string()),
        )
        .await?
        {
            n += 1;
        }
    }
    Ok(n)
}

// ── per-folder settings ─────────────────────────────────────────────────────

pub async fn settings_get(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<WorkTaskFolderSettings, DbError> {
    let row = work_task_settings::Entity::find()
        .filter(work_task_settings::Column::FolderId.eq(folder_id))
        .one(conn)
        .await?;
    Ok(row
        .and_then(|r| serde_json::from_str(&r.config).ok())
        .unwrap_or_default())
}

/// The folder's own row only — `None` when the folder never saved its own
/// settings (and thus follows the global row). This is what lets the settings
/// dialog distinguish "own settings" from the global fallback, which
/// `settings_get_effective` deliberately hides.
pub async fn settings_get_own(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<Option<WorkTaskFolderSettings>, DbError> {
    let row = work_task_settings::Entity::find()
        .filter(work_task_settings::Column::FolderId.eq(folder_id))
        .one(conn)
        .await?;
    Ok(row.and_then(|r| serde_json::from_str(&r.config).ok()))
}

/// Drop the folder's own settings row so it follows the global defaults
/// again. Idempotent — a folder without a row is left as-is.
pub async fn settings_delete(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<(), DbError> {
    work_task_settings::Entity::delete_many()
        .filter(work_task_settings::Column::FolderId.eq(folder_id))
        .exec(conn)
        .await?;
    Ok(())
}

/// Sentinel `folder_id` of the global-defaults settings row. Real folders
/// start at 1 (AUTOINCREMENT), so 0 can never collide.
pub const GLOBAL_SETTINGS_FOLDER_ID: i32 = 0;

/// Effective settings for a folder: the folder's own row wins wholesale; a
/// folder that never saved its own settings falls back to the global row
/// (`folder_id = 0`), then to the built-in defaults. Deliberately not a
/// field-by-field merge — one save detaches the folder from the global row
/// entirely, which keeps the behavior predictable.
pub async fn settings_get_effective(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<WorkTaskFolderSettings, DbError> {
    let own = work_task_settings::Entity::find()
        .filter(work_task_settings::Column::FolderId.eq(folder_id))
        .one(conn)
        .await?
        .and_then(|r| serde_json::from_str(&r.config).ok());
    if let Some(settings) = own {
        return Ok(settings);
    }
    settings_get(conn, GLOBAL_SETTINGS_FOLDER_ID).await
}

pub async fn settings_set(
    conn: &DatabaseConnection,
    folder_id: i32,
    settings: &WorkTaskFolderSettings,
) -> Result<(), DbError> {
    let config = serde_json::to_string(settings)
        .map_err(|e| DbError::Validation(format!("settings not serializable: {e}")))?;
    let now = Utc::now();
    let existing = work_task_settings::Entity::find()
        .filter(work_task_settings::Column::FolderId.eq(folder_id))
        .one(conn)
        .await?;
    match existing {
        Some(row) => {
            let mut active = row.into_active_model();
            active.config = Set(config);
            active.updated_at = Set(now);
            active.update(conn).await?;
        }
        None => {
            let active = work_task_settings::ActiveModel {
                id: NotSet,
                folder_id: Set(folder_id),
                config: Set(config),
                created_at: Set(now),
                updated_at: Set(now),
            };
            active.insert(conn).await?;
        }
    }
    Ok(())
}

// ── Templates ───────────────────────────────────────────────────────────────

fn to_template_info(m: work_task_template::Model) -> crate::models::WorkTaskTemplateInfo {
    crate::models::WorkTaskTemplateInfo {
        id: m.id,
        name: m.name,
        title: m.title,
        config: serde_json::from_str(&m.config).unwrap_or(serde_json::Value::Null),
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

pub async fn template_list(
    conn: &DatabaseConnection,
) -> Result<Vec<crate::models::WorkTaskTemplateInfo>, DbError> {
    let rows = work_task_template::Entity::find()
        .order_by_asc(work_task_template::Column::Name)
        .order_by_asc(work_task_template::Column::Id)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(to_template_info).collect())
}

/// Upsert by exact name: saving under an existing template's name replaces its
/// title + config in place instead of accumulating duplicates.
pub async fn template_save(
    conn: &DatabaseConnection,
    draft: &crate::models::WorkTaskTemplateDraft,
) -> Result<crate::models::WorkTaskTemplateInfo, DbError> {
    let name = draft.name.trim();
    if name.is_empty() {
        return Err(DbError::Validation("template name is required".into()));
    }
    if draft.title.trim().is_empty() {
        return Err(DbError::Validation("template title is required".into()));
    }
    if !draft.config.is_object() {
        return Err(DbError::Validation("template config must be an object".into()));
    }
    let config = serde_json::to_string(&draft.config)
        .map_err(|e| DbError::Validation(format!("template config not serializable: {e}")))?;
    let now = Utc::now();
    let existing = work_task_template::Entity::find()
        .filter(work_task_template::Column::Name.eq(name))
        .one(conn)
        .await?;
    let model = match existing {
        Some(row) => {
            let mut active = row.into_active_model();
            active.title = Set(draft.title.trim().to_string());
            active.config = Set(config);
            active.updated_at = Set(now);
            active.update(conn).await?
        }
        None => {
            let active = work_task_template::ActiveModel {
                id: NotSet,
                name: Set(name.to_string()),
                title: Set(draft.title.trim().to_string()),
                config: Set(config),
                created_at: Set(now),
                updated_at: Set(now),
            };
            active.insert(conn).await?
        }
    };
    Ok(to_template_info(model))
}

pub async fn template_delete(conn: &DatabaseConnection, id: i32) -> Result<(), DbError> {
    work_task_template::Entity::delete_by_id(id).exec(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests;

