//! Work-task execution engine: drives the manual pipeline
//! `todo → queued → running ⇄ awaiting_input → review → merging → done`.
//!
//! Structure mirrors `automation::engine` (single per-process engine elected by
//! an exclusive data-dir file lock; a `tokio::select!` loop over the internal
//! event bus + a reconcile tick), with three task-specific additions:
//! - **run_seq generations**: every launch claims a new `run_seq`; events are
//!   matched on `(connection_id, run_seq)` and settle through CAS updates, so a
//!   cancel racing a late `TurnComplete` is a zero-side-effect no-op.
//! - **backend-driven awaiting_input**: the engine subscribes to
//!   Question/Permission/PlanApproval request+resolve events (the frontend has
//!   no global pending-question channel for unopened conversations) and flips
//!   `running ⇄ awaiting_input` from an outstanding-request-id set.
//! - **two-stage merge with persisted intent**: stage A merges base INTO the
//!   worktree (conflicts always land there); stage B lands on the base branch
//!   in the project folder under a per-folder git mutex, with the merge intent
//!   persisted before execution so crash recovery can replay git truth.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{oneshot, Mutex, Notify};
use tokio::time::MissedTickBehavior;

use crate::acp::manager::ConnectionManager;
use crate::acp::types::{
    AcpEvent, ConnectionStatus, EventEnvelope, PromptCapabilitiesInfo, PromptInputBlock,
};
use crate::acp::work_task_tools::{TaskReportAck, WorkTaskToolAccess};
use crate::acp::InternalEventBus;
use crate::commands::acp::{build_session_runtime_env, verify_agent_installed};
use crate::commands::conversations::{
    create_conversation_core, emit_conversation_upsert, get_folder_conversation_core,
};
use crate::commands::folders::{
    emit_folder_deleted, emit_folder_upsert, get_folder_core, git_worktree_add,
    open_worktree_folder_core, resolve_git_head,
};
use crate::db::entities::conversation::{self, ConversationStatus};
use crate::db::entities::work_task::WorkTaskStatus;
use crate::db::entities::{folder, folder_command};
use crate::db::service::{conversation_service, tab_service, work_task_service};
use crate::db::AppDatabase;
use crate::forge::deliver::{
    adopt_pull_request, pull_request_body, writeback_comment_body, DeliveryCtx, ForgeDeliveryApi,
    ForgePr, NewPullRequest, PrAdoption, TaskOutcome,
};
use crate::forge::{ForgeItemKind, ForgeSourceMeta, SOURCE_KIND_ISSUE, SOURCE_KIND_PR};
use crate::logging::throttle::{LagLogThrottle, LAG_LOG_WINDOW};
use crate::models::{
    AgentType, FollowUpIntent, WorkTaskConfig, WorkTaskFolderSettings, WorkTaskMergeOp,
    WorkTaskMergeState, WorkTaskPreflight, WorkTaskQueuedMerge, DELIVERABLE_REPORT,
    STAGE_PROMPT_ALL,
};
use crate::web::event_bridge::{
    emit_event, EventEmitter, WorkTaskChange, WORK_TASK_CHANGED_EVENT,
};
use crate::work_task::compact;
use crate::work_task::git as task_git;

/// Reconcile sweep cadence.
const RECONCILE_INTERVAL_SECS: u64 = 30;

/// How often planned starts are checked. Its own (tighter) tick rather than a
/// step of the reconcile sweep: this one is a cheap indexed-ish scan, and it
/// bounds how late a task the user planned for a given minute actually starts.
const SCHEDULE_INTERVAL_SECS: u64 = 15;

/// Cap on the preflight output tail persisted with a red light.
const PREFLIGHT_TAIL_CHARS: usize = 4000;

/// How often a waiting pre-prompt compaction re-checks that its connection is
/// still alive. NOT a deadline on the turn — see `await_compaction_turn`, which
/// only ever ends the wait on evidence the turn is over or the connection is
/// gone. Loose enough to cost nothing on a turn that runs for minutes.
const COMPACTION_LIVENESS_TICK: Duration = Duration::from_secs(5);

/// Why a worktree removal declined to run, as one clause. Every surface that
/// can decline one composes its message from THIS — `complete_task`'s checkbox,
/// `cleanup_task`, the delivered-PR keep reason, and the task delete — so the
/// card, the toast and the timeline cannot drift into describing the same
/// condition differently. Only the remedy changes, because only the remedy
/// depends on what the user was doing.
const WORKTREE_HOLDS_UNCOMMITTED: &str = "it still holds uncommitted files";

/// The reason plus the way out of it.
pub(crate) fn worktree_kept(remedy: &str) -> String {
    format!("the worktree was kept: {WORKTREE_HOLDS_UNCOMMITTED}. {remedy}")
}

/// The remedy for the surfaces whose retry IS the cleanup itself.
pub(crate) const RETRY_THE_CLEANUP: &str = "Remove them, then retry the cleanup.";

/// The probe every worktree-removal gate shares: does this checkout still hold
/// work (tracked edits or files git has never seen)?
///
/// FAILS CLOSED. Only two reasons for git being unable to answer are safe to
/// read as "clean": the checkout is already off disk, or git removed its
/// registration and contents but left a readable, strictly empty directory
/// behind. Any entry in that shell (ignored and hidden files included), a
/// corrupt index, a permission error, or another transient filesystem failure
/// means we could not prove the directory is safe to destroy. The operation
/// waiting on this answer is `git worktree remove --force`, so guessing "clean"
/// there trades a recoverable stall for unrecoverable files.
///
/// The `.git` marker is checked FIRST, and not as an optimization: `has_changes`
/// runs `git status` with the path as its working directory, and git walks UP
/// from there. A shell with no marker left is not a checkout git can speak for,
/// so whatever it answers is about the repository that ENCLOSES the shell — the
/// project itself whenever the folder's worktree root is a path inside it. That
/// answer is a clean `Ok(false)` or a dirty `Ok(true)` about somebody else's
/// files, and neither is this path's; only the directory probe below is.
async fn path_holds_uncommitted(path: &str) -> bool {
    match std::fs::symlink_metadata(Path::new(path).join(".git")) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return shell_holds_entries(path),
        // Could not even look at the marker — no proof of anything. Fail closed.
        Err(_) => return true,
    }
    match task_git::has_changes(path).await {
        Ok(dirty) => dirty,
        Err(_) => shell_holds_entries(path),
    }
}

/// The one thing left to ask about a directory git has stopped speaking for: is
/// it strictly empty? Anything else — an entry of any kind, or a failure to read
/// the directory at all — is "holds work", because a `--force` removal is what
/// waits on the answer. Only a path that is gone reads as nothing to lose.
fn shell_holds_entries(path: &str) -> bool {
    match std::fs::read_dir(path) {
        // `Some(Err(_))` is deliberately still "holds work": even learning
        // whether an entry exists has to succeed before removal is safe.
        Ok(mut entries) => entries.next().is_some(),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// The process-global engine, set once at boot by [`build_task_engine`]. Read
/// by the start/cancel/merge/cleanup commands.
pub fn engine() -> Option<Arc<TaskEngine>> {
    ENGINE.get().cloned()
}

pub struct TaskEngine {
    db: AppDatabase,
    manager: ConnectionManager,
    emitter: EventEmitter,
    bus: Arc<InternalEventBus>,
    data_dir: PathBuf,
    /// Live runs: `connection_id -> (task_id, run_seq)` — the only way events
    /// keyed by connection_id map back to a task generation. Lost on restart
    /// (boot reconcile covers that).
    index: Arc<Mutex<HashMap<String, (i32, i32)>>>,
    /// Outstanding blocking requests per task (`"q:<id>"`, `"p:<id>"`,
    /// `"a:<id>"` — namespaced so the three id spaces can't collide). Non-empty
    /// set ⇔ awaiting_input. Requests raised by a delegation sub-agent are
    /// additionally prefixed with the child's connection id
    /// (`"<child_conn>#p:<id>"`) so [`TaskEngine::forget_delegation_child`] can
    /// drop the whole group when that child goes away.
    awaiting: Arc<Mutex<HashMap<i32, HashSet<String>>>>,
    /// `child_connection_id -> parent_connection_id` for delegation children of
    /// a task run. A sub-agent's blocking prompts arrive on the CHILD's
    /// connection, which is not in `index` — without this mapping they are
    /// dropped and the board keeps saying "running" while the run is actually
    /// parked on the user (#447). Populated from `DelegationStarted` (only for
    /// connections that ARE task runs) and dropped on `DelegationCompleted`.
    delegation_parents: Arc<Mutex<HashMap<String, String>>>,
    /// Tasks currently being launched (`queued`, then `preparing` in DB, but
    /// owned by an in-flight launch), mapped to their folder so the pump's
    /// concurrency accounting stays per-folder. Keeps the pump from
    /// double-launching, and is the reconcile sweep's ownership test for
    /// `preparing` rows. Entries carry an ownership token: a slow launch that
    /// is still unwinding must not drop the entry a newer launch of the same
    /// task now depends on.
    launching: Arc<Mutex<HashMap<i32, LaunchOwner>>>,
    /// Source of `LaunchOwner` tokens.
    launch_token: Arc<std::sync::atomic::AtomicU64>,
    /// Live setup (init command) child processes, by task. A cancel kills the
    /// process TREE so a long `pnpm install` stops with the task instead of
    /// running to completion in the background.
    setup_children: Arc<Mutex<HashMap<i32, SetupChild>>>,
    /// Tasks whose launch is currently waiting out a pre-prompt compaction
    /// turn, and the connection running it. Serves the same role
    /// `setup_children` does for a long init command: the launch holds the task
    /// lock across the whole compaction, so a cancel has to be able to reach
    /// the turn WITHOUT that lock — otherwise "stop" would sit behind the very
    /// turn it is trying to stop. The compaction connection is deliberately NOT
    /// in `index`: its `TurnComplete` is the launch's own signal, and an
    /// indexed one would settle the task into review before its work ever
    /// started.
    compacting: Arc<Mutex<HashMap<i32, CompactRun>>>,
    /// `connection_id -> event seq` of a compaction `TurnComplete` the launch
    /// has already consumed as its own signal.
    ///
    /// The bus is a BROADCAST: this engine's own receiver and the launch's
    /// waiter each get their own copy of that envelope. The waiter is awaiting
    /// exactly it, so it always reacts first; the engine's loop awaits DB work
    /// per event and can be arbitrarily far behind. "The connection was not in
    /// `index` when this was emitted" is therefore something `on_event` cannot
    /// observe — by the time it dequeues, the launch may have indexed the
    /// connection and sent the round's real prompt, and the compaction's
    /// completion would settle THAT turn (and disconnect the agent mid-round).
    /// The seq is the discriminator: per-connection and monotonic (assigned
    /// under the state write lock in `emit_with_state_gated`), so it names one
    /// exact envelope rather than "a turn on this connection".
    compaction_turns: Arc<Mutex<HashMap<String, u64>>>,
    /// Tasks whose merge/delivery is executing in THIS process — the reconcile
    /// tick must not run crash recovery against them. A merge only needs it
    /// for the dispatch window (a live agent connection covers the rest); a
    /// delivery has no connection, so it stays in here for its whole run.
    ///
    /// Entries carry an ownership TOKEN, for the same reason `launching` does:
    /// a losing attempt that is still unwinding must not drop the entry the
    /// winner now depends on — that would hand a live delivery to the
    /// reconcile sweep, which would bounce it mid-push.
    merging: Arc<Mutex<HashMap<i32, u64>>>,
    /// Source of `merging` ownership tokens.
    in_flight_token: Arc<std::sync::atomic::AtomicU64>,
    /// Push + pull-request calls, behind a trait so engine tests can drive the
    /// delivery state machine — including every failure point and both
    /// recovery branches — without a network or the OS keyring.
    forge: Arc<dyn ForgeDeliveryApi>,
    /// Per-task lock serializing launch vs cancel teardown (same role as the
    /// automation engine's fire lock).
    task_locks: Arc<Mutex<HashMap<i32, Arc<Mutex<()>>>>>,
    /// Per-project-folder git mutex: merge and worktree cleanup serialize here.
    folder_locks: Arc<Mutex<HashMap<i32, Arc<Mutex<()>>>>>,
    /// Per-folder pump lock so concurrent pumps can't over-launch past
    /// max_concurrent.
    pump_locks: Arc<Mutex<HashMap<i32, Arc<Mutex<()>>>>>,
    /// Held for the engine's lifetime: exclusive advisory lock on
    /// `<db>.tasks.lock`. Its existence proves this process is the sole task
    /// engine on the DB — the precondition for the destructive boot reconcile.
    _engine_lock: std::fs::File,
}

/// Build the engine and publish it to the process global. Fails closed like the
/// automation engine: `None` unless this process holds the exclusive task lock.
pub fn build_task_engine(
    db: AppDatabase,
    manager: ConnectionManager,
    emitter: EventEmitter,
    bus: Arc<InternalEventBus>,
    data_dir: PathBuf,
) -> Option<Arc<TaskEngine>> {
    let engine_lock = match acquire_engine_ownership(&data_dir) {
        Ownership::Exclusive(file) => file,
        Ownership::Taken => {
            tracing::info!(
                "[work_task] another codeg process owns the task engine for {}; \
                 this process will not drive tasks",
                data_dir.display()
            );
            return None;
        }
        Ownership::Unavailable => {
            tracing::warn!(
                "[work_task] could not establish the task engine lock for {}; \
                 tasks are disabled in this process",
                data_dir.display()
            );
            return None;
        }
    };
    let engine = Arc::new(TaskEngine {
        db,
        manager,
        emitter,
        bus,
        data_dir,
        index: Arc::new(Mutex::new(HashMap::new())),
        awaiting: Arc::new(Mutex::new(HashMap::new())),
        delegation_parents: Arc::new(Mutex::new(HashMap::new())),
        launching: Arc::new(Mutex::new(HashMap::new())),
        launch_token: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        setup_children: Arc::new(Mutex::new(HashMap::new())),
        compacting: Arc::new(Mutex::new(HashMap::new())),
        compaction_turns: Arc::new(Mutex::new(HashMap::new())),
        merging: Arc::new(Mutex::new(HashMap::new())),
        in_flight_token: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        forge: Arc::new(crate::forge::deliver::ForgeDelivery),
        task_locks: Arc::new(Mutex::new(HashMap::new())),
        folder_locks: Arc::new(Mutex::new(HashMap::new())),
        pump_locks: Arc::new(Mutex::new(HashMap::new())),
        _engine_lock: engine_lock,
    });
    let _ = ENGINE.set(engine.clone());
    Some(engine)
}

/// Hard stop when walking `TaskEngine::delegation_parents` up to its run. Well
/// above any usable `depth_limit` (the delegation config's, which bounds the
/// real chain), so it only ever fires against a map that shouldn't exist —
/// insurance against spinning, not a functional limit.
const MAX_DELEGATION_CHAIN_HOPS: usize = 16;

/// Build an engine WITHOUT taking the process-wide ownership lock or publishing
/// it to the `ENGINE` cell, so a test can drive the instance methods (event
/// handling, request tracking) directly. The subscriber loop is never started;
/// tests feed `on_event` themselves.
#[cfg(test)]
fn test_engine(db: AppDatabase) -> Arc<TaskEngine> {
    test_engine_with_forge(db, Arc::new(crate::forge::deliver::ForgeDelivery))
}

/// As [`test_engine`], with the forge write path replaced — delivery tests
/// drive push/find/create from a fake so no test ever reaches the network or
/// the credential store.
#[cfg(test)]
fn test_engine_with_forge(db: AppDatabase, forge: Arc<dyn ForgeDeliveryApi>) -> Arc<TaskEngine> {
    test_engine_full(db, forge, EventEmitter::Noop)
}

/// As [`test_engine_with_forge`], with the emitter chosen by the caller — the
/// broadcast-ordering tests need a real `WebEventBroadcaster` behind it to read
/// back what the engine announced, and in what order.
#[cfg(test)]
fn test_engine_full(
    db: AppDatabase,
    forge: Arc<dyn ForgeDeliveryApi>,
    emitter: EventEmitter,
) -> Arc<TaskEngine> {
    Arc::new(TaskEngine {
        db,
        manager: ConnectionManager::new(),
        emitter,
        bus: Arc::new(InternalEventBus::new(Default::default())),
        // An anonymous temp file, not a handle on `data_dir`: opening a
        // DIRECTORY as a File succeeds on Unix but fails on Windows.
        _engine_lock: tempfile::tempfile().expect("temp file"),
        data_dir: std::env::temp_dir(),
        index: Arc::new(Mutex::new(HashMap::new())),
        awaiting: Arc::new(Mutex::new(HashMap::new())),
        delegation_parents: Arc::new(Mutex::new(HashMap::new())),
        launching: Arc::new(Mutex::new(HashMap::new())),
        launch_token: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        setup_children: Arc::new(Mutex::new(HashMap::new())),
        compacting: Arc::new(Mutex::new(HashMap::new())),
        compaction_turns: Arc::new(Mutex::new(HashMap::new())),
        merging: Arc::new(Mutex::new(HashMap::new())),
        in_flight_token: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        forge,
        task_locks: Arc::new(Mutex::new(HashMap::new())),
        folder_locks: Arc::new(Mutex::new(HashMap::new())),
        pump_locks: Arc::new(Mutex::new(HashMap::new())),
    })
}

enum Ownership {
    Exclusive(std::fs::File),
    Taken,
    Unavailable,
}

/// `<db-file>.tasks.lock` — sibling of the automation engine's `<db>.lock`, so
/// the two engines elect independently but both contend exactly when the DB is
/// shared.
fn engine_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{}.tasks.lock", crate::db::database_file_name()))
}

fn acquire_engine_ownership(data_dir: &Path) -> Ownership {
    let path = engine_lock_path(data_dir);
    let file = match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("[work_task] engine lock open failed: {e}");
            return Ownership::Unavailable;
        }
    };
    match file.try_lock() {
        Ok(()) => Ownership::Exclusive(file),
        Err(std::fs::TryLockError::WouldBlock) => Ownership::Taken,
        Err(std::fs::TryLockError::Error(e)) => {
            tracing::warn!("[work_task] engine lock failed: {e}");
            Ownership::Unavailable
        }
    }
}

/// Long-running driver: boot recovery, then a select loop over the event bus +
/// the reconcile tick. Spawn once per process in each boot path.
pub async fn run_task_engine(engine: Arc<TaskEngine>) {
    // Subscribed BEFORE the boot work below, not after it: a broadcast receiver
    // only ever sees what is published after it subscribes, and the boot work
    // is not instant (a git probe per merging row, a re-measure per review
    // row). A task the user starts inside that window would emit its events
    // into a bus nobody is listening to, and a missed `TurnComplete` leaves it
    // running forever — the reconcile sweep deliberately leaves rows whose
    // connection is still live to `on_event`. Anything that piles up while boot
    // finishes is processed right after, which is safe by construction: every
    // settle is a CAS on `(connection_id, run_seq)`, so an event for a
    // generation boot already failed is a no-op. A backlog past the channel's
    // capacity surfaces as `Lagged`, which the loop already handles.
    let mut rx = engine.bus.subscribe();

    // Boot recovery: no connections (and no setup child processes) survive a
    // restart, so queued / preparing / running / awaiting_input are
    // interruptions → failed(interrupted); retry is idempotent (the worktree is
    // reused, and an init command that never finished re-runs — see the setup
    // marker). merging is exempt — it recovers from git truth below, never from
    // connection liveness.
    match work_task_service::boot_reconcile_interrupted(&engine.db.conn).await {
        Ok(n) if n > 0 => {
            tracing::info!("[work_task] boot reconcile failed {n} interrupted task(s)");
            engine.emit_changed_all();
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("[work_task] boot reconcile error: {e}"),
    }
    match work_task_service::list_by_status(&engine.db.conn, &[WorkTaskStatus::Merging]).await {
        Ok(rows) => {
            for row in rows {
                engine.recover_merging(row.id).await;
            }
        }
        Err(e) => tracing::warn!("[work_task] boot merging scan error: {e}"),
    }
    engine.refresh_review_diff_stats().await;

    let mut reconcile = {
        let mut i = tokio::time::interval(Duration::from_secs(RECONCILE_INTERVAL_SECS));
        i.set_missed_tick_behavior(MissedTickBehavior::Delay);
        i
    };
    // Fires immediately on its first tick, which is also the catch-up pass: a
    // plan whose time passed while the app was closed runs late rather than
    // never.
    let mut schedule = {
        let mut i = tokio::time::interval(Duration::from_secs(SCHEDULE_INTERVAL_SECS));
        i.set_missed_tick_behavior(MissedTickBehavior::Delay);
        i
    };
    let mut lag_throttle = LagLogThrottle::new(LAG_LOG_WINDOW);

    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(env) => engine.on_event(&env).await,
                Err(RecvError::Lagged(n)) => {
                    if let Some(s) = lag_throttle.record(n) {
                        tracing::warn!(
                            "[work_task] event bus lagged: dropped {} events across \
                             {} occurrence(s) in the last {}s; reconcile will recover",
                            s.dropped,
                            s.occurrences,
                            LAG_LOG_WINDOW.as_secs()
                        );
                    }
                }
                Err(RecvError::Closed) => break,
            },
            _ = schedule.tick() => engine.claim_due_scheduled().await,
            _ = reconcile.tick() => engine.reconcile_once().await,
        }
    }
}

/// How a task finished, carried to the (spawned) forge write-back. Owned
/// strings because it outlives the settle that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WritebackOutcome {
    /// Landed on the local base branch — the commit it became.
    Merged(String),
    /// Published as a pull request — its URL.
    Delivered(String),
    /// Accepted without landing anything — see `TaskOutcome::Accepted`.
    Accepted { nothing_to_land: bool },
}

impl MergeDispatch {
    pub fn is_queued(self) -> bool {
        matches!(self, MergeDispatch::Queued)
    }
}

/// The queued merge a pump dispatch is FOR, carried from the scan down to the
/// CAS that spends it.
///
/// `raw` is the row's `pending_merge` JSON verbatim — an optimistic token, not
/// a re-serialization: every write that consumes a queued merge demands the
/// column still equal it. Between the scan and the dispatch the pump does a
/// worktree stat, takes the folder lock and runs three git subprocesses, and a
/// user can withdraw or edit the merge anywhere in that window. `run_seq` does
/// not move for either, so without this token a withdrawn merge would still
/// land on the base branch.
struct QueuedMergeClaim {
    raw: String,
    /// The instant the task took its place in line, so a re-park (the slot got
    /// taken first) keeps it rather than going to the back.
    queued_at: chrono::DateTime<chrono::Utc>,
}

/// What one pass over a folder's merge queue concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    /// A merge is running (or was just dispatched / refused) — the folder's one
    /// merge slot is spoken for this round.
    Taken,
    /// Nothing is queued. The slot is free for the auto-merge sweep.
    Empty,
    /// The queue changed while this pass worked on it; the snapshot is out of
    /// date and must be re-read before anything else may use the slot.
    Stale,
}

/// How many times a drain re-reads a queue that changed under it before it
/// gives the slot back to the next pump. Each retry requires a fresh
/// concurrent change, so this only bounds a pathological click loop.
const MERGE_QUEUE_DRAIN_ATTEMPTS: usize = 3;

impl TaskEngine {
    // ── user entry points ───────────────────────────────────────────────────

    /// Manual start: claim todo → queued, then pump the folder.
    pub async fn start(self: &Arc<Self>, task_id: i32) -> Result<(), String> {
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| e.to_string())?;
        self.preflight_folder(task.folder_id).await?;
        match work_task_service::claim_for_run(&self.db.conn, task_id, WorkTaskStatus::Todo, "user")
            .await
            .map_err(|e| e.to_string())?
        {
            Some(_) => {
                self.emit_upsert(task_id);
                self.pump_folder(task.folder_id).await;
                Ok(())
            }
            None => Err("task is not in todo".to_string()),
        }
    }

    /// "Start all": claim every todo of the folder, then pump. With no folder
    /// selected this is the global sweep — every folder that holds todos, each
    /// on its own preflight (an invalid folder is skipped, not fatal).
    pub async fn start_all(self: &Arc<Self>, folder_id: Option<i32>) -> Result<u32, String> {
        let folder_ids = match folder_id {
            Some(id) => vec![id],
            None => work_task_service::folders_with_todos(&self.db.conn)
                .await
                .map_err(|e| e.to_string())?,
        };
        let explicit = folder_id.is_some();
        let mut claimed = 0u32;
        for fid in folder_ids {
            if let Err(e) = self.preflight_folder(fid).await {
                if explicit {
                    return Err(e);
                }
                tracing::info!("[work_task] start all skips folder {fid}: {e}");
                continue;
            }
            let ids = work_task_service::list_todo_ids(&self.db.conn, fid)
                .await
                .map_err(|e| e.to_string())?;
            let mut folder_claimed = 0u32;
            for id in ids {
                // The unplanned-only claim, not the generic one: `ids` is a
                // snapshot, and a plan set in the meantime must still be
                // honoured rather than silently overridden by a bulk button.
                if work_task_service::claim_unplanned_for_run(
                    &self.db.conn,
                    id,
                    WorkTaskStatus::Todo,
                    "user",
                )
                .await
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    folder_claimed += 1;
                    self.emit_upsert(id);
                }
            }
            if folder_claimed > 0 {
                self.pump_folder(fid).await;
            }
            claimed += folder_claimed;
        }
        Ok(claimed)
    }

    /// Retry a failed task: claim failed → queued (same worktree / session
    /// reused by the launch), then pump. An optional note rides the claim's own
    /// transaction and reaches the retry prompt — a failure usually has a cause
    /// the user knows and the agent doesn't.
    pub async fn retry(
        self: &Arc<Self>,
        task_id: i32,
        note: Option<String>,
        attachments: Vec<serde_json::Value>,
        allow_duplicate_source: bool,
    ) -> Result<(), String> {
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| e.to_string())?;
        self.preflight_folder(task.folder_id).await?;
        let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        // An attachment is an instruction on its own: a screenshot with no
        // sentence still has to reach the retry prompt, so the action is
        // recorded whenever EITHER part is present.
        let action = (note.is_some() || !attachments.is_empty()).then(|| {
            serde_json::json!({
                "action": "retry",
                "note": note.unwrap_or_default(),
                "blocks": attachments,
            })
        });
        match work_task_service::claim_for_run_with_action(
            &self.db.conn,
            task_id,
            WorkTaskStatus::Failed,
            "user",
            action,
            allow_duplicate_source,
        )
        .await
        .map_err(|e| e.to_string())?
        {
            Some(_) => {
                self.emit_upsert(task_id);
                self.pump_folder(task.folder_id).await;
                Ok(())
            }
            None => Err("task is not in failed".to_string()),
        }
    }

    /// Send a reviewed task back to the agent with a follow-up. Launches
    /// directly (explicit user action — does not wait behind the queue).
    ///
    /// The intent picks the wording the agent receives; the feedback itself is
    /// recorded inside the claim's transaction, so a pump that steals the
    /// freshly queued generation still finds the instruction.
    pub async fn return_task(
        self: &Arc<Self>,
        task_id: i32,
        intent: FollowUpIntent,
        feedback: String,
        attachments: Vec<serde_json::Value>,
    ) -> Result<(), String> {
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| e.to_string())?;
        self.preflight_folder(task.folder_id).await?;
        let Some(_) = work_task_service::claim_for_run_with_action(
            &self.db.conn,
            task_id,
            WorkTaskStatus::Review,
            "user",
            Some(serde_json::json!({
                "action": "return",
                "intent": intent.as_str(),
                "feedback": feedback,
                "blocks": attachments,
            })),
            false,
        )
        .await
        .map_err(|e| e.to_string())?
        else {
            return Err("task is not in review".to_string());
        };
        self.emit_upsert(task_id);
        self.spawn_launch(
            task_id,
            task.folder_id,
            LaunchMode::Return {
                intent,
                feedback,
                attachments,
            },
        );
        Ok(())
    }

    /// Cancel a task from any non-terminal state except merging. The worktree
    /// is kept — removing it is a separate call the callers chain on when the
    /// user asked for it (`work_task_cancel_core`'s checkbox, the delete path),
    /// and it has to come after this one returns: the removal refuses while a
    /// live agent connection is still on the task, and shedding that connection
    /// is the last thing we do below. `reason` is the user's own note for the
    /// timeline; internal cancels (a conversation the user stopped from the
    /// chat UI, a delete) pass None.
    pub async fn cancel(
        self: &Arc<Self>,
        task_id: i32,
        reason: Option<String>,
    ) -> Result<(), String> {
        let won = work_task_service::cancel(&self.db.conn, task_id, reason.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        if !won {
            return Err("task cannot be canceled in its current state".to_string());
        }
        self.emit_upsert(task_id);

        // Stop a running init command / pre-prompt compaction BEFORE waiting on
        // the task lock: the launch holds that lock for its whole setup, so
        // waiting first would mean waiting out the very `pnpm install` — or the
        // very compaction turn — we are trying to stop. The run_seq we just
        // canceled scopes both to this generation (cancel does not bump it, so
        // the row still carries it).
        if let Ok(task) = work_task_service::get_model(&self.db.conn, task_id).await {
            self.kill_setup_child(task_id, task.run_seq).await;
            self.abort_compaction(task_id, task.run_seq).await;
        }

        // Serialize the teardown with a possibly in-flight launch: the launch
        // holds the task lock across spawn → prompt, and its status gates
        // re-read after each step — so we tear down either before the prompt
        // (gate aborts) or after the turn is truly in flight (manager.cancel
        // aborts a real turn), never interleaved with the prompt enqueue.
        let lock = self.task_lock(task_id).await;
        let _guard = lock.lock().await;

        let conn_id = {
            self.index
                .lock()
                .await
                .iter()
                .find(|(_, (tid, _))| *tid == task_id)
                .map(|(c, _)| c.clone())
        };
        if let Some(conn_id) = conn_id {
            let _ = self.manager.cancel(&self.db.conn, &conn_id).await;
            self.forget_connection(&conn_id).await;
            self.forget_delegation_children_of(&conn_id).await;
            let _ = self.manager.disconnect(&conn_id).await;
        }
        self.awaiting.lock().await.remove(&task_id);

        // Converge a stranded InProgress conversation.
        let task = work_task_service::get_model(&self.db.conn, task_id).await.ok();
        if let Some(conv_id) = task.as_ref().and_then(|t| t.conversation_id) {
            if self.conversation_status(conv_id).await == Some(ConversationStatus::InProgress) {
                self.cancel_conversation(conv_id).await;
            }
        }
        // The slot freed — refill from the queue (and an auto folder's todo).
        if let Some(folder_id) = task.map(|t| t.folder_id) {
            self.pump_folder(folder_id).await;
        }
        Ok(())
    }

    // ── scheduler ───────────────────────────────────────────────────────────

    /// Queue every to-do task whose planned start has arrived, then pump the
    /// folders that gained one. Runs on its own tick and also as a nudge right
    /// after a plan is set, so a time already in the past takes effect at once.
    ///
    /// Claiming does not launch: the task lands in `queued` like any manual
    /// start, and the folder's concurrency limit still governs what actually
    /// runs.
    pub async fn claim_due_scheduled(self: &Arc<Self>) {
        let claimed =
            match work_task_service::claim_due_scheduled(&self.db.conn, chrono::Utc::now()).await {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!("[work_task] scheduled claim error: {e}");
                    return;
                }
            };
        if claimed.is_empty() {
            return;
        }
        let mut folders: Vec<i32> = Vec::new();
        for (task_id, folder_id) in claimed {
            tracing::info!("[work_task] scheduled start claimed task {task_id}");
            self.emit_upsert(task_id);
            if !folders.contains(&folder_id) {
                folders.push(folder_id);
            }
        }
        for folder_id in folders {
            self.pump_folder(folder_id).await;
        }
    }

    // ── pump ────────────────────────────────────────────────────────────────

    /// Launch queued tasks of a folder up to its `max_concurrent` (0 =
    /// unlimited). Serialized per folder so concurrent pumps can't over-launch.
    pub async fn pump_folder(self: &Arc<Self>, folder_id: i32) {
        let lock = {
            let mut locks = self.pump_locks.lock().await;
            locks
                .entry(folder_id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        let settings = work_task_service::settings_get_effective(&self.db.conn, folder_id)
            .await
            .unwrap_or_default();
        let max = settings.max_concurrent.max(0) as u64;

        // Scheduler arm: an auto_process folder claims todo heads into the
        // queue until the budget (which counts queued) is spent; the drain
        // loop below then launches them like any manually queued task.
        if settings.auto_process {
            loop {
                match work_task_service::auto_claim_next(
                    &self.db.conn,
                    folder_id,
                    settings.max_concurrent,
                )
                .await
                {
                    Ok(Some(id)) => self.emit_upsert(id),
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("[work_task] auto claim error: {e}");
                        break;
                    }
                }
            }
        }

        loop {
            // Only THIS folder's in-flight launches count against its limit.
            let launching: Vec<i32> = self
                .launching
                .lock()
                .await
                .iter()
                .filter(|(_, owner)| owner.folder_id == folder_id)
                .map(|(tid, _)| *tid)
                .collect();
            let active = match work_task_service::active_launched_count(&self.db.conn, folder_id)
                .await
            {
                Ok(n) => n + launching.len() as u64,
                Err(e) => {
                    tracing::warn!("[work_task] pump count error: {e}");
                    return;
                }
            };
            if max != 0 && active >= max {
                return;
            }
            let next = match work_task_service::next_queued(&self.db.conn, folder_id, &launching)
                .await
            {
                Ok(Some(t)) => t,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!("[work_task] pump next error: {e}");
                    return;
                }
            };
            // Claimed synchronously: this loop iterates immediately, and the
            // task must already read as in-flight when it does.
            let token = self.claim_launch_slot(next.id, folder_id).await;
            self.spawn_launch_owned(next.id, folder_id, launch_mode_for(&next), Some(token));
        }
    }

    /// Mark `task_id` as owned by a new launch and return the ownership token.
    async fn claim_launch_slot(&self, task_id: i32, folder_id: i32) -> u64 {
        let token = self
            .launch_token
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.launching
            .lock()
            .await
            .insert(task_id, LaunchOwner { folder_id, token });
        token
    }

    /// Give up ownership — but only if the entry is still ours. A launch that
    /// is slowly unwinding must not drop the slot a NEWER launch of the same
    /// task now holds, or the reconcile sweep would requeue a live setup and
    /// the folder's accounting would lose a slot.
    async fn release_launch_slot(&self, task_id: i32, token: u64) {
        let mut launching = self.launching.lock().await;
        if launching.get(&task_id).is_some_and(|o| o.token == token) {
            launching.remove(&task_id);
        }
    }

}


pub(crate) mod launch;
pub(crate) mod compaction;
pub(crate) mod outstanding;
pub(crate) mod tools;
pub(crate) mod worktree;
pub(crate) mod events;
pub(crate) mod progress;
pub(crate) mod merge;
pub(crate) mod deliver;
pub(crate) mod pump;
pub(crate) mod cleanup;

pub use self::cleanup::*;
pub use self::tools::*;
use self::compaction::*;
// Only the engine's own tests reach into `deliver` directly; the lib build has
// no unqualified user, so the glob is scoped to the test build (an unscoped one
// is an unused-import warning).
#[cfg(test)]
use self::deliver::*;
use self::launch::*;
use self::merge::*;
use self::outstanding::*;
pub(crate) use self::progress::*;
use self::worktree::*;

#[cfg(test)]
mod tests;

