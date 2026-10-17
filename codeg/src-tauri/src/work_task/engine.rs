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

/// Why [`TaskEngine::cleanup_task`] did not remove a worktree.
///
/// The two answers are not interchangeable to `work_task_delete_core`, which
/// tombstones the task immediately afterwards. A cleanup that TRIED AND FAILED
/// leaves stale bookkeeping behind and the delete may proceed over it. A
/// cleanup that was REFUSED to protect work must stop it: the tombstone takes
/// the card, its `cleanup_state` and its retry entry all at once, stranding a
/// directory the user was told would be deleted with nothing left anywhere to
/// say why — the failure mode a plain `Err` swallowed by a `warn!` produces.
#[derive(Debug)]
pub struct CleanupBlocked {
    pub message: String,
    /// The worktree was left standing deliberately, and still holds work.
    pub holds_work: bool,
}

impl CleanupBlocked {
    /// Tried, could not finish. Callers may proceed past it.
    fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            holds_work: false,
        }
    }
}

static ENGINE: OnceLock<Arc<TaskEngine>> = OnceLock::new();

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

/// What a merge request actually did. Merges into one base branch are serial,
/// so a click that finds the folder's slot busy takes a place in line instead
/// of failing — and the caller has to be able to tell the user which happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDispatch {
    /// The merge generation is running now.
    Dispatched,
    /// Parked on the row; the folder's merge pump dispatches it when the slot
    /// frees.
    Queued,
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

/// How a launch composes its prompt.
enum LaunchMode {
    /// First run: the task's own prompt blocks.
    Fresh,
    /// Retry after failure: resume the session if possible and ask to continue.
    Retry,
    /// A follow-up on a reviewed task: the user's text, framed by their intent,
    /// plus whatever the composer attached out of band (images, pasted bytes).
    Return {
        intent: FollowUpIntent,
        feedback: String,
        attachments: Vec<serde_json::Value>,
    },
    /// Merge generation: the agent lands the task onto the base branch itself
    /// (sync base into the worktree, resolve conflicts, merge into base). The
    /// task sits in `merging` for the whole turn; the engine settles from git
    /// truth, never from the agent's word.
    Merge {
        root_path: String,
        base_branch: String,
        work_branch: String,
        /// "squash" | "merge"
        strategy: String,
        /// `None` → the agent writes the commit message itself.
        message: Option<String>,
        /// Whatever the user added in the merge dialog, verbatim. `None` when
        /// they left it empty — which is every merge dispatched before this
        /// existed, and the auto-merge sweep's every landing.
        instructions: Option<String>,
    },
}

impl LaunchMode {
    /// The task status a launch of this mode expects to find when it starts.
    fn expected_status(&self) -> WorkTaskStatus {
        match self {
            LaunchMode::Merge { .. } => WorkTaskStatus::Merging,
            _ => WorkTaskStatus::Queued,
        }
    }

    /// The status the task holds for the REST of the launch, once setup has
    /// begun — what the cancel gates re-check. A merge generation stays
    /// `merging` throughout; every other mode moves `queued → preparing` before
    /// it touches the worktree.
    fn in_flight_status(&self) -> WorkTaskStatus {
        match self {
            LaunchMode::Merge { .. } => WorkTaskStatus::Merging,
            _ => WorkTaskStatus::Preparing,
        }
    }

    /// Timeline `round` label for the prompt this mode composes. Every
    /// follow-up intent shares the `return` stage: the folder's per-stage
    /// prompt settings stay four stages wide, and the intent rides the `round`
    /// event beside this label for the transcript's phase divider.
    fn round_kind(&self) -> &'static str {
        match self {
            LaunchMode::Fresh => "work",
            LaunchMode::Retry => "retry",
            LaunchMode::Return { .. } => "return",
            LaunchMode::Merge { .. } => "merge",
        }
    }

    /// The follow-up intent this launch carries, for the `round` marker.
    fn round_intent(&self) -> Option<FollowUpIntent> {
        match self {
            LaunchMode::Return { intent, .. } => Some(*intent),
            _ => None,
        }
    }

    /// Whether this launch may write to the worktree. A question is answered in
    /// chat; everything else is work.
    fn is_read_only(&self) -> bool {
        matches!(
            self,
            LaunchMode::Return {
                intent: FollowUpIntent::Question,
                ..
            }
        )
    }
}

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

struct WorktreeRef {
    folder_id: i32,
    path: String,
}

/// Converge DB + clients once a task worktree is off disk. Split out of
/// [`TaskEngine::remove_worktree_locked`] so the ordering contract below is
/// unit-testable without a real git worktree.
///
/// Write order: conversations first (never orphaned under a vanishing folder),
/// then its tabs, then the folder row. Each step ends in a broadcast, because a
/// removal no client hears about is a removal no client shows: the sidebar keeps
/// the dead worktree until the next full `fetchFolders` (i.e. a reload).
async fn converge_worktree_removal(
    db: &AppDatabase,
    emitter: &EventEmitter,
    task_id: i32,
    wt_id: i32,
    project_folder_id: i32,
    wt_path: &str,
) {
    let moved = conversation_service::reparent_folder_conversations(
        &db.conn,
        wt_id,
        project_folder_id,
        wt_path,
    )
    .await
    .unwrap_or(0);

    match tab_service::delete_folder_tabs_and_bump(&db.conn, wt_id).await {
        Ok(inv) => {
            if let Some(tabs) = inv.emit {
                emit_event(
                    emitter,
                    crate::web::event_bridge::TABS_CHANGED_EVENT,
                    crate::web::event_bridge::TabsChanged {
                        version: inv.version,
                        origin: "server".to_string(),
                        tabs,
                    },
                );
            }
        }
        Err(e) => tracing::warn!("[work_task] tab cleanup failed for folder {wt_id}: {e}"),
    }

    // Announce the folder drop only when the row really is gone, so a failed
    // write can't leave clients disagreeing with what a refetch would return.
    let folder_gone = match folder::Entity::find_by_id(wt_id).one(&db.conn).await {
        Ok(Some(row)) => {
            let now = chrono::Utc::now();
            let mut active = row.into_active_model();
            active.is_open = Set(false);
            active.deleted_at = Set(Some(now));
            active.updated_at = Set(now);
            match active.update(&db.conn).await {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!("[work_task] worktree folder {wt_id} soft-delete failed: {e}");
                    false
                }
            }
        }
        // Already soft-deleted / never there: a client still holding it is stale.
        Ok(None) => true,
        Err(e) => {
            tracing::warn!("[work_task] worktree folder {wt_id} lookup failed: {e}");
            false
        }
    };

    let _ = work_task_service::clear_worktree(&db.conn, task_id).await;
    let _ = work_task_service::record_event(
        &db.conn,
        task_id,
        "user_action",
        "engine",
        Some(serde_json::json!({ "action": "cleanup", "reparented": moved })),
    )
    .await;

    // Conversations first: this starts every client's refetch, so the moved
    // rows have the shortest possible window with no folder to render under.
    emit_event(
        emitter,
        crate::web::event_bridge::CONVERSATIONS_BULK_CHANGED_EVENT,
        crate::web::event_bridge::ConversationsBulkChanged {
            imported: 0,
            updated: moved as u32,
            folder_ids: vec![project_folder_id],
        },
    );
    if folder_gone {
        emit_folder_deleted(emitter, wt_id);
    }
}

/// The commit THIS TASK's own work sits on top of: the point its worktree was
/// checked out at, which is also the point an acceptance would deliver from.
/// The counters on the row and every "is there anything to land" probe measure
/// from here — deliberately not from the review baseline, which answers a
/// different question ("what is under review", pull request included).
///
/// For an ordinary task the two coincide: the branch was minted at the recorded
/// base, so "diff against the base" and "what the agent did" are one set. Two
/// things move the anchor off it:
///
/// - **A task triggered from a pull request starts ON that pull request's
///   head**, while its base is the MERGE BASE — because the review is meant to
///   show the pull request plus whatever the agent did (see
///   [`TaskEngine::pr_checkout_point`]). Measuring the task's own work from
///   there would credit it with every file the pull request itself changed: a
///   review-only task that committed nothing would report a full change set,
///   the board would offer to push it back (a delivery with nothing in it),
///   and "complete" — the acceptance that actually applies — would never
///   appear. The head comes from the task's source snapshot, which is what the
///   checkout used; it must still be a commit HERE, since a force-push can
///   leave the row naming one this repository never had.
/// - **The branch absorbing base content** (the agent merged the base branch
///   in, or rebased onto it) moves the merge base forward, and everything the
///   base contributed would otherwise read as this task's work. Taken only
///   when it moved FORWARD from the recorded base: a base branch that is
///   behind locally, or a same-name branch of unrelated history, must not drag
///   the anchor backwards and inflate the count it was meant to correct.
///
/// `wt_path` is the task's worktree — where every ref here is resolved.
async fn own_work_anchor(
    wt_path: &str,
    task: &crate::db::entities::work_task::Model,
) -> Option<String> {
    if task.source_kind.as_deref() == Some(SOURCE_KIND_PR) {
        let head = task
            .source_meta
            .as_deref()
            .and_then(|s| serde_json::from_str::<ForgeSourceMeta>(s).ok())
            .and_then(|m| m.head_sha)
            .filter(|s| !s.trim().is_empty());
        if let Some(head) = head {
            if task_git::commit_present(wt_path, &head).await.unwrap_or(false) {
                return Some(head);
            }
        }
    }
    let recorded = task.base_sha.clone().or_else(|| task.base_branch.clone());
    let Some(base_branch) = task.base_branch.as_deref() else {
        return recorded;
    };
    let Ok(Some(tip)) = task_git::local_branch_tip(wt_path, base_branch).await else {
        return recorded;
    };
    let Ok(merge_base) = task_git::merge_base(wt_path, &tip, "HEAD").await else {
        return recorded;
    };
    match task.base_sha.as_deref() {
        Some(sha) => task_git::is_ancestor(wt_path, sha, &merge_base)
            .await
            .unwrap_or(false)
            .then_some(merge_base)
            .or(recorded),
        // No recorded sha to move forward from — the merge base is strictly
        // better than the branch NAME this would otherwise diff against, which
        // measures against its tip and calls the base's own progress a change.
        None => Some(merge_base),
    }
}

/// `(files, additions, deletions)` of the worktree against `anchor`,
/// uncommitted work included — nothing forces the agent to commit before the
/// task lands in review, and a task whose new files are still untracked has
/// done work all the same.
async fn diff_stats_from(wt_path: &str, anchor: &str) -> Option<(i32, i32, i32)> {
    let files = task_git::diff_numstat_with_untracked(wt_path, anchor)
        .await
        .ok()?;
    let adds: i32 = files.iter().map(|f| f.additions).sum();
    let dels: i32 = files.iter().map(|f| f.deletions).sum();
    Some((files.len() as i32, adds, dels))
}

/// Row-level auto-merge eligibility: everything the sweep can decide without
/// touching git. Mirrors what the board offers — a task whose merge button the
/// user would not see (nothing to land) or would not press yet (red or pending
/// preflight, an error banner from an earlier attempt) is not auto-merged.
///
/// - `files_changed == 0` is the "complete" button's territory: dispatching a
///   merge generation there would land nothing and bounce. `None` (stats
///   unreadable) merges, exactly like the UI defaults to the merge button.
/// - With a preflight configured, only a green light qualifies; `None` means
///   the light is not written yet (the post-preflight hook re-sweeps) and a
///   red light waits for the user. Without one, there is no gate.
/// - `last_error` present = an earlier merge failed or was refused; retrying
///   unattended would loop, so the row waits for the user (any claim or a
///   fresh dispatch clears it).
fn auto_merge_candidate(
    task: &crate::db::entities::work_task::Model,
    settings: &WorkTaskFolderSettings,
) -> bool {
    if task.status != WorkTaskStatus::Review {
        return false;
    }
    // Forge-sourced tasks never auto-merge: their prompt embeds text authored
    // by arbitrary external users, so landing without a human review would be
    // an injection -> main-branch pipeline. A hard rule, deliberately not a
    // setting — the user's explicit merge click on the card is unaffected.
    if task.source_kind.is_some() {
        return false;
    }
    if task.last_error.is_some() {
        return false;
    }
    // A merge the user queued is theirs to land, with the commit message and
    // the worktree choice THEY picked. The unattended dispatch has neither, so
    // the queue's own drain owns this row (and clears the intent in the same
    // CAS that starts the merge, which makes the row a normal candidate again).
    //
    // Any value in the column counts, parseable or not: the authoritative gate
    // is `begin_merge`'s `PendingMerge IS NULL` filter, SQL cannot parse the
    // JSON, and a looser predicate here would just dispatch into a CAS that
    // misses every time — a sweep spinning on a row it can never land.
    if task.pending_merge.is_some() {
        return false;
    }
    if task.files_changed == Some(0) {
        return false;
    }
    if preflight_configured(settings) {
        let passed = task
            .preflight
            .as_deref()
            .and_then(|s| serde_json::from_str::<WorkTaskPreflight>(s).ok())
            .is_some_and(|p| p.status == "passed");
        if !passed {
            return false;
        }
    }
    true
}

/// Whether the folder's settings would make `run_preflight` attempt a command
/// at all — the sweep's gate must not wait for a light that will never be
/// written.
fn preflight_configured(settings: &WorkTaskFolderSettings) -> bool {
    settings
        .preflight_command
        .as_deref()
        .map(str::trim)
        .is_some_and(|c| !c.is_empty())
        || settings.preflight_command_id.is_some()
}

/// Why a queue-bound CAS missed. With a claim it can be either "the row left
/// review" or "this queued merge is no longer the one on the row" (withdrawn,
/// or edited into a different one) — the pump treats both the same way, and
/// both are races it should lose quietly rather than banner.
fn missed_queue_cas(claim: Option<&QueuedMergeClaim>) -> String {
    match claim {
        Some(_) => "the queued merge was changed or withdrawn before it could start".to_string(),
        None => "task left review before the merge was queued".to_string(),
    }
}

/// The order the folder's merge queue drains in: oldest request first, task id
/// breaking a tie. Mirrored client-side by `mergeQueueRanks`, so the place in
/// line a card shows is the order the pump actually dispatches in.
fn queue_order(
    intent: &WorkTaskQueuedMerge,
    task: &crate::db::entities::work_task::Model,
) -> (chrono::DateTime<chrono::Utc>, i32) {
    (intent.queued_at, task.id)
}

/// Merge-dispatch refusals that mean "someone else is (or just was) handling
/// this" rather than "this merge cannot work": the losing side of a race with
/// a click, another sweep or a user action. Matched on `merge_task`'s own
/// wording (same file, a screen up) — these are skipped silently, because the
/// next settle re-sweeps, while every other refusal banners the row.
fn is_benign_merge_race(error: &str) -> bool {
    error.contains("not in review")
        || error.contains("left review")
        || error.contains("already merging")
        || is_queued_merge_superseded(error)
}

/// The one benign race the drain cannot simply step over: the user withdrew or
/// edited the very merge it was dispatching (see [`missed_queue_cas`]). Their
/// word replaces the whole snapshot — a re-read, not a skip.
fn is_queued_merge_superseded(error: &str) -> bool {
    error.contains("changed or withdrawn")
}

/// What a refused push-back most likely means, read off git's own words.
///
/// Deliberately narrow. Whatever this returns is printed to a human as advice,
/// so the only two shapes recognised are the ones git states plainly, and
/// everything else is `Unknown` — which prints no advice at all. An unhelpful
/// message costs a reader a moment; a confident wrong one sends them to change
/// a setting that was never the problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushRefusal {
    /// The remote refused the account: no write access, or a fork whose author
    /// did not allow maintainer edits.
    Permission,
    /// The branch has commits the task's branch does not, so a fast-forward
    /// push cannot apply. Routine on a long-lived pull request: the author
    /// pushed, or merged the base branch in, after this task was triggered.
    BranchMoved,
    Unknown,
}

/// Match on git's stderr, which reaches here inside the delivery error string.
/// Permission is tested first: a forge that refuses the push outright can also
/// mention "rejected", and being told the wrong one of these two is precisely
/// the failure this classification exists to stop.
fn classify_push_refusal(error: &str) -> PushRefusal {
    let lower = error.to_lowercase();
    // The status codes are matched in the phrasing git and curl actually
    // print, not as bare digits: "403" on its own also matches a branch called
    // `fix/403-page` or a repository named after an issue number, and the whole
    // point of this function is to stop it handing out confident wrong advice.
    let permission = [
        "permission to",
        "denied to",
        "error: 403",
        "error: 401",
        "http 403",
        "http 401",
        "status code 403",
        "status code 401",
        "authentication failed",
        "write access",
        "read-only",
        "protected branch",
        "pre-receive hook declined",
    ];
    if permission.iter().any(|needle| lower.contains(needle)) {
        return PushRefusal::Permission;
    }
    let moved = [
        "non-fast-forward",
        "fetch first",
        "updates were rejected",
        "[rejected]",
        "behind its remote",
        "stale info",
    ];
    if moved.iter().any(|needle| lower.contains(needle)) {
        return PushRefusal::BranchMoved;
    }
    PushRefusal::Unknown
}

/// The repository a pull-request task's push-back lands in: the HEAD
/// repository recorded at trigger time — the fork, when the pull request comes
/// from one. A row that recorded none falls back to the source repository:
/// builds that predate the field refused forks at trigger, so their rows are
/// same-repo by construction. `Err` is the one head codeg cannot push to ever —
/// a fork it cannot name (GitLab's unresolved `project-{id}` placeholder, or a
/// fork deleted since GitHub hydrated the row).
fn pull_push_repo(meta: &ForgeSourceMeta) -> Result<String, String> {
    let recorded = meta
        .head_repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or(&meta.owner_repo);
    crate::forge::normalize_repo(recorded).ok_or_else(|| {
        format!(
            "{} #{} comes from a fork whose repository codeg cannot see (it may be private or \
             deleted), so there is nowhere to push the work back to — its commits stay on the \
             task's local branch",
            meta.provider.change_noun(),
            meta.number
        )
    })
}

/// Pick the launch mode for a pump-driven launch from the task's history: a
/// task with a prior conversation continues (retry semantics); one with no
/// session to continue starts fresh. Explicit returns launch directly with
/// `LaunchMode::Return`.
///
/// The bound session IS the whole criterion, which is what lets a transition
/// decide how the next start behaves: `retry` (failed → queued) keeps the link
/// and therefore continues, while `requeue_canceled` drops it so a task the
/// user put back on the board runs from the top. `compose_prompt` must not
/// read `Fresh` as "this task has no history" — see its doc.
fn launch_mode_for(task: &crate::db::entities::work_task::Model) -> LaunchMode {
    if task.conversation_id.is_some() {
        LaunchMode::Retry
    } else {
        LaunchMode::Fresh
    }
}

/// The branch the task was created FOR, if its config names one. A blank
/// choice is no choice: the editor writes the empty string for "the project
/// folder's current branch", which is also what every task predating the
/// field carries.
fn chosen_base_branch(task: &crate::db::entities::work_task::Model) -> Option<String> {
    serde_json::from_str::<WorkTaskConfig>(&task.config)
        .ok()?
        .base_branch
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
}

/// Layered agent config: task override wins wholesale; else the folder's task
/// settings; else the folder's default agent with no extra options.
fn effective_agent_config(
    cfg: &WorkTaskConfig,
    settings: &WorkTaskFolderSettings,
    root: &crate::models::FolderDetail,
) -> (
    Option<String>,
    Option<String>,
    std::collections::BTreeMap<String, String>,
) {
    if let Some(agent) = cfg.agent_type.clone() {
        return (Some(agent), cfg.mode_id.clone(), cfg.config_values.clone());
    }
    if let Some(agent) = settings.default_agent_type.clone() {
        return (
            Some(agent),
            settings.mode_id.clone(),
            settings.config_values.clone(),
        );
    }
    let folder_default = root
        .default_agent_type
        .as_ref()
        .and_then(|a| serde_json::to_value(a).ok())
        .and_then(|v| v.as_str().map(String::from));
    (
        folder_default,
        settings.mode_id.clone(),
        settings.config_values.clone(),
    )
}

/// Compose the prompt for a launch mode. Fresh runs replay the task's blocks.
/// Retry/return compose against the session we actually got: a resumed session
/// already carries the task context, while a fresh fallback session needs the
/// full original description again. Every prompt ends with the worktree guard,
/// then with whatever the folder's settings add for this stage.
///
/// "Fresh" is a mode, NOT a promise that the task is pristine: `requeue_canceled`
/// drops the session link so a re-queued task starts over, which sends a task
/// that already ran — worktree, branch and all — back through the fresh arm. Both
/// replaying arms therefore read the instruction log rather than the mode.
async fn compose_prompt(
    cfg: &WorkTaskConfig,
    task: &crate::db::entities::work_task::Model,
    mode: &LaunchMode,
    settings: &WorkTaskFolderSettings,
    resumed: bool,
    conn: &sea_orm::DatabaseConnection,
) -> Result<Vec<PromptInputBlock>, String> {
    let mut blocks: Vec<PromptInputBlock> = Vec::new();
    let original: Vec<PromptInputBlock> = cfg
        .prompt_blocks
        .iter()
        .map(|v| serde_json::from_value::<PromptInputBlock>(v.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("bad prompt blocks: {e}"))?;

    // Whether this launch is (re)doing the task's ORIGINAL work order, as
    // opposed to follow-up work the user asked for later. The worktree guard
    // below keys its licence on this: a report-deliverable task forbids code
    // changes on its original order, but a returned "now apply the fix" turn
    // is precisely a change order and must get the normal write licence.
    let mut original_work_order = false;
    // A replay standing in for an unanswered question: it already says "do not
    // change any files for it", so the guard must not hand back the commit
    // grant three blocks later.
    let mut replayed_question = false;

    match mode {
        LaunchMode::Fresh => {
            if original.is_empty() {
                return Err("prompt is empty".to_string());
            }
            // Read the log, not the mode. A fresh launch used to mean a task
            // with no history at all; since a requeue drops the session link it
            // also means "run this again from the top", and that task may owe
            // the user an instruction its stopped generation never answered —
            // a returned "rework this" is silently lost otherwise, and a
            // returned question comes back as a work order with a full commit
            // grant. Same two reads as the retry arm, for the same reasons.
            let scan = instruction_scan(conn, task.id).await;
            original_work_order = scan.interrupted.is_none();
            replayed_question = matches!(scan.interrupted, Some(FollowUpIntent::Question));
            // Only when a generation actually got as far as its own turn
            // (`started_at` is written by `mark_running` and never cleared): a
            // task canceled during setup has nothing in its worktree to warn
            // about, and its prompt stays byte-identical to a first run's.
            if task.started_at.is_some() {
                blocks.push(PromptInputBlock::Text {
                    text: "You are running this task again from the top: an earlier run was \
                           stopped and the task was put back on the board, so this session \
                           carries none of that run's context. Its worktree may still hold \
                           that run's work — check the current state first and build on \
                           whatever is already there instead of redoing it. The original task \
                           was:"
                        .to_string(),
                });
            }
            blocks.extend(original);
            push_replay(&mut blocks, &scan, resumed, task.id);
        }
        LaunchMode::Retry => {
            blocks.push(PromptInputBlock::Text {
                text: "The previous run of this task was interrupted. Continue working in \
                       this worktree and complete the task."
                    .to_string(),
            });
            if !resumed {
                blocks.push(PromptInputBlock::Text {
                    text: "The original task follows for reference:".to_string(),
                });
                blocks.extend(original);
            }
            // Replay whatever instruction the interrupted generation still
            // owed the user, framed the way they meant it — an unanswered
            // question must not come back as a work order.
            let scan = instruction_scan(conn, task.id).await;
            // A retry stands in for whichever turn was interrupted, and the
            // licence follows THAT turn — `interrupted`, not the newest note:
            // a retry/requeue note refines the turn it interrupts (a hint, a
            // screenshot), it does not change what kind of turn it was. With
            // no unsettled follow-up underneath, this is the original order
            // again.
            original_work_order = scan.interrupted.is_none();
            replayed_question = matches!(scan.interrupted, Some(FollowUpIntent::Question));
            push_replay(&mut blocks, &scan, resumed, task.id);
        }
        LaunchMode::Return {
            intent,
            feedback,
            attachments,
        } => {
            if !resumed {
                // Session resume failed — the fresh session has no context, so
                // replay the task before the feedback.
                blocks.push(PromptInputBlock::Text {
                    text: "You are picking up a task whose previous session could not be \
                           resumed. The worktree already contains that session's work. \
                           The original task was:"
                        .to_string(),
                });
                blocks.extend(original);
            }
            blocks.push(PromptInputBlock::Text {
                text: follow_up_text(*intent, feedback),
            });
            blocks.extend(attachment_blocks(attachments, task.id));
        }
        LaunchMode::Merge {
            root_path,
            base_branch,
            work_branch,
            strategy,
            message,
            instructions,
        } => {
            if !resumed {
                blocks.push(PromptInputBlock::Text {
                    text: "You are picking up a task whose previous session could not be \
                           resumed. The worktree already contains that session's work. \
                           The original task was:"
                        .to_string(),
                });
                blocks.extend(original);
            }
            let land_command = if strategy == "merge" {
                format!(
                    "git -C \"{root_path}\" merge --no-ff -m \"<message>\" {work_branch}"
                )
            } else {
                format!(
                    "git -C \"{root_path}\" merge --squash {work_branch} && \
                     git -C \"{root_path}\" commit -m \"<message>\""
                )
            };
            // Indented under step 3 rather than left dangling after it: it says
            // what to substitute for that step's `<message>`, and read as a
            // fourth line it looked like a fourth instruction.
            let message_rule = match message {
                Some(m) => format!("   Use exactly this commit message:\n   {m}"),
                None => "   For `<message>`, write a concise Conventional Commits \
                         message yourself, summarizing what this task changed."
                    .to_string(),
            };
            // The user's own words get their own paragraph, before the closing
            // rules — those rules stay last on purpose (same reason the standing
            // worktree guard below is the last built-in block): whatever the
            // user asked for, it does not license a push or a self-deletion.
            let extra = match instructions {
                Some(i) => format!("\nAlso follow these instructions from the user:\n{i}\n"),
                None => String::new(),
            };
            blocks.push(PromptInputBlock::Text {
                text: format!(
                    "The user accepted this task — land it onto the base branch \
                     `{base_branch}` now, doing all git operations yourself:\n\
                     1. Commit any uncommitted changes in this worktree to the current \
                     branch (`{work_branch}`).\n\
                     2. Run `git merge {base_branch}` here and resolve every conflict so \
                     both the base's changes and this task's intent survive; complete the \
                     merge commit.\n\
                     3. Land onto the base checkout at `{root_path}`:\n   {land_command}\n\
                     {message_rule}\n\
                     {extra}\n\
                     Do NOT push, do NOT delete this worktree or its branch, and do not \
                     change anything else on the base branch — leave the checkout at \
                     `{root_path}` on `{base_branch}`, never switch branches there. \
                     Finish with one short line saying what landed."
                ),
            });
        }
    }

    // The standing worktree guard — a merge generation replaces it with its
    // own instructions (it exists to forbid exactly what a merge must do).
    //
    // It is the LAST built-in block, so its licence clause is the last thing
    // the agent reads: a read-only turn — and a report-deliverable original
    // order — has to swap that clause out, or "commit to the current branch as
    // you like" would quietly undo the intent's own "don't touch any file"
    // instruction several blocks earlier.
    if !matches!(mode, LaunchMode::Merge { .. }) {
        let branch = task
            .work_branch
            .as_deref()
            .map(|b| format!(" (branch `{b}`)"))
            .unwrap_or_default();
        let base = task
            .base_branch
            .as_deref()
            .map(|b| format!(" (`{b}`)"))
            .unwrap_or_default();
        let licence = if mode.is_read_only() || replayed_question {
            format!(
                "This turn is a question, not a work order: answer it in your reply and do NOT \
                 create, edit, delete or commit any file, and do not merge into, rebase onto, or \
                 push the base branch{base}. If answering would require a change, describe the \
                 change instead of making it."
            )
        } else if original_work_order && cfg.deliverable.as_deref() == Some(DELIVERABLE_REPORT) {
            // Report-deliverable order (forge plan-first / review-only):
            // the generic "commit as you like" grant would quietly undo the
            // task's own "analysis only" instruction several blocks earlier —
            // the licence must defer to the task text, not overrule it.
            format!(
                "This turn delivers a report, not code changes: put the full findings in your \
                 final reply — the user reads them from there, and may return this task for \
                 follow-up work afterwards. Commit only what the task's instructions \
                 explicitly allow (often nothing), and do NOT merge into, rebase onto, or \
                 push the base branch{base}."
            )
        } else {
            format!(
                "Commit to the current branch as you like, but do NOT merge into, rebase onto, \
                 or push the base branch{base} — the user lands the result after review. Finish \
                 with a short summary of what you did."
            )
        };
        blocks.push(PromptInputBlock::Text {
            text: format!(
                "—— Work task context ——\nYou are working inside a dedicated git worktree for \
                 this task{branch}. {licence}\nIf the `task_progress` and `task_complete` tools \
                 are available to you, report milestones with `task_progress` as you go, and \
                 call `task_complete` once right before you finish (verdict `success`, \
                 `needs_review`, or `blocked`, plus a short summary).",
            ),
        });
    }

    // Whatever the user added in task settings, always last: it refines the
    // built-in wording above it, and staying at the end keeps `prompt_head`
    // (the transcript's round marker) on the prompt's own opening text.
    blocks.extend(stage_prompt_block(settings, mode.round_kind()));
    Ok(blocks)
}

/// The user's own instructions for a stage: the `all` text (every stage) then
/// the stage's own, as one trailing block. Empty when neither is configured.
fn stage_prompt_block(
    settings: &WorkTaskFolderSettings,
    stage: &str,
) -> Option<PromptInputBlock> {
    let extras: Vec<&str> = [STAGE_PROMPT_ALL, stage]
        .into_iter()
        .filter_map(|key| settings.stage_prompts.get(key))
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .collect();
    if extras.is_empty() {
        return None;
    }
    Some(PromptInputBlock::Text {
        text: format!("—— Additional instructions ——\n{}", extras.join("\n\n")),
    })
}

/// The prompt text for a follow-up on a reviewed task. The intent decides the
/// framing, which is the whole point of having intents: the same sentence from
/// the user means "fix this", "also do this" or "explain this" depending on it,
/// and an agent told it was *returned* work will start editing either way.
fn follow_up_text(intent: FollowUpIntent, feedback: &str) -> String {
    match intent {
        // Historical wording, kept verbatim: this is what an unlabelled
        // follow-up composes, so the default path is unchanged.
        FollowUpIntent::Revise => format!(
            "The user reviewed your work on this task and returned it with the following \
             feedback. Address it in this same worktree:\n\n{feedback}"
        ),
        FollowUpIntent::Continue => format!(
            "The user reviewed your work on this task and accepted it as it stands. Keep going \
             in this same worktree with the following additional work — do not redo or second-\
             guess what is already there:\n\n{feedback}"
        ),
        FollowUpIntent::Question => format!(
            "The user has a question about your work on this task. Answer it directly in your \
             reply. This is a question, not a work order: do not create, edit, delete or commit \
             any file unless the user explicitly asks you to. If answering would require a \
             change, describe the change instead of making it.\n\n{feedback}"
        ),
        FollowUpIntent::Verify => {
            let base = "The user wants this task checked over before they accept it. Review your \
                        own work: read the full diff of this worktree against the base branch \
                        looking for bugs, leftovers, debug code and anything the task asked for \
                        but you did not do; run the project's own checks or tests; and fix what \
                        you find. Report what you checked and what you changed.";
            if feedback.is_empty() {
                base.to_string()
            } else {
                format!("{base}\n\nWhat they want you to pay attention to:\n\n{feedback}")
            }
        }
    }
}

/// The attachment blocks recorded on a `user_action` payload, if any. Absent on
/// every event written before follow-ups could carry attachments, so a missing
/// or malformed field simply means "nothing was attached".
fn payload_blocks(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    payload
        .get("blocks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Parse stored attachment blocks, dropping (with a warning) any that no longer
/// deserialize. Unlike the task's own prompt, an attachment is an addition to
/// the instruction — losing one must not stop the run that carries the rest.
fn attachment_blocks(raw: &[serde_json::Value], task_id: i32) -> Vec<PromptInputBlock> {
    raw.iter()
        .filter_map(|v| match serde_json::from_value::<PromptInputBlock>(v.clone()) {
            Ok(block) => Some(block),
            Err(e) => {
                tracing::warn!("[work_task] task {task_id}: dropping bad attachment block: {e}");
                None
            }
        })
        .collect()
}

/// How long a launch waits for the agent to advertise what a prompt may carry,
/// before sending its attached images in whatever shape they were stored. Only
/// image-bearing prompts ever wait, and only until the handshake lands — which
/// is the same round trip the prompt itself is about to need anyway.
const IMAGE_CAPABILITY_WAIT: Duration = Duration::from_secs(20);

/// Whether this block carries image bytes in either of the two wire encodings.
fn carries_image(block: &PromptInputBlock) -> bool {
    match block {
        PromptInputBlock::Image { .. } => true,
        PromptInputBlock::Resource {
            mime_type, blob, ..
        } => {
            blob.is_some()
                && mime_type
                    .as_deref()
                    .is_some_and(|m| m.starts_with("image/"))
        }
        _ => false,
    }
}

/// Swap every attached image into the shape the connected agent advertised.
///
/// Two wire encodings carry the same bytes: a native `Image` block, and a
/// `Resource` whose `blob` holds them under an image mime type (what an agent
/// that rejects image content but accepts embedded context takes). The composer
/// picks one at compose time from a probe; this picks again at dispatch, when
/// the session has actually said what it accepts.
///
/// Grok needs neither swap — it advertises both bits — but its images still get
/// sorted per mime further downstream, in `acp::connection`, which is the last
/// point that sees the blocks.
///
/// Only image-carrying blocks are touched, and only to move between those two
/// encodings — never to invent or drop content. An agent that advertises
/// neither is left alone: there is no third shape to reach for, and rewriting
/// into one it also rejects would only obscure the error it is about to raise.
fn reencode_images(blocks: &mut [PromptInputBlock], caps: &PromptCapabilitiesInfo) {
    for (index, block) in blocks.iter_mut().enumerate() {
        match block {
            PromptInputBlock::Image {
                data,
                mime_type,
                uri,
            } if !caps.image && caps.embedded_context => {
                *block = PromptInputBlock::Resource {
                    // A path-less image (a pasted screenshot) needs some stable
                    // identifier; its position in this prompt is one.
                    uri: uri
                        .clone()
                        .unwrap_or_else(|| format!("clipboard://work-task-image-{index}")),
                    mime_type: Some(mime_type.clone()),
                    text: None,
                    blob: Some(std::mem::take(data)),
                };
            }
            PromptInputBlock::Resource {
                uri,
                mime_type,
                text: None,
                blob: Some(blob),
            } if caps.image
                && !caps.embedded_context
                && mime_type
                    .as_deref()
                    .is_some_and(|m| m.starts_with("image/")) =>
            {
                *block = PromptInputBlock::Image {
                    data: std::mem::take(blob),
                    mime_type: mime_type.clone().unwrap_or_default(),
                    uri: Some(uri.clone()),
                };
            }
            _ => {}
        }
    }
}

/// A restart note reaches the agent as context, not as the task itself.
fn restart_note_block(note: &str) -> PromptInputBlock {
    PromptInputBlock::Text {
        text: format!(
            "The user restarted this task and left this note — take it into account:\n\n{note}"
        ),
    }
}

/// What the user last asked for, and how they meant it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OutstandingKind {
    /// A follow-up on a reviewed task.
    Review(FollowUpIntent),
    /// A note attached to a retry or a re-queue.
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Outstanding {
    kind: OutstandingKind,
    text: String,
    /// Raw prompt blocks the user attached to that instruction (images, pasted
    /// bytes). Kept unparsed here — `attachment_blocks` validates at the point
    /// the prompt is composed.
    attachments: Vec<serde_json::Value>,
}

/// The user instruction the agent still owes a turn to, if any.
///
/// Scans the task's events newest-first and stops at whichever comes first:
/// - a follow-up `user_action` (return / retry / requeue) → that one is
///   outstanding;
/// - a settle into `review` → a generation ran to completion after anything
///   earlier, so nothing is owed.
///
/// It is a barrier, not a filter. Filtering (e.g. "skip questions") would walk
/// *past* the newest instruction and resurrect an older one the agent already
/// carried out — replaying "add the tests" long after they were added. And the
/// review barrier is what stops a note from being re-injected into every
/// subsequent generation for the rest of the task's life.
/// What a retry launch learns from the event log: the newest instruction the
/// interrupted generation still owes the user (`outstanding`, replayed into
/// the prompt), and the kind of turn that generation was actually running
/// (`interrupted`, which picks the guard's licence). They differ exactly when
/// a retry/requeue note sits on top of a failed follow-up: the note is the
/// newer INSTRUCTION, but the turn underneath is still that follow-up — a
/// note refines the turn it interrupts, it does not change its kind.
struct InstructionScan {
    outstanding: Option<Outstanding>,
    /// The unsettled `return` intent beneath any retry/requeue notes; `None`
    /// when the generation was (re)doing the task's original order.
    interrupted: Option<FollowUpIntent>,
    /// That same `return` as an INSTRUCTION, and only when a newer
    /// retry/requeue note took the `outstanding` slot away from it.
    ///
    /// A resumed session already has those words in its transcript, so the
    /// note alone is the right replay there — that is what "a note refines the
    /// turn it interrupts" means. A session with no context (a re-queued
    /// task's fresh launch, a resume that failed over) has nothing: replaying
    /// only the note hands the agent "take this into account" with no account
    /// to take it into, and for a question it pairs a read-only licence with a
    /// question the agent was never shown.
    interrupted_instruction: Option<Outstanding>,
}

async fn instruction_scan(
    conn: &sea_orm::DatabaseConnection,
    task_id: i32,
) -> InstructionScan {
    // Newest-first, and narrowed IN THE QUERY to the two kinds that can settle
    // the question. Scanning raw events would let a chatty run bury the answer:
    // `agent_progress` volume is up to the agent, so a few hundred milestones
    // would push the instruction past any limit. Filtered this way the limit
    // bounds a log of decisions, which is small.
    let events = work_task_service::recent_events_of_kinds(
        conn,
        task_id,
        &["user_action", "status_changed"],
        200,
    )
    .await
    .unwrap_or_default();
    let mut scan = InstructionScan {
        outstanding: None,
        interrupted: None,
        interrupted_instruction: None,
    };
    for event in events {
        match event.kind.as_str() {
            "status_changed" => {
                let settled = event
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("to"))
                    .and_then(|v| v.as_str())
                    == Some("review");
                // Reaching review consumed every older instruction: whatever
                // lies beyond was delivered and decided on.
                if settled {
                    break;
                }
            }
            "user_action" => {
                let payload = match event.payload {
                    Some(p) => p,
                    None => continue,
                };
                let Some(action) = payload.get("action").and_then(|v| v.as_str()) else {
                    break;
                };
                match action {
                    "return" => {
                        let intent = FollowUpIntent::from_wire(
                            payload.get("intent").and_then(|v| v.as_str()),
                        )
                        .unwrap_or_default();
                        let returned =
                            payload.get("feedback").and_then(|v| v.as_str()).map(|text| {
                                Outstanding {
                                    kind: OutstandingKind::Review(intent),
                                    text: text.to_string(),
                                    attachments: payload_blocks(&payload),
                                }
                            });
                        if scan.outstanding.is_none() {
                            let Some(returned) = returned else {
                                break;
                            };
                            scan.outstanding = Some(returned);
                        } else {
                            // A retry/requeue note already claimed the newest
                            // slot. Keep the returned words too — see the field.
                            scan.interrupted_instruction = returned;
                        }
                        // The newest unsettled return IS the interrupted turn
                        // (a second return needs another review in between,
                        // which would have settled this scan already).
                        scan.interrupted = Some(intent);
                        break;
                    }
                    "retry" | "requeue" => {
                        if scan.outstanding.is_none() {
                            let Some(text) = payload.get("note").and_then(|v| v.as_str()) else {
                                break;
                            };
                            scan.outstanding = Some(Outstanding {
                                kind: OutstandingKind::Restart,
                                text: text.to_string(),
                                attachments: payload_blocks(&payload),
                            });
                        }
                        // Keep looking: the turn this note interrupted lies
                        // deeper in the log.
                    }
                    // Other user actions (delete, …) neither carry nor consume
                    // an instruction.
                    _ => continue,
                }
            }
            _ => continue,
        }
    }
    scan
}

/// Replay everything the launch still owes the user, oldest first, with each
/// instruction's own attachments right behind the sentence that framed it.
///
/// `resumed` is the whole difference. A resumed session carries the interrupted
/// turn in its transcript, so the newest instruction is the entire replay — a
/// retry/requeue note refines a turn the agent can still read. A session with
/// no context has to be given that turn as well, or the note lands on nothing:
/// see [`InstructionScan::interrupted_instruction`].
///
/// Shared by both replaying arms — a re-queued task reaches `Fresh` with
/// exactly the same debt to the user as an interrupted one reaching `Retry`,
/// so the two must not drift apart in what they replay or how they word it.
fn push_replay(
    blocks: &mut Vec<PromptInputBlock>,
    scan: &InstructionScan,
    resumed: bool,
    task_id: i32,
) {
    let mut push = |instruction: &Outstanding| {
        blocks.push(outstanding_block(instruction));
        blocks.extend(attachment_blocks(&instruction.attachments, task_id));
    };
    if !resumed {
        if let Some(interrupted) = &scan.interrupted_instruction {
            push(interrupted);
        }
    }
    if let Some(outstanding) = &scan.outstanding {
        push(outstanding);
    }
}

/// The block that replays one outstanding instruction, framed the way the user
/// meant it: an unanswered question must not come back as a work order.
fn outstanding_block(outstanding: &Outstanding) -> PromptInputBlock {
    match outstanding.kind {
        OutstandingKind::Restart => restart_note_block(&outstanding.text),
        OutstandingKind::Review(FollowUpIntent::Question) => PromptInputBlock::Text {
            text: format!(
                "The user asked this question before the interruption and never got an \
                 answer. Answer it, and do not change any files for it:\n\n{}",
                outstanding.text
            ),
        },
        OutstandingKind::Review(_) => PromptInputBlock::Text {
            text: format!("Latest review feedback to address:\n{}", outstanding.text),
        },
    }
}

/// One-shot sink for the generation a launch actually operated on. The launch
/// fills it as soon as it has read the row; `spawn_launch` reads it afterwards
/// so a setup failure is attributed to the right generation.
#[derive(Default)]
struct LaunchSeq(std::sync::Mutex<Option<i32>>);

impl LaunchSeq {
    // Poison-tolerant, matching the locks in `office_watch` and
    // `background_watch`. The guarded value is a plain `Option<i32>` that
    // cannot be left half-written, so a panic elsewhere in the launch has
    // nothing to corrupt here, while `expect` would turn that unrelated panic
    // into a permanent failure of every later launch. The schedule tick reaches
    // this with nobody at the keyboard.
    fn set(&self, run_seq: i32) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(run_seq);
    }

    fn get(&self) -> Option<i32> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// One-shot "this generation is live" report from a launch to whoever asked
/// for it — today the merge click, which must not sit on the rest of the run.
///
/// Fire-once by construction, and callable from either side of the launch: the
/// launch itself fires `Ok` the moment the agent is up and the row carries its
/// coordinates, while the task that drives it fires the launch's error for
/// every way of failing before that point. Whichever comes first wins; the
/// other is a no-op, so a failure AFTER a successful dispatch stays on the card
/// instead of being reported to a caller that has already been told the merge
/// started.
struct DispatchSignal(std::sync::Mutex<Option<oneshot::Sender<Result<(), String>>>>);

impl DispatchSignal {
    fn new() -> (Self, oneshot::Receiver<Result<(), String>>) {
        let (tx, rx) = oneshot::channel();
        (Self(std::sync::Mutex::new(Some(tx))), rx)
    }

    fn fire(&self, result: Result<(), String>) {
        // Poison-tolerant for the same reason as `LaunchSeq`: the fire-once
        // guarantee is the `take()`, not the lock's poison flag, and refusing
        // to fire after an unrelated panic would leave the caller waiting on a
        // oneshot that is never sent.
        let sender = self.0.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }
}

/// Ownership of a task's in-flight launch slot: which folder it counts
/// against, plus a token so only the launch that took the slot can release it.
struct LaunchOwner {
    folder_id: i32,
    token: u64,
}

/// A task's setup (init command) kill slot, open for the whole preparing phase.
///
/// The slot deliberately does NOT hold the child's pid. Only the task that
/// spawned the child ever kills it, so a pid can never be signalled after it
/// has been reaped (and possibly recycled by the OS onto an unrelated
/// process). A canceller just raises `kill_requested` and rings `wake`.
struct SetupChild {
    /// A cancel arrived: refuse to start an init command, and kill a live one.
    kill_requested: bool,
    /// Generation that owns the slot — a stale cancel must not stop a newer run.
    run_seq: i32,
    /// Rung by the canceller, awaited by whoever is running the child.
    wake: Arc<Notify>,
}

/// A launch's in-flight pre-prompt compaction, keyed by task in
/// [`TaskEngine::compacting`]. Holds the connection the compaction turn runs
/// on so a cancel can abort that turn without the task lock, and the generation
/// that owns it so a stale cancel cannot abort a newer run's compaction.
struct CompactRun {
    run_seq: i32,
    conn_id: String,
}

/// Everything [`TaskEngine::compact_before_prompt`] needs about the launch it
/// is running inside. A struct rather than a parameter list because four of
/// the fields are bare `i32`s in a row — named at the call site, a transposed
/// pair is a compile error instead of a task compacting someone else's session.
struct CompactRequest<'a> {
    task_id: i32,
    run_seq: i32,
    conn_id: &'a str,
    agent_type: AgentType,
    settings: &'a WorkTaskFolderSettings,
    /// The WORKTREE folder the conversation belongs to — same value the round's
    /// own prompt is linked with.
    folder_id: i32,
    conversation_id: i32,
    /// Status this generation holds for the rest of the launch, which the
    /// cancel gates re-check (`preparing`, or `merging` for a merge round).
    in_flight: WorkTaskStatus,
}

/// One context-window occupancy reading, plus where it came from — the source
/// rides the timeline event because "live" and "transcript" answer slightly
/// different questions (what the agent has loaded vs. what its last turn
/// recorded), and a surprising percentage is only debuggable with it.
struct ContextReading {
    percent: f64,
    used: Option<u64>,
    size: Option<u64>,
    source: &'static str,
}

/// How a compaction turn ended: `"ok"` / `"canceled"` / `"failed"`, plus the
/// stop reason or error worth showing beside it.
struct CompactOutcome {
    status: &'static str,
    detail: Option<String>,
}

impl CompactOutcome {
    fn new(status: &'static str, detail: Option<String>) -> Self {
        Self { status, detail }
    }
}

/// One decimal place, for percentages written to the timeline — the same
/// precision the composer's context ring shows, so the two never read as
/// disagreeing about the same session.
fn round1(percent: f64) -> f64 {
    (percent * 10.0).round() / 10.0
}

/// Marker file (in the worktree's PRIVATE git dir, so it can never show up in
/// `git status` or be committed) recording that the folder's init command
/// completed successfully in this worktree.
const SETUP_MARKER: &str = "codeg-task-init-ok";

async fn setup_marker_present(wt_path: &str) -> bool {
    match task_git::git_dir(wt_path).await {
        Ok(dir) => dir.join(SETUP_MARKER).exists(),
        // Can't resolve the git dir → assume not initialized. Re-running an
        // init command is wasteful at worst; skipping one is a broken tree.
        Err(_) => false,
    }
}

async fn write_setup_marker(wt_path: &str) {
    match task_git::git_dir(wt_path).await {
        Ok(dir) => {
            if let Err(e) = tokio::fs::write(dir.join(SETUP_MARKER), b"").await {
                tracing::warn!("[work_task] could not write the setup marker: {e}");
            }
        }
        Err(e) => tracing::warn!("[work_task] could not resolve the worktree git dir: {e}"),
    }
}

/// Best-effort kill of a child's whole process tree. Killing only the direct
/// child is not enough: the init command runs as `sh -c "<line>"`, and the real
/// work (`pnpm install` and its downloads) happens in descendants that would
/// otherwise keep running. Deliberately not `kill_on_drop`, which SIGKILLs the
/// shell first and reparents the descendants out of reach — same rationale as
/// `commands::office_tools::stream_install_or_kill_tree`.
async fn kill_process_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    if let Err(e) = kill_tree::tokio::kill_tree(pid).await {
        tracing::warn!("[work_task] kill_tree failed for setup pid {pid}: {e}");
    }
}

async fn still_expected(
    conn: &sea_orm::DatabaseConnection,
    task_id: i32,
    run_seq: i32,
    expected: WorkTaskStatus,
) -> bool {
    matches!(
        work_task_service::get_model(conn, task_id).await,
        Ok(t) if t.status == expected && t.run_seq == run_seq
    )
}

/// Head of the first text block of a prompt — the transcript viewer matches
/// user turns against it to place round dividers.
fn prompt_head(blocks: &[PromptInputBlock]) -> String {
    blocks
        .iter()
        .find_map(|b| match b {
            PromptInputBlock::Text { text } => Some(first_chars(text.trim(), 160)),
            _ => None,
        })
        .unwrap_or_default()
}

/// Run one shell command line in `cwd`, capturing combined output. stdin is
/// null (a command waiting on input sees EOF); no timeout by design — the
/// result write is generation-guarded, so a runaway command can only waste its
/// own process. Returns (exit code, trailing output capped to
/// `PREFLIGHT_TAIL_CHARS`).
async fn run_shell_capture(line: &str, cwd: &str) -> Result<(Option<i32>, String), String> {
    let out = shell_command(line, cwd)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(combine_capture(&out))
}

/// The platform's shell invocation for one command line, ready to `output()` or
/// `spawn()`. stdin is null so a command waiting on input sees EOF.
fn shell_command(line: &str, cwd: &str) -> tokio::process::Command {
    #[cfg(not(windows))]
    let mut command = {
        let mut c = crate::process::tokio_command("/bin/sh");
        c.arg("-c").arg(line);
        c
    };
    #[cfg(windows)]
    let mut command = {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let mut c = crate::process::tokio_command(comspec);
        c.arg("/C").arg(line);
        c
    };
    command.current_dir(cwd);
    command.stdin(std::process::Stdio::null());
    command
}

/// (exit code, trailing combined output) of a finished shell capture.
fn combine_capture(out: &std::process::Output) -> (Option<i32>, String) {
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (
        out.status.code(),
        tail_chars(&combined, PREFLIGHT_TAIL_CHARS),
    )
}

fn tail_chars(s: &str, n: usize) -> String {
    let trimmed = s.trim_end();
    let count = trimmed.chars().count();
    if count <= n {
        return trimmed.to_string();
    }
    trimmed.chars().skip(count - n).collect()
}

fn parse_agent_type(s: &str) -> Result<AgentType, String> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|_| format!("unknown agent type: {s}"))
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Cap on a task-derived conversation title. Long enough for a real card name,
/// short enough that the sidebar row is not one giant ellipsis.
const CONVERSATION_TITLE_MAX_CHARS: usize = 80;

/// The title the session a task produces carries: the task's own name, capped.
/// It is LOCKED on the conversation row at launch (`conversation_service::
/// lock_title`), so a board card and the session it spawned always read the
/// same in the sidebar (issue #495).
///
/// One definition, because two callers must agree byte-for-byte: the launch-time
/// seed here, and the rename propagation in `commands::work_task`, which
/// recognises "still the task's name" by comparing against this exact value.
pub(crate) fn conversation_title_for_task(task_title: &str) -> String {
    first_chars(task_title.trim(), CONVERSATION_TITLE_MAX_CHARS)
}

/// The project folder's own name — what a task worktree directory is named
/// after. `Path` semantics rather than a `/` split, so `C:\src\repo` yields
/// `repo` too.
///
/// The answer is always RELATIVE, empty included: a path with no final
/// component (a filesystem root, or one ending in `..`) must not fall back to
/// the path itself, because the name is JOINED onto the configured worktree
/// root — and an absolute one replaces that root instead of landing under it.
fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
}

/// `name` placed next to the project folder — where task worktrees live when
/// the folder configures no root of its own. A project folder with no parent
/// (a drive or filesystem root) falls back to the bare name, which git then
/// resolves against the repository itself.
fn sibling_path(root_path: &str, name: &str) -> String {
    match Path::new(root_path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join(name).to_string_lossy().into_owned()
        }
        _ => name.to_string(),
    }
}

/// Where the task worktree directory `name` goes for a project at
/// `root_path`. `worktree_root` is the folder setting: blank (or absent) keeps
/// the historical layout — right next to the project folder — and any other
/// value becomes the directory every new worktree of that folder is created
/// in. A typed path is taken at face value in the two ways a user expects: `~`
/// is the home directory, and something relative hangs off the project folder.
fn worktree_path_in(root_path: &str, worktree_root: Option<&str>, name: &str) -> String {
    worktree_path_in_home(root_path, worktree_root, name, dirs::home_dir().as_deref())
}

/// [`worktree_path_in`] with the home directory injected — `dirs::home_dir()`
/// reads the real user's home even under a pinned `HOME`, so tests must hand
/// one in rather than set the environment.
fn worktree_path_in_home(
    root_path: &str,
    worktree_root: Option<&str>,
    name: &str,
    home: Option<&Path>,
) -> String {
    let Some(configured) = worktree_root.map(str::trim).filter(|r| !r.is_empty()) else {
        return sibling_path(root_path, name);
    };
    let base = expand_home(configured, home);
    let base = if base.is_absolute() {
        base
    } else {
        Path::new(root_path).join(base)
    };
    base.join(name).to_string_lossy().into_owned()
}

/// Expand a leading `~` against `home`. Left as-is when the home directory is
/// unknown, so the path still resolves as literally written instead of
/// silently becoming a different directory.
fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    if path == "~" {
        return home.to_path_buf();
    }
    match path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\"))
    {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

/// codeg-mcp `task_progress` / `task_complete` access handed to the delegation
/// listener at boot. Resolves the process-global engine at CALL time — the
/// listener is constructed before the engine, and a process that never wins the
/// engine lock cleanly rejects every report.
pub struct EngineWorkTaskTools;

#[async_trait::async_trait]
impl WorkTaskToolAccess for EngineWorkTaskTools {
    async fn report_progress(&self, parent_connection_id: &str, message: &str) -> TaskReportAck {
        let Some(engine) = engine() else {
            return TaskReportAck::rejected("no task engine running in this process");
        };
        engine.record_progress(parent_connection_id, message).await
    }

    async fn complete(
        &self,
        parent_connection_id: &str,
        verdict: &str,
        summary: Option<&str>,
    ) -> TaskReportAck {
        let Some(engine) = engine() else {
            return TaskReportAck::rejected("no task engine running in this process");
        };
        engine
            .record_complete(parent_connection_id, verdict, summary)
            .await
    }
}

pub(crate) mod launch;
pub(crate) mod compaction;
pub(crate) mod events;
pub(crate) mod progress;
pub(crate) mod merge;
pub(crate) mod deliver;
pub(crate) mod pump;
pub(crate) mod cleanup;

#[cfg(test)]
mod tests;

