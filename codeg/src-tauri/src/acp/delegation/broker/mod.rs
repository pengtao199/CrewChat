//! `DelegationBroker` — the coordination unit for multi-agent delegation.
//!
//! Delegation is **asynchronous**: `delegate_to_agent` returns a `task_id`
//! ack as soon as setup finishes; the LLM collects the result later with
//! `get_delegation_status` (optionally long-polling) or stops it with
//! `cancel_delegation`. There is no blocking `oneshot` — a running task is just
//! an entry in the `running` map, and a terminal event migrates it into the
//! `completed` cache (atomically, under one lock) and wakes any long-poll via
//! `result_notify`.
//!
//! Lifecycle of a single task:
//!
//! 1. [`DelegationBroker::start_delegation`] is the broker's entry point. The
//!    MCP listener feeds it the LLM-issued `delegate_to_agent` payload.
//! 2. Pre-checks: feature enabled? depth limit ok? Both failures return a
//!    terminal report immediately, no child session created.
//! 3. Spawn the child via [`ConnectionSpawner::spawn`].
//! 4. Send the delegation task as the first prompt via
//!    [`ConnectionSpawner::send_prompt_linked_for_delegation`]. The trailing
//!    [`DelegationLink`] carries the parent's `tool_use_id` and a
//!    broker-internal `call_id` (UUID = `task_id`) — persisted onto the new
//!    conversation row so the lifecycle resolver can find it.
//! 5. Register a [`RunningTask`] keyed by `call_id` and return a `Running` ack
//!    [`DelegationTaskReport`] (or a terminal report when the child finished
//!    during setup / a cancel reached it mid-setup / setup itself failed).
//! 6. Later, a terminal event resolves the task — migrating it `running` →
//!    `completed` and tearing the child down:
//!       - the lifecycle calling [`DelegationBroker::complete_call`] on
//!         `TurnComplete` (happy path), or
//!       - a cancel — MCP-side (`notifications/cancelled` →
//!         [`DelegationBroker::cancel_by_external_handle`]), child-side
//!         ([`DelegationBroker::cancel_by_child_connection`]), parent-side
//!         ([`DelegationBroker::cancel_by_parent`] /
//!         [`DelegationBroker::cancel_by_parent_turn`]), or the LLM's own
//!         [`DelegationBroker::cancel_task_by_id`].
//!
//! v1 is explicitly one-shot — no session reuse. The one exception is
//! [`DelegationBroker::resume_delegation`]: an INTERRUPTED task (canceled, or
//! stranded `in_progress` by a crash) can be brought back under its original
//! `task_id` by reloading the child's recorded agent session — a continuation
//! of the original task, never a new iteration on it.
//!
//! Result durability: child output is NOT stored in codeg's DB, so the broker
//! caches the completed text in `completed` (parent-scoped, FIFO-capped). Once
//! evicted, [`DelegationBroker::get_task_status`] falls back to the DB for the
//! task's terminal STATUS (via [`ChildStatusLookup`]); the full output is always
//! viewable in the child's own session.
//!
//! Cancellation cascade: when a parent session goes away (user-initiated
//! cancel, parent disconnect), the lifecycle subscriber calls
//! [`DelegationBroker::cancel_by_parent`] which fans out cancel + disconnect
//! to every running child of that parent. A normal `end_turn` does NOT cancel
//! children — they keep running in the background (the whole point of async).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify};

use crate::acp::delegation::event_emitter::{DelegationEventEmitter, NoopEventEmitter};
use crate::acp::delegation::live_reply::{ChildLiveReplyLookup, NoopChildLiveReplyLookup};
use crate::acp::delegation::meta_writer::{
    build_delegation_meta, is_synthetic_parent_tool_use_id, DelegationMetaWriter, NoopMetaWriter,
};
use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink};
use crate::acp::delegation::types::{
    AgentDelegationDefaults, BlockedKind, BlockedOn, DelegationError, DelegationOutcome,
    DelegationRequest, DelegationTaskReport, ResumeDelegationRequest, TaskStatus,
};
use crate::acp::types::DelegationResultSummary;
use crate::models::AgentType;

/// Default per-parent byte budget for cached completed-task result text. The
/// completed-cache lets `get_delegation_status` / `cancel_delegation` return a
/// finished task's result after the lifecycle resolved it; once a parent's
/// retained result text exceeds this budget the OLDEST results are FIFO-evicted
/// (evicted tasks fall back to the DB status lookup, which carries status only).
/// This is the seed value baked into `DelegationConfig::default()`; the live
/// value is user-configurable from the settings page (in MB) and `0` disables
/// eviction entirely. See `PendingInner::completed_cap_bytes`.
const DEFAULT_COMPLETED_CACHE_CAP_BYTES: usize = 512 * 1024 * 1024;

/// Per-result cap on cached completed text. The full child output always lives
/// in the child's own session (viewable via the frontend's child-session
/// sheet); this only bounds the broker's in-memory copy of a SINGLE result.
/// Because it is far below the per-parent byte budget
/// (`DEFAULT_COMPLETED_CACHE_CAP_BYTES`), the newest result always fits and is
/// never the eviction victim in `insert_completed`.
const COMPLETED_TEXT_CAP: usize = 256 * 1024;

/// Cap on the `task_preview` carried by the `DelegationStarted` event and the
/// parent-card meta writes. The full task text lives in the MCP call (and, on
/// most hosts, in the parent tool call's own `raw_input`); the preview only
/// has to label the delegation card, so it shares the status-preview budget
/// rather than the multi-KiB result cap.
const TASK_PREVIEW_CAP: usize = 2 * 1024;

/// Cap on the inline `text_preview` carried by the `DelegationCompleted` event
/// and the terminal meta, so the parent card can render the result inline
/// without re-fetching the child session.
const STATUS_PREVIEW_CAP: usize = 2 * 1024;

/// Lookup the `parent_id` for a conversation. Abstracted so the broker can be
/// unit-tested against an in-memory chain without touching SeaORM.
#[async_trait]
pub trait ConversationDepthLookup: Send + Sync {
    async fn parent_of(&self, conversation_id: i32) -> Result<Option<i32>, DelegationError>;
}

/// Status-level facts the broker recovers from a child conversation row when a
/// task's in-memory completed-cache entry was evicted. Carries NO result text —
/// child output isn't stored in codeg's DB; the full result lives in the
/// child's own session (viewable via the frontend's child-session sheet).
#[derive(Debug, Clone)]
pub struct ChildStatusRecord {
    pub child_conversation_id: i32,
    pub status: TaskStatus,
    pub agent_type: AgentType,
    /// The parent conversation id this child was spawned under. Used to scope
    /// the DB fallback to the calling parent so one parent can't read another's
    /// task by guessing a UUID.
    pub parent_id: Option<i32>,
}

/// Everything `resume_delegation` needs to know about an interrupted child
/// before it can bring the session back: where it ran, which agent, which
/// agent-side session to load, and how it is currently recorded.
///
/// Superset of [`ChildStatusRecord`] — kept separate so the hot status path
/// keeps its lean shape while resume (rare) pays for the extra fields.
#[derive(Debug, Clone)]
pub struct ChildResumeContext {
    pub child_conversation_id: i32,
    pub status: TaskStatus,
    pub agent_type: AgentType,
    /// Parent conversation scope — same ownership rule as
    /// [`ChildStatusRecord::parent_id`].
    pub parent_id: Option<i32>,
    /// The ORIGINAL delegation's parent-side tool_use_id persisted on the row.
    /// Reused for the resumed run's meta writes / started event so the parent's
    /// original delegation card re-binds to the resumed child. `None` maps to a
    /// synthetic id (meta/events safely skipped).
    pub parent_tool_use_id: Option<String>,
    /// The agent-assigned session id to resume (`conversation.external_id`).
    /// `None` means the child died before its session id was ever recorded —
    /// nothing to load, so the task is not resumable.
    pub external_id: Option<String>,
    /// The child row's folder — the resumed prompt is sent back into the same
    /// row, which requires its folder id (send-adopt contract).
    pub folder_id: i32,
    /// Absolute path of `folder_id`, used as the resumed connection's
    /// working_dir. `None` when the folder row is gone (refuse, don't guess).
    pub working_dir: Option<String>,
    /// The child row's title (originally seeded from the delegated task text) —
    /// bounded and reused as the resumed task's `task_preview` label.
    pub title: Option<String>,
}

/// DB fallback for `get_delegation_status` / `cancel_delegation` once a task's
/// result has aged out of the broker's in-memory completed-cache. Abstracted
/// so broker unit tests can run without SeaORM; production wires
/// [`DbChildStatusLookup`] via [`DelegationBroker::with_status_lookup`].
#[async_trait]
pub trait ChildStatusLookup: Send + Sync {
    async fn find_by_call_id(&self, call_id: &str) -> Option<ChildStatusRecord>;

    /// Resolve the full resume context for `resume_delegation`. Default `None`
    /// ("cannot resume") so status-only lookups and existing test doubles stay
    /// unchanged; production and resume-focused tests override it.
    async fn find_resume_context_by_call_id(&self, _call_id: &str) -> Option<ChildResumeContext> {
        None
    }
}

/// Default lookup — always "unknown". Used by `DelegationBroker::new` /
/// `with_writers` (tests that don't exercise the DB-fallback path); production
/// replaces it via `with_status_lookup`.
#[derive(Default, Clone)]
pub struct NoopChildStatusLookup;

#[async_trait]
impl ChildStatusLookup for NoopChildStatusLookup {
    async fn find_by_call_id(&self, _call_id: &str) -> Option<ChildStatusRecord> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct DelegationConfig {
    pub enabled: bool,
    /// Max chain depth a *new* delegation may exist at. With `depth_limit = 2`
    /// the chain root → child → grandchild is allowed; the grandchild trying
    /// to spawn a great-grandchild is rejected. See spec §5.
    pub depth_limit: u32,
    /// Per-agent overrides applied when spawning a delegation child. Keyed by
    /// the target `agent_type`; missing entries mean "no override." Forwarded
    /// to `ConnectionSpawner::spawn` as `preferred_mode_id` /
    /// `preferred_config_values`.
    pub agent_defaults: BTreeMap<AgentType, AgentDelegationDefaults>,
    /// Per-parent byte budget for cached completed-task result text. `0`
    /// disables eviction (unlimited). Surfaced from the settings page in MB and
    /// converted to bytes in `into_broker_config`. Pushed into the pending-calls
    /// bucket by `set_config` so `insert_completed` reads it lock-free.
    pub completed_cache_cap_bytes: usize,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            depth_limit: 1,
            agent_defaults: BTreeMap::new(),
            completed_cache_cap_bytes: DEFAULT_COMPLETED_CACHE_CAP_BYTES,
        }
    }
}

/// A delegation task running in the background after `start_delegation`
/// returned its `Running` ack. The async redesign drops the parked
/// `oneshot::Sender` the old `PendingCall` carried: the parent's
/// `delegate_to_agent` no longer blocks on a channel, so there is nothing to
/// signal. A terminal event instead migrates the entry into `completed` (same
/// lock) and wakes any `get_delegation_status` long-poll via the broker's
/// `result_notify`.
struct RunningTask {
    child_connection_id: String,
    child_conversation_id: i32,
    parent_connection_id: String,
    parent_tool_use_id: String,
    /// Target agent — surfaced in status reports.
    agent_type: AgentType,
    /// Bounded preview of the delegated task text ([`TASK_PREVIEW_CAP`]).
    /// Carried so TERMINAL meta writes can keep labeling the parent card —
    /// meta is replace-wholesale on the ToolCallState, so a terminal write
    /// that dropped the task text would erase what the running write supplied.
    task_preview: String,
    /// The broker-minted task id — duplicates this entry's key in `running`
    /// because the cancel/teardown paths hand around the drained
    /// `RunningTask` by value without its map key.
    task_id: String,
    /// MCP-side opaque handle minted by the companion per `tools/call`. The
    /// listener forwards it through `DelegationRequest`; we keep it here so
    /// `cancel_by_external_handle` can find the entry. `None` for delegations
    /// that didn't come through MCP (tests, future internal callers).
    external_handle: Option<String>,
    /// When the child started running (after `send_prompt` succeeded). Used to
    /// compute a real `duration_ms` at terminal time.
    started_at: Instant,
    /// The last blocking prompt a status query reported to the parent, and when.
    /// Makes "the block I told you about" distinguishable from "a NEW block",
    /// which is what stops a `wait_ms: 0` poll from returning instantly forever
    /// once a child parks on a permission (#447): the first observation
    /// returns, a re-poll on the same prompt goes back to waiting, and a second
    /// prompt returns again.
    ///
    /// The timestamp is the safety valve. Marking a block reported is not proof
    /// the parent RECEIVED it — the report still has to cross the listener, the
    /// companion's stdout and the agent CLI, and an MCP `notifications/cancelled`
    /// or a client-side call abort discards it silently. Without the valve that
    /// lost report would be the only one ever offered, re-creating the exact
    /// stall this fixes. So the same unanswered prompt becomes reportable again
    /// after [`BLOCK_RESURFACE_INTERVAL`].
    last_surfaced_block: Option<SurfacedBlock>,
}

/// See [`RunningTask::last_surfaced_block`].
struct SurfacedBlock {
    request_id: String,
    at: Instant,
}

/// A terminal delegation result retained so `get_delegation_status` /
/// `cancel_delegation` can answer after the lifecycle resolved the task.
/// Parent-scoped, FIFO-evicted once the parent's retained result text exceeds
/// `PendingInner::completed_cap_bytes`, and dropped wholesale when the parent
/// connection tears down.
#[derive(Clone)]
struct CompletedTask {
    parent_connection_id: String,
    child_conversation_id: i32,
    agent_type: AgentType,
    status: TaskStatus,
    /// Result text for `Completed` (capped at [`COMPLETED_TEXT_CAP`]). `None`
    /// for failures/cancels.
    text: Option<String>,
    error_code: Option<String>,
    message: Option<String>,
    duration_ms: u64,
}

#[derive(Default)]
struct PendingCalls {
    inner: Mutex<PendingInner>,
}

/// Everything guarded by the single pending-calls mutex. Co-locating the parked
/// calls with the early-terminal bookkeeping under ONE lock is what makes the
/// terminal-vs-registration race safe: a terminal event for a delegation that
/// is still mid-setup (its `handle_request` hasn't parked the [`PendingCall`]
/// yet) and the matching registration are serialized on this lock, so the
/// terminal event either finds the parked entry (resolves via `tx`) or buffers
/// its outcome (and `handle_request` drains it the instant it parks) — never
/// both, never neither. Without this, a terminal that fires in the spawn→park
/// window would no-op the resolver and then strand the parked `rx.await`.
///
/// Both CHILD-terminal pre-park resolvers are covered, because either can win
/// the race against the parent `write_meta` await between `send_prompt` and the
/// park:
///   * `complete_call` — a fast/empty turn's `TurnComplete` (the prompt is only
///     *enqueued* by `send_prompt`; the child loop emits `TurnComplete`
///     independently). Keyed by `call_id`.
///   * `cancel_by_child_connection` — a freshly-spawned child connection dying
///     before its first prompt is answered. Keyed by `child_connection_id`.
///
/// Parent-side cancels (`cancel_by_parent` / `cancel_by_parent_turn`) are
/// covered symmetrically by the `inflight` registry: `handle_request` registers
/// each setup at entry, and `mark_inflight_canceled_for_parent` runs in the SAME
/// lock acquisition that drains the parked `calls`. A parent cancel landing
/// while a child is still mid-setup therefore flags the in-flight record, and
/// `handle_request` observes the flag at its next checkpoint (or atomically at
/// park) and tears the child down itself — it is no longer left to the child's
/// own terminal / connection-teardown cascade.
///
/// The reservation records the `child_connection_id` each resolver gates on;
/// `handle_request` drains both buffers at park.
#[derive(Default)]
struct PendingInner {
    /// Tasks running in the background after their `Running` ack, keyed by
    /// broker `call_id` (= `task_id`). A terminal event migrates an entry from
    /// here into `completed` under THIS lock (atomic `running` → `completed`
    /// transition), so a concurrent `get_delegation_status` never observes a
    /// task as neither running nor completed.
    running: HashMap<String, RunningTask>,
    /// Terminal results retained for `get_delegation_status` / `cancel_delegation`,
    /// keyed by `task_id`. Bounded by the per-parent byte valve
    /// (`completed_cap_bytes` over `completed_bytes`, FIFO-evicted via
    /// `completed_order`) and dropped per-parent on connection teardown.
    /// Evicted/unknown tasks fall back to the DB status lookup.
    completed: HashMap<String, CompletedTask>,
    /// Per-parent FIFO index over `completed` for byte-valve eviction and
    /// per-parent teardown. Keyed by `parent_connection_id`; each deque holds
    /// that parent's completed `task_id`s oldest-first.
    completed_order: HashMap<String, VecDeque<String>>,
    /// Per-parent running total of retained completed result-text bytes (the
    /// `CompletedTask::text` lengths). Drives the `completed_cap_bytes` valve in
    /// `insert_completed`; kept in sync on insert/evict and cleared per-parent
    /// on teardown.
    completed_bytes: HashMap<String, usize>,
    /// Per-parent byte budget for retained completed result text. `0` =
    /// unlimited (no eviction). Seeded by `set_config` from the live
    /// `DelegationConfig` (default until then: `0`, but `set_config` always runs
    /// at startup via `apply_persisted_config`). Read lock-free by
    /// `insert_completed`, which already holds THIS mutex — so the cap is
    /// consulted WITHOUT nesting the `config` lock under the pending lock.
    completed_cap_bytes: usize,
    /// In-setup delegations (spawned + id minted, not yet parked), mapping
    /// `call_id` → `child_connection_id`. Gating the early buffers on membership
    /// here distinguishes a genuine pre-registration race (still reserved →
    /// buffer) from the normal post-resolution teardown that fires on every
    /// completion (no longer reserved → ignore). Removed at park / on the
    /// send-failure path.
    setups: HashMap<String, String>,
    /// Completion outcomes captured by a `TurnComplete` that beat registration
    /// (gated by `setups`), keyed by `call_id`. Each carries the `seq` arrival
    /// stamp taken when it buffered, so the park can order it against a racing
    /// parent cancel (first-terminal-wins). Drained at park.
    early_completes: HashMap<String, (u64, DelegationOutcome)>,
    /// Cancel reasons captured by a child failure that beat registration (gated
    /// by `setups`), keyed by `child_connection_id`. The value pairs the `seq`
    /// arrival stamp (for the park's first-terminal-wins ordering against a
    /// racing parent cancel) with the pre-computed `Canceled { reason }` text
    /// (same wording the parked `cancel_by_child_connection` path produces);
    /// `handle_request` rebuilds the full outcome at park with the real
    /// `child_conversation_id` (which the resolver, finding no entry, lacked).
    early_cancels: HashMap<String, (u64, String)>,
    /// In-flight `handle_request` setups, keyed by a unique per-call id and
    /// registered at entry (BEFORE the claim poll, so the whole claim→park
    /// window is covered). This is the parent-cancel counterpart to `setups`:
    /// `setups` lets a *child* terminal reach a not-yet-parked delegation,
    /// while `inflight` lets a *parent* cancel reach one. `cancel_by_parent*`
    /// flags every entry it owns (`mark_inflight_canceled_for_parent`);
    /// `handle_request` consults the flag after claim, after spawn, and
    /// atomically at park, tearing the spawned child down itself when set.
    /// Removed at park and on every early-return (no Drop guard — see
    /// `register_inflight`).
    inflight: HashMap<u64, InflightSetup>,
    /// Monotonic arrival clock (see `tick`). Hands out the unique `inflight`
    /// keys AND the arrival stamps on buffered child terminals / parent cancels,
    /// so the park can resolve a setup-window race by true first-terminal-wins
    /// order. Keys and stamps share this sequence but are never cross-compared
    /// (keys match by identity, stamps only by `<` against other stamps).
    seq: u64,
    /// Task ids with a `resume_delegation` currently mid-setup (claimed in the
    /// same lock pass as its memory gate, released when the resume settles).
    /// Two concurrent resumes of the SAME id would otherwise both pass the
    /// gate — the id is in neither `running` nor (post-crash) `completed` —
    /// spawn two children onto one agent session, and the second registration
    /// would silently stomp the first's `RunningTask`. Unlike `setups` this is
    /// a plain claim with no early-terminal gating semantics.
    resuming: HashSet<String>,
}

/// One in-flight `handle_request` setup tracked for parent-cancel coverage.
struct InflightSetup {
    parent_connection_id: String,
    /// `Some(stamp)` once a parent cancel lands while this delegation is
    /// mid-setup (spawned / sending, not yet parked), where `stamp` is the `seq`
    /// arrival-clock value at that moment. First-write-wins and never cleared,
    /// so a cancel can't be lost between `handle_request`'s checkpoints, and its
    /// stamp lets the park order it against a racing child terminal.
    canceled_at: Option<u64>,
}

impl PendingInner {
    /// Mark a delegation as setting-up (spawned + id minted, not yet parked) so
    /// a terminal event racing the park is buffered rather than dropped.
    ///
    /// No cap: a reservation lives only for the brief spawn→park window and is
    /// always released by `unreserve` on every `handle_request` exit (park, or
    /// the send-failure path), so `setups` is bounded by the count of
    /// concurrently-in-setup delegations — it never accumulates stale entries.
    /// A cap here would be actively unsafe: every reservation is live, so
    /// evicting one to make room would drop a real in-flight delegation's race
    /// guard and reopen the very hang this machinery exists to prevent.
    fn reserve(&mut self, call_id: &str, child_connection_id: &str) {
        self.setups
            .insert(call_id.to_string(), child_connection_id.to_string());
    }

    /// Release a delegation's reservation and discard any un-drained buffered
    /// terminal — called once the entry is parked (the buffers were already
    /// drained, so the removals are no-ops then) or when setup errors out
    /// (discarding a buffer no `handle_request` will pick up).
    fn unreserve(&mut self, call_id: &str, child_connection_id: &str) {
        self.setups.remove(call_id);
        self.early_completes.remove(call_id);
        self.early_cancels.remove(child_connection_id);
    }

    /// Whether a child connection belongs to a still-in-setup delegation. O(n)
    /// over `setups`, but n is the (tiny) count of concurrently-in-setup
    /// delegations.
    fn is_child_reserved(&self, child_connection_id: &str) -> bool {
        self.setups
            .values()
            .any(|child| child == child_connection_id)
    }

    /// Buffer a completion for a still-reserved delegation, stamped with the
    /// current arrival clock so the park can order it against a racing parent
    /// cancel. No-op when the `call_id` isn't reserved (already resolved by
    /// another terminal path), so the buffer only ever holds genuine
    /// pre-registration races.
    fn buffer_early_complete(&mut self, call_id: &str, outcome: DelegationOutcome) {
        if self.setups.contains_key(call_id) {
            let stamp = self.tick();
            self.early_completes
                .insert(call_id.to_string(), (stamp, outcome));
        }
    }

    /// Buffer a child failure for a still-reserved delegation, stamped with the
    /// current arrival clock so the park can order it against a racing parent
    /// cancel. No-op when the child isn't reserved (normal post-resolution
    /// teardown). Stores the pre-computed cancel reason so the park rebuilds the
    /// same wording the parked `cancel_by_child_connection` path produces.
    fn buffer_child_failure(&mut self, child_connection_id: &str, detail: Option<String>) {
        if self.is_child_reserved(child_connection_id) {
            let stamp = self.tick();
            self.early_cancels.insert(
                child_connection_id.to_string(),
                (stamp, child_canceled_reason(detail.as_deref())),
            );
        }
    }

    /// Drain a buffered completion with its arrival stamp (by `call_id`) — used
    /// by `handle_request` at park.
    fn take_early_complete(&mut self, call_id: &str) -> Option<(u64, DelegationOutcome)> {
        self.early_completes.remove(call_id)
    }

    /// Drain a buffered cancel reason with its arrival stamp (by
    /// `child_connection_id`) — used by `handle_request` at park.
    fn take_early_cancel(&mut self, child_connection_id: &str) -> Option<(u64, String)> {
        self.early_cancels.remove(child_connection_id)
    }

    /// Advance the monotonic arrival clock, returning the pre-increment value.
    /// Strictly increasing (wraps only after 2^64 calls — unreachable), so two
    /// events stamped under this lock always compare in their true arrival
    /// order. Backs both `inflight` keys and terminal/cancel arrival stamps; the
    /// two uses never cross-compare (keys match by identity, stamps by `<`).
    fn tick(&mut self) -> u64 {
        let v = self.seq;
        self.seq = self.seq.wrapping_add(1);
        v
    }

    /// Register an in-flight setup at `handle_request` entry, returning its
    /// unique id. The caller MUST `deregister_inflight` on every exit path
    /// (each early-return, and at park). There is deliberately NO Drop guard:
    /// the park hand-off — `calls.insert` followed by `deregister_inflight` —
    /// has to be atomic under this lock so a concurrent parent cancel sees the
    /// entry in exactly one of `inflight` or `calls`, and a guard firing after
    /// the lock releases would reopen that window.
    fn register_inflight(&mut self, parent_connection_id: &str) -> u64 {
        let id = self.tick();
        self.inflight.insert(
            id,
            InflightSetup {
                parent_connection_id: parent_connection_id.to_string(),
                canceled_at: None,
            },
        );
        id
    }

    /// Drop an in-flight setup record (idempotent).
    fn deregister_inflight(&mut self, id: u64) {
        self.inflight.remove(&id);
    }

    /// Whether a parent cancel flagged this in-flight setup. False once the
    /// record is gone (already parked / deregistered). Used by the pre-spawn /
    /// post-spawn checkpoints, which only need the boolean.
    fn inflight_canceled(&self, id: u64) -> bool {
        self.inflight
            .get(&id)
            .map(|s| s.canceled_at.is_some())
            .unwrap_or(false)
    }

    /// Arrival stamp of the parent cancel that flagged this in-flight setup, if
    /// any (`None` when not canceled, or the record is already gone). Used at
    /// park to order the cancel against a buffered child terminal.
    fn inflight_canceled_at(&self, id: u64) -> Option<u64> {
        self.inflight.get(&id).and_then(|s| s.canceled_at)
    }

    /// Flag every in-flight setup owned by `parent_connection_id` as canceled,
    /// stamping each with one shared arrival-clock value (this cancel is a
    /// single event). First-write-wins per setup, so a later cancel can't push
    /// an earlier one's stamp forward. Called from `drain_for_parent_cancel` in
    /// the SAME lock acquisition that drains the parked `calls`, so each of the
    /// parent's delegations is caught either here (still in-flight → flagged;
    /// `handle_request` tears its child down at the next checkpoint) or by the
    /// parked-call drain (already parked) — never neither.
    fn mark_inflight_canceled_for_parent(&mut self, parent_connection_id: &str) {
        let stamp = self.tick();
        for setup in self.inflight.values_mut() {
            if setup.parent_connection_id == parent_connection_id && setup.canceled_at.is_none() {
                setup.canceled_at = Some(stamp);
            }
        }
    }

    /// Insert a terminal result into the completed-cache, then FIFO-evict this
    /// parent's OLDEST results until its retained result-text bytes fit
    /// `completed_cap_bytes` (`0` = unlimited). Evicted tasks fall back to the
    /// DB status lookup (status only — child text lives in the child session).
    /// The just-inserted entry is never the victim: a single result is capped
    /// at [`COMPLETED_TEXT_CAP`] (256 KiB), far below any MB-scale budget, so
    /// the newest result always survives for the LLM's immediate
    /// `get_delegation_status`. The caller does the atomic `running.remove` +
    /// this insert under one lock, then notifies long-poll waiters AFTER
    /// releasing the lock.
    fn insert_completed(&mut self, call_id: &str, task: CompletedTask) {
        let parent = task.parent_connection_id.clone();
        let task_bytes = task.text.as_ref().map_or(0, |t| t.len());
        self.completed.insert(call_id.to_string(), task);
        *self.completed_bytes.entry(parent.clone()).or_insert(0) += task_bytes;
        self.completed_order
            .entry(parent.clone())
            .or_default()
            .push_back(call_id.to_string());
        self.evict_completed_over_cap(&parent);
    }

    /// Evict `parent`'s OLDEST completed results until its retained result-text
    /// bytes fit `completed_cap_bytes` (`0` = unlimited). Evicted tasks fall
    /// back to the DB status lookup (status only — child text lives in the child
    /// session). The newest entry is never evicted: a single result is capped at
    /// [`COMPLETED_TEXT_CAP`] (256 KiB), far below any MB-scale budget, so the
    /// LLM's immediate `get_delegation_status` always hits.
    fn evict_completed_over_cap(&mut self, parent: &str) {
        let cap = self.completed_cap_bytes;
        if cap == 0 {
            return;
        }
        loop {
            if self.completed_bytes.get(parent).copied().unwrap_or(0) <= cap {
                break;
            }
            let evicted = match self.completed_order.get_mut(parent) {
                Some(order) if order.len() > 1 => order.pop_front(),
                _ => None,
            };
            let Some(evicted) = evicted else {
                break;
            };
            if let Some(removed) = self.completed.remove(&evicted) {
                let freed = removed.text.as_ref().map_or(0, |t| t.len());
                if let Some(slot) = self.completed_bytes.get_mut(parent) {
                    *slot = slot.saturating_sub(freed);
                }
            }
        }
    }

    /// Remove one completed entry WITH its byte / FIFO-order bookkeeping.
    /// Used by `resume_delegation` when it re-registers a canceled task as
    /// running again: a plain `completed.remove` would leave the id in
    /// `completed_order` (a later re-terminal would then double-push it) and
    /// leave its bytes counted against the parent's cap forever.
    fn remove_completed(&mut self, call_id: &str) -> Option<CompletedTask> {
        let removed = self.completed.remove(call_id)?;
        let freed = removed.text.as_ref().map_or(0, |t| t.len());
        if let Some(slot) = self.completed_bytes.get_mut(&removed.parent_connection_id) {
            *slot = slot.saturating_sub(freed);
        }
        if let Some(order) = self.completed_order.get_mut(&removed.parent_connection_id) {
            order.retain(|id| id != call_id);
        }
        Some(removed)
    }

    /// Re-apply the current `completed_cap_bytes` to EVERY parent. Called by
    /// `set_config` when the cap may have been LOWERED at runtime, so
    /// already-retained results are pruned promptly — insert-time eviction alone
    /// would otherwise strand them until a parent's next completion (which may
    /// never arrive).
    fn enforce_completed_cap_all_parents(&mut self) {
        if self.completed_cap_bytes == 0 {
            return;
        }
        let parents: Vec<String> = self.completed_bytes.keys().cloned().collect();
        for parent in parents {
            self.evict_completed_over_cap(&parent);
        }
    }

    /// Forget every completed result for a parent. Called on connection
    /// teardown (the parent is gone — nothing left to query). A turn cancel
    /// deliberately does NOT call this: the connection stays alive and the LLM
    /// may still query its just-canceled tasks.
    fn drop_completed_for_parent(&mut self, parent_connection_id: &str) {
        self.completed_bytes.remove(parent_connection_id);
        if let Some(ids) = self.completed_order.remove(parent_connection_id) {
            for id in ids {
                self.completed.remove(&id);
            }
        }
    }
}

/// Cap result text retained in the completed-cache. The full output always
/// lives in the child session; this only bounds the broker's copy.
fn cap_completed_text(text: &str) -> String {
    truncate_on_char_boundary(text, COMPLETED_TEXT_CAP)
}

/// Build the bounded inline preview carried by the `DelegationCompleted` event
/// and terminal meta. `None` for empty text.
fn build_text_preview(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    Some(truncate_on_char_boundary(text, STATUS_PREVIEW_CAP))
}

/// Truncate `s` so the RESULT (including the appended ellipsis) is at most `cap`
/// bytes, cut on a UTF-8 char boundary. Reserving the ellipsis bytes keeps the
/// output within the advertised cap rather than `cap + 3`.
fn truncate_on_char_boundary(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    const ELLIPSIS: &str = "…";
    // Leave room for the ellipsis; clamp at 0 for pathologically small caps.
    let budget = cap.saturating_sub(ELLIPSIS.len());
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ELLIPSIS}", &s[..end])
}

/// Derive the completed-cache fields (status / text / error_code / message)
/// from a resolved [`DelegationOutcome`]. `Canceled`-coded errors map to
/// [`TaskStatus::Canceled`]; every other error maps to [`TaskStatus::Failed`].
fn terminal_fields(
    outcome: &DelegationOutcome,
) -> (TaskStatus, Option<String>, Option<String>, Option<String>) {
    match outcome {
        DelegationOutcome::Ok(ok) => (
            TaskStatus::Completed,
            Some(cap_completed_text(&ok.text)),
            None,
            None,
        ),
        DelegationOutcome::Err { code, message, .. } => {
            let status = if code == "canceled" {
                TaskStatus::Canceled
            } else {
                TaskStatus::Failed
            };
            (status, None, Some(code.clone()), Some(message.clone()))
        }
    }
}

/// Build a [`CompletedTask`] from a resolved outcome for the completed-cache.
fn build_completed(
    parent_connection_id: &str,
    child_conversation_id: i32,
    agent_type: AgentType,
    duration_ms: u64,
    outcome: &DelegationOutcome,
) -> CompletedTask {
    let (status, text, error_code, message) = terminal_fields(outcome);
    CompletedTask {
        parent_connection_id: parent_connection_id.to_string(),
        child_conversation_id,
        agent_type,
        status,
        text,
        error_code,
        message,
        duration_ms,
    }
}

/// A `canceled`-coded [`DelegationOutcome`] carrying the child conversation id.
fn canceled_outcome(child_conversation_id: i32, reason: &str) -> DelegationOutcome {
    DelegationOutcome::from_err(
        DelegationError::Canceled {
            reason: reason.to_string(),
        },
        Some(child_conversation_id),
    )
}

/// Remove `keys` from `running`, recording each as a `Canceled` completed entry
/// (so a `get_delegation_status` still answers) and returning the drained tasks
/// — each paired with the `duration_ms` captured at this drain point — for I/O
/// teardown. MUST be called with the pending lock held so the running →
/// completed migration is atomic.
///
/// The duration is captured ONCE here and returned so the slow teardown
/// (parent-card meta, report) reuses the exact value recorded into the
/// completed-cache, rather than recomputing `started_at.elapsed()` later — which
/// would inflate it for the backgrounded `cancel_by_parent_turn` teardown and
/// disagree with the `get_delegation_status` / `cancel_delegation` cards.
fn drain_and_record_canceled(
    inner: &mut PendingInner,
    keys: Vec<String>,
    reason: &str,
) -> Vec<(RunningTask, u64)> {
    let mut out = Vec::with_capacity(keys.len());
    for k in keys {
        let task = inner.running.remove(&k).expect("key just observed");
        let outcome = canceled_outcome(task.child_conversation_id, reason);
        let duration_ms = task.started_at.elapsed().as_millis() as u64;
        inner.insert_completed(
            &k,
            build_completed(
                &task.parent_connection_id,
                task.child_conversation_id,
                task.agent_type,
                duration_ms,
                &outcome,
            ),
        );
        out.push((task, duration_ms));
    }
    out
}

/// Project a `DelegationOutcome` + broker-measured `duration_ms` onto the
/// wire-stable `DelegationResultSummary` carried by `DelegationCompleted`.
/// Keeps the mapping (and the bounded `text_preview`) in one place.
fn outcome_to_summary(outcome: &DelegationOutcome, duration_ms: u64) -> DelegationResultSummary {
    match outcome {
        DelegationOutcome::Ok(ok) => DelegationResultSummary::Ok {
            duration_ms,
            text_preview: build_text_preview(&ok.text),
        },
        DelegationOutcome::Err { code, .. } => DelegationResultSummary::Err {
            error_code: code.clone(),
        },
    }
}

/// Project a resolved outcome onto a terminal [`DelegationTaskReport`] (used by
/// the setup-window terminal dispositions and the test shim).
fn report_from_outcome(
    task_id: Option<String>,
    agent_type: Option<AgentType>,
    outcome: &DelegationOutcome,
    duration_ms: Option<u64>,
) -> DelegationTaskReport {
    let (status, text, error_code, message) = terminal_fields(outcome);
    let child_conversation_id = match outcome {
        DelegationOutcome::Ok(ok) => Some(ok.child_conversation_id),
        DelegationOutcome::Err {
            child_conversation_id,
            ..
        } => *child_conversation_id,
    };
    DelegationTaskReport {
        task_id,
        status,
        child_conversation_id,
        agent_type,
        text,
        error_code,
        message,
        duration_ms,
        // Terminal by construction — nothing is waiting on the user anymore.
        blocked_on: None,
    }
}

/// Build a `Failed`/`Canceled` report for a setup error (no task id — setup
/// failed before/around registration, so the LLM has no task to track).
fn report_err(
    agent_type: AgentType,
    err: DelegationError,
    child_conversation_id: Option<i32>,
) -> DelegationTaskReport {
    let outcome = DelegationOutcome::from_err(err, child_conversation_id);
    report_from_outcome(None, Some(agent_type), &outcome, None)
}

/// The `Running` ack returned by `start_delegation` for a backgrounded task.
fn running_ack(
    call_id: String,
    child_conversation_id: i32,
    agent_type: AgentType,
) -> DelegationTaskReport {
    // Embed the literal task_id in the message so it survives clients that only
    // surface the MCP `content` text (not `structuredContent`) — without it the
    // LLM couldn't call get_delegation_status / cancel_delegation.
    let message = format!(
        "Delegation successful. task_id={call_id}. Call get_delegation_status \
         with this id in the task_ids array (optionally wait_ms) to collect the \
         result, or cancel_delegation to stop it."
    );
    DelegationTaskReport {
        task_id: Some(call_id),
        status: TaskStatus::Running,
        child_conversation_id: Some(child_conversation_id),
        agent_type: Some(agent_type),
        text: None,
        error_code: None,
        message: Some(message),
        duration_ms: None,
        blocked_on: None,
    }
}

/// Cap on the free-form `reason` a `resume_delegation` call may carry into the
/// child's continuation prompt. The reason is interruption CONTEXT, not new
/// instructions — the cap (shared with `TASK_PREVIEW_CAP`'s budget) keeps it
/// from smuggling a task-sized payload past the "no new iterations" contract.
const RESUME_REASON_CAP: usize = 2 * 1024;

/// The wire-stable `error_code` on every refused `resume_delegation` report.
/// Distinct from the task's own status so the caller can tell "this task is
/// completed" (status) from "and that is why it was not resumed" (code).
pub const NOT_RESUMABLE_CODE: &str = "not_resumable";

/// Build the fixed continuation prompt a resumed child receives. Deliberately
/// NOT caller-controlled beyond the bounded `reason`: `resume_delegation`
/// continues the ORIGINAL task and must not be usable as a second
/// `delegate_to_agent` (see the tool schema's contract).
pub fn build_resume_prompt(reason: Option<&str>) -> String {
    let mut prompt = String::from(
        "This delegated task was interrupted before completion and is now being \
         resumed. Re-read the original task from the earlier messages in this \
         session, assess what has already been done (partial work may exist on \
         disk), continue from where it stopped, and finish the ORIGINAL task. \
         Do not take on any new work beyond that original scope.",
    );
    if let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) {
        prompt.push_str("\nContext on why the run was interrupted / resumed: ");
        prompt.push_str(&truncate_on_char_boundary(reason, RESUME_REASON_CAP));
    }
    prompt
}

/// The `Running` ack returned by `resume_delegation` once the child is back up.
/// Same shape as [`running_ack`], resume-flavored first sentence — which must
/// NOT start with `"Delegation successful. task_id="`: Cursor's completion-time
/// result sniff (`cursor_companion_title_from_content`) anchors on that exact
/// prefix to re-title identity-less calls as `delegate_to_agent`.
/// A confirmed resume.
///
/// The opening "Delegation resumed" is load-bearing, like the "Not resumed" of
/// [`not_resumable_report`]: `companion::render_task_report` renders only
/// `message` as content text and keeps the rest in `structuredContent`, which
/// some hosts drop wholesale (OpenCode — see `acp::connection`). There the
/// words are all that separates this from a refusal or an
/// [`unknown_report`], so the card can still tell a real resume from the
/// model naming somebody else's task. `isAffirmedResume` in
/// `src/lib/delegation-card.ts` reads it; keep the two spellings in step.
fn resume_ack(
    call_id: String,
    child_conversation_id: i32,
    agent_type: AgentType,
) -> DelegationTaskReport {
    let message = format!(
        "Delegation resumed. task_id={call_id} (unchanged). Call \
         get_delegation_status with this id in the task_ids array (optionally \
         wait_ms) to collect the result, or cancel_delegation to stop it."
    );
    DelegationTaskReport {
        task_id: Some(call_id),
        status: TaskStatus::Running,
        child_conversation_id: Some(child_conversation_id),
        agent_type: Some(agent_type),
        text: None,
        error_code: None,
        message: Some(message),
        duration_ms: None,
        blocked_on: None,
    }
}

/// A refused resume: reports the task's ACTUAL current status (so the caller
/// learns where the task really stands) with `error_code: "not_resumable"` and
/// a message that opens with "Not resumed" — unambiguous against the ack even
/// on hosts that only surface the content text.
///
/// That prefix is load-bearing, not decorative: `isRefusedResume` in
/// `src/lib/delegation-card.ts` falls back to it wherever `error_code` did not
/// survive, and without it a refusal renders as a resumed sub-agent card. Keep
/// the two spellings in step.
fn not_resumable_report(
    task_id: &str,
    status: TaskStatus,
    child_conversation_id: Option<i32>,
    agent_type: Option<AgentType>,
    why: &str,
) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status,
        child_conversation_id,
        agent_type,
        text: None,
        error_code: Some(NOT_RESUMABLE_CODE.to_string()),
        message: Some(format!("Not resumed: {why}")),
        duration_ms: None,
        blocked_on: None,
    }
}

/// How long [`DelegationBroker::get_task_status`] may block before returning the
/// current (possibly still-running) snapshot. Derived by the listener from the
/// MCP tool's `wait_ms`: omitted → [`Immediate`], an explicit `0` → [`Infinite`],
/// any positive value → [`Bounded`] (clamped to the listener's hard ceiling).
///
/// [`Immediate`]: StatusWait::Immediate
/// [`Bounded`]: StatusWait::Bounded
/// [`Infinite`]: StatusWait::Infinite
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusWait {
    /// Return the current snapshot right away — the default poll.
    Immediate,
    /// Block up to this many milliseconds, then return whatever snapshot we have
    /// (the child keeps running past the deadline; the caller re-issues to wait
    /// more).
    Bounded(u64),
    /// Block until the task reaches a terminal state — never time out. Lets a
    /// long-running child be awaited in a single call. A parent disconnect or
    /// cancel also drives the task terminal (and fires the completion signal),
    /// so this never outlives the task itself.
    Infinite,
}

/// How long an already-reported blocking prompt stays suppressed before a
/// status long-poll will surface it again. Two jobs: it keeps a `wait_ms: 0`
/// caller from spinning on a block it has already been told about, and it
/// bounds the damage when a report is generated but never delivered (see
/// [`RunningTask::last_surfaced_block`]) — a lost report costs a delayed notice
/// instead of a permanent stall. Long relative to a human noticing a prompt,
/// short relative to walking away from the machine.
const BLOCK_RESURFACE_INTERVAL: Duration = Duration::from_secs(300);

/// Second line of a blocked running report's message. The frontend anchors on
/// this exact prefix to keep recognizing the poll as "running" on hosts that
/// persist only the `CallToolResult` content text — see `textRunningStatus` in
/// `src/lib/delegation-status.ts`. Backend protocol string, never localized.
const BLOCKED_LINE_PREFIX: &str = "Awaiting your decision:";

/// Mark a running report as parked on a user decision: set the structured
/// `blocked_on` and replace the bare `"Running."` message with a two-line
/// version naming what needs answering.
///
/// The note goes on its OWN line, for the same reason the live-reply hint does:
/// content-only hosts recognize a still-running poll by the standalone first
/// line `"Running."`, so a *completed* result whose text happens to start with
/// "Running. …" can never be misread as running.
fn attach_blocked(report: &mut DelegationTaskReport, blocked: BlockedOn) {
    let what = match (&blocked.kind, blocked.title.as_deref()) {
        (_, Some(title)) => title.to_string(),
        (BlockedKind::Permission, None) => "a tool permission".to_string(),
        (BlockedKind::Question, None) => "a question".to_string(),
        (BlockedKind::PlanApproval, None) => "a plan approval".to_string(),
    };
    // The third line is guidance for the orchestrating LLM, in the same spirit
    // as `running_ack`'s "call get_delegation_status with this id". Keep it in
    // step with the tool description in `tool_schema.json`.
    report.message = Some(format!(
        "Running.\n{BLOCKED_LINE_PREFIX} {what}\nThe sub-agent is parked until \
         a human answers in Codeg — it is not making progress. Tell the user, \
         then wait again."
    ));
    report.blocked_on = Some(blocked);
}

/// Status report for a still-running task.
fn running_report(task_id: &str, task: &RunningTask) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status: TaskStatus::Running,
        child_conversation_id: Some(task.child_conversation_id),
        agent_type: Some(task.agent_type),
        text: None,
        error_code: None,
        // Bare baseline; `get_task_status` upgrades this to a two-line
        // "Running.\nLatest sub-agent reply: …" when the child has live output.
        message: Some("Running.".to_string()),
        duration_ms: None,
        blocked_on: None,
    }
}

/// Status report from a cached completed result.
fn completed_report(task_id: &str, c: &CompletedTask) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status: c.status,
        child_conversation_id: Some(c.child_conversation_id),
        agent_type: Some(c.agent_type),
        text: c.text.clone(),
        error_code: c.error_code.clone(),
        message: c.message.clone(),
        duration_ms: Some(c.duration_ms),
        blocked_on: None,
    }
}

/// Status report when a task id isn't known to the caller (never existed,
/// owned by a different parent, or evicted with no DB record).
fn unknown_report(task_id: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status: TaskStatus::Unknown,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: None,
        message: Some(
            "Unknown task id — it never existed, isn't owned by this session, \
             or its result was evicted with no stored record."
                .to_string(),
        ),
        duration_ms: None,
        blocked_on: None,
    }
}

/// Status report recovered from the DB after the in-memory result was evicted.
/// Carries status only — the full output lives in the child session.
fn db_report(task_id: &str, rec: &ChildStatusRecord) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status: rec.status,
        child_conversation_id: Some(rec.child_conversation_id),
        agent_type: Some(rec.agent_type),
        text: None,
        error_code: (rec.status == TaskStatus::Canceled).then(|| "canceled".to_string()),
        message: Some(format!(
            "Result no longer cached; open child session {} for the full output.",
            rec.child_conversation_id
        )),
        duration_ms: None,
        blocked_on: None,
    }
}

/// What [`DelegationBroker::claim_new_blocks`] decided for one status pass.
#[derive(Default)]
struct ClaimedBlocks {
    /// At least one blocking prompt is reportable now — the wait should return.
    any_claimed: bool,
    /// Earliest moment a currently-suppressed prompt becomes reportable again.
    /// Bounds how long an otherwise-unbounded park may sleep, so the
    /// resurface valve actually fires instead of waiting on a notify that will
    /// never come (a blocked child emits nothing further on its own).
    retry_at: Option<Instant>,
}

/// Per-id classification captured under the pending lock during a (possibly
/// batched) status query. The async resolution that can't run under the lock —
/// `attach_live_reply` (a different lock) for a running task, `status_from_db`
/// (a DB round-trip) for one not in memory — is deferred to `assemble_reports`
/// AFTER the lock is released, so a status query never nests the pending lock
/// inside another await. This is the same lock-ordering the single-task path
/// has always used; batching just captures it per id.
enum StatusClass {
    /// Terminal/owned-cached, or a cross-parent `unknown` — the report is final.
    Settled(DelegationTaskReport),
    /// Running and owned — the bare running snapshot plus its child connection
    /// id, so `assemble_reports` can attach the latest live reply out of lock.
    Running {
        report: DelegationTaskReport,
        child_connection_id: String,
    },
    /// Neither running nor completed in memory — resolve via the DB fallback in
    /// `assemble_reports`. A not-in-memory id is, for wait purposes, already
    /// settled: it can never transition back to running, so a batch wait need
    /// not park on it (and must not hit the DB on every wake).
    NotInMemory,
}

/// Classify one task id against the in-memory maps while the pending lock is
/// held. Mirrors the single-task resolution order — completed cache (parent
/// scoped) → running set (parent scoped) → not-in-memory — and yields a
/// cross-parent hit as `unknown` so a task owned by another parent never leaks.
fn classify_locked(inner: &PendingInner, parent_connection_id: &str, task_id: &str) -> StatusClass {
    if let Some(c) = inner.completed.get(task_id) {
        if c.parent_connection_id == parent_connection_id {
            return StatusClass::Settled(completed_report(task_id, c));
        }
        return StatusClass::Settled(unknown_report(task_id));
    }
    match inner.running.get(task_id) {
        Some(r) if r.parent_connection_id == parent_connection_id => StatusClass::Running {
            report: running_report(task_id, r),
            child_connection_id: r.child_connection_id.clone(),
        },
        Some(_) => StatusClass::Settled(unknown_report(task_id)),
        None => StatusClass::NotInMemory,
    }
}

/// Map a terminal [`DelegationTaskReport`] back to a [`DelegationOutcome`] for
/// the test-only `handle_request` shim (so pre-async tests keep asserting on
/// the old outcome shape).
#[cfg(any(test, feature = "test-utils"))]
fn report_to_outcome(report: &DelegationTaskReport) -> DelegationOutcome {
    use crate::acp::delegation::types::DelegationSuccess;
    match report.status {
        TaskStatus::Completed => DelegationOutcome::Ok(DelegationSuccess {
            text: report.text.clone().unwrap_or_default(),
            child_conversation_id: report.child_conversation_id.unwrap_or(0),
            child_agent_type: report.agent_type.unwrap_or(AgentType::ClaudeCode),
            turn_count: 1,
            duration_ms: report.duration_ms.unwrap_or(0),
            token_usage: None,
        }),
        // Running never reaches here (the shim loops until terminal); the other
        // states all project onto Err.
        _ => DelegationOutcome::Err {
            code: report
                .error_code
                .clone()
                .unwrap_or_else(|| "canceled".to_string()),
            message: report.message.clone().unwrap_or_default(),
            child_conversation_id: report.child_conversation_id,
        },
    }
}

/// Build the `Canceled { reason }` string for a child that ended without a
/// clean `TurnComplete`, optionally stitching in the terminal `Error` detail.
/// Shared by `cancel_by_child_connection` and `handle_request`'s early-terminal
/// pickup so both surface the same wording.
fn child_canceled_reason(terminal_error: Option<&str>) -> String {
    match terminal_error {
        Some(detail) if !detail.trim().is_empty() => {
            format!("child session ended without TurnComplete: {detail}")
        }
        _ => "child session ended without TurnComplete".to_string(),
    }
}

/// Set of MCP-side `external_handle` tokens for which the companion
/// already received `notifications/cancelled` BEFORE the matching
/// `handle_request` reached the pending-registration phase. Without
/// this pre-cancel buffer, a fast cancel that lands during the
/// pre-check / spawn window would find no entry in `pending`, drop
/// silently, and let the broker proceed to spawn a child the caller
/// no longer wants. `handle_request` consults this set both at entry
/// (so we never even spawn) and immediately after parking the pending
/// entry (so a cancel landing mid-spawn still wins).
///
/// Capped at [`PRE_CANCELED_CAP`] so a misbehaving MCP client (or a
/// pathological cancel-for-unknown-id storm) can't grow the set
/// without bound. Eviction is FIFO via the parallel `order` deque,
/// which is fine because pre-cancels only matter for the short window
/// between the cancel and the late-arriving `handle_request`.
#[derive(Default)]
struct PreCanceledHandles {
    inner: Mutex<PreCanceledState>,
}

#[derive(Default)]
struct PreCanceledState {
    set: HashSet<String>,
    order: VecDeque<String>,
}

const PRE_CANCELED_CAP: usize = 256;

/// Per-parent tracking of `tool_call_id`s that the ACP lifecycle
/// observed firing `delegate_to_agent`. MCP clients (Codex, Claude
/// Code) generally do NOT populate `_meta.tool_use_id` when invoking
/// an MCP tool, so the broker can't read the LLM-issued
/// `tool_use_id` from the wire — we capture it from the parallel ACP
/// `tool_call` event stream instead.
///
/// Each bucket holds two FIFOs under the SAME mutex:
///
/// * `pending` — ids the lifecycle has registered but the matching
///   broker round-trip has not yet claimed. UNKEYED entries are subject
///   to [`PENDING_TOOL_CALL_TTL`] eviction so an anonymous ACP id whose
///   MCP round-trip never arrives can't linger and FIFO-mis-bind a later
///   delegation. KEYED entries carry no count cap: they are drained only
///   by their exact-match claim, by terminal tombstoning
///   (`tombstone_pending_tool_call`), or by per-parent teardown — because
///   the host may serialize a delegation's round-trip arbitrarily far
///   behind earlier long-running ones, so a count cap would drop a
///   still-pending keyed id and orphan its card.
/// * `consumed` — ids that were already claimed by a prior
///   round-trip, or terminal-tombstoned (`tombstone_pending_tool_call`).
///   NEITHER subject to TTL eviction NOR to a per-bucket
///   cap: a delegated child agent may run for minutes to hours, and
///   the host can re-emit the same `tool_call` (e.g. as a `completed`
///   status flip) at the end of that run, so the consumed memory
///   must outlast the entire parent-side tool call lifetime. It is
///   scoped to the parent connection's lifetime instead, cleared by
///   `drop_pending_tool_calls_for_parent` on disconnect. The growth
///   is bounded by how many entries ever register for the parent:
///   `delegate_to_agent` calls (typically tens at most) plus, on
///   Cursor connections, one identity-less "MCP: tool" candidate per
///   MCP call the session makes (see `acp::lifecycle`'s
///   `CURSOR_IDENTITYLESS_MCP_TITLE` registration) — with each
///   `(String, Instant)` entry costing well under 100 bytes, an
///   unbounded set stays comfortable for realistic sessions without
///   OOM risk in the typical operating envelope.
///
/// Co-locating the two halves under one lock makes the
/// claim → mark-consumed pair atomic. A host re-emit racing with the
/// claim cannot observe an empty pending queue AND a consumed memory
/// that does not yet remember the id; consequently it cannot inject
/// a stale duplicate that would mis-bind the next delegation.
#[derive(Default)]
struct ToolCallTracker {
    inner: Mutex<HashMap<String, ToolCallTrackerBucket>>,
}

/// The arguments that uniquely identify a `delegate_to_agent` invocation,
/// used to correlate a parent-side ACP `tool_call` to the matching MCP
/// `tools/call` round-trip. All three fields are values the LLM passed
/// identically to both wire paths, so the triple is the deterministic key
/// when a parent fires several `delegate_to_agent` calls in parallel —
/// matching on `task` alone would swap two calls targeting different agents
/// with the same task, and adding `agent_type` alone would still swap two
/// same-agent/same-task calls aimed at different directories (e.g. "run
/// tests" against `/repo-a` vs `/repo-b`).
///
/// `working_dir` here is the value the LLM EXPLICITLY passed (`None` when
/// omitted), NOT the listener-defaulted spawn directory: the listener
/// defaults a missing MCP `working_dir` to the parent's launch dir, but the
/// ACP `raw_input` omits it then too, so keying on the explicit value keeps
/// both sides symmetric (`None == None`) for the common omitted case while
/// still distinguishing two calls that name different directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DelegationMatchKey {
    pub agent_type: AgentType,
    pub task: String,
    pub working_dir: Option<String>,
}

/// One captured parent-side `delegate_to_agent` tool_call awaiting its
/// matching MCP round-trip.
struct PendingToolCall {
    tool_call_id: String,
    /// The `(agent_type, task, working_dir)` correlation key parsed from the ACP
    /// tool_call's `raw_input`. Matched against the MCP round-trip's own
    /// key so parallel `delegate_to_agent` calls each bind to their own
    /// `tool_call_id` regardless of arrival order — pure arrival-order FIFO
    /// can mis-assign them (or, when one MCP round-trip out-races the
    /// matching ACP event, orphan to a synthetic id). `None` when the host
    /// shipped no parseable `raw_input` at ToolCall time; such entries are
    /// claimable ONLY via the post-budget FIFO fallback
    /// (`take_pending_tool_call`), never the in-loop key-match path.
    match_key: Option<DelegationMatchKey>,
    /// True when the entry came from an identity-less MCP announcement
    /// (Cursor's `"MCP: tool"` + empty input — see
    /// `lifecycle::CURSOR_IDENTITYLESS_MCP_TITLE`). Such an id belongs to
    /// *some* codeg-mcp call whose identity only the companion round-trip can
    /// reveal, so it is additionally claimable by
    /// [`DelegationBroker::rewrite_identityless_tool_call`] (the
    /// `get_delegation_status` / `cancel_delegation` call-time rename), and a
    /// `delegate_to_agent` claim that lands on one triggers the identity
    /// write. Plain unkeyed entries (a delegation invocation whose args were
    /// unparseable) keep `false` and are never renamed.
    identityless: bool,
    registered_at: Instant,
}

#[derive(Default)]
struct ToolCallTrackerBucket {
    pending: VecDeque<PendingToolCall>,
    consumed: VecDeque<(String, Instant)>,
    /// Sticky "this parent has EVER announced an identity-less MCP call" flag.
    /// Gates [`DelegationBroker::rewrite_identityless_tool_call`]'s poll: on
    /// hosts that ship real identities (Claude/Codex/…) no identity-less
    /// entry ever registers, the flag stays `false`, and every status/cancel
    /// round-trip skips the rename at zero cost instead of burning the poll
    /// budget. Never cleared on claim — only with the bucket on teardown.
    identityless_seen: bool,
}

/// Maximum age before a `pending` entry is discarded as stale — but ONLY for
/// UNKEYED entries (anonymous, arrival-order correlated). KEYED entries are
/// retained regardless of age: each is claimed solely by an exact key match,
/// so it can't mis-bind a later delegation, and its MCP round-trip may be
/// serialized arbitrarily far behind earlier long-running delegations (Claude
/// Code runs parallel `delegate_to_agent` calls one-at-a-time — observed gap
/// 77 s). See the retain block in `take_matching_tool_call_at`.
/// 60 s comfortably covers the ACP→MCP race for the unkeyed case (<5 ms
/// typical) while still GC'ing a forgotten anonymous id before it can
/// FIFO-mis-bind a subsequent unkeyed delegation.
///
/// The `consumed` side has no TTL — see [`ToolCallTrackerBucket`] — because
/// long-running delegations can re-emit the parent-side `tool_call` well past
/// this window.
const PENDING_TOOL_CALL_TTL: Duration = Duration::from_secs(60);

/// Poll cadence and budget used by `claim_pending_tool_call_with_brief_wait`
/// to correlate an MCP `delegate_to_agent` round-trip to its parent-side
/// ACP `tool_call_id`. The exact-match path returns instantly; this budget is
/// spent while waiting for THIS delegation's own `tool_call` to register (or to
/// backfill its key onto an already-registered entry) so we bind by exact match
/// instead of stealing a parallel sibling's id, or while no claimable id has
/// arrived yet. Unkeyed entries are never claimed in-loop — arrival-order FIFO
/// is deferred to the post-budget last resort, which runs only after the caller
/// has waited the full budget (the correct clock for "this delegation has no
/// key coming"), so a round-trip can't grab a sibling's not-yet-keyed id
/// mid-race.
///
/// 200 × 10 ms = 2 s. This budget only matters when the MCP round-trip
/// out-races its own ACP `tool_call` registration — i.e. the `tools/call`
/// reaches the broker before the in-process `session/update(tool_call)` (and
/// any slightly-later `ToolCallUpdate` carrying the `agent_type`/`task` args)
/// has registered the key. That race is sub-5ms locally; the headroom covers
/// busier hosts and split arg streaming. The wait is invisible in the happy
/// path (it returns the instant the key matches) and negligible against the
/// multi-second-to-minutes child run it precedes.
///
/// NOTE: the budget is NOT what protects a *serialized* second delegation
/// whose round-trip lands many seconds after its tool_call registered (Claude
/// Code runs parallel `delegate_to_agent` calls one-at-a-time, so the 2nd may
/// arrive minutes later). That id is already registered and waiting — the
/// thing that used to orphan it was age-eviction, now fixed by retaining keyed
/// entries indefinitely (see `take_matching_tool_call_at`'s retain
/// block). A host that emits no observable ACP `tool_call` at all still falls
/// through to the synthetic id after the budget, exactly as before.
const CLAIM_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLAIM_POLL_ATTEMPTS: usize = 200;

/// Poll budget for the status/cancel call-time rename
/// ([`DelegationBroker::rewrite_identityless_tool_call`]): 30 × 10 ms = 300 ms.
/// Much shorter than the delegate claim budget because the rename is
/// best-effort (a miss falls back to the completion-time result sniff) and the
/// poll delays the status snapshot the LLM is waiting on. It only has to
/// absorb the in-process ordering race between the ACP announcement landing in
/// the lifecycle dispatcher and the companion's UDS round-trip reaching the
/// listener — sub-5 ms in practice; the sticky `identityless_seen` gate keeps
/// hosts that never announce identity-less calls at zero cost.
const IDENTITYLESS_RENAME_POLL_ATTEMPTS: usize = 30;

/// The broker is intentionally `Clone` (cheap — only `Arc`s inside) so
/// listener/handler code can hand copies to spawned tasks without lifetime
/// gymnastics.
#[derive(Clone)]
pub struct DelegationBroker {
    spawner: Arc<dyn ConnectionSpawner>,
    depth_lookup: Arc<dyn ConversationDepthLookup>,
    /// Writer for `meta["codeg.delegation"]` on the parent's active
    /// `delegate_to_agent` ToolCallState. Defaults to a no-op so tests
    /// that aren't exercising the meta lifecycle don't need to wire
    /// anything; production constructs the broker with the
    /// `ConnectionManagerMetaWriter` via `with_writers`.
    meta_writer: Arc<dyn DelegationMetaWriter>,
    /// Emitter for `AcpEvent::DelegationCompleted` against the parent
    /// connection's event stream. Same Noop/Mock/Production scheme as
    /// the meta writer — production wires `ConnectionManagerEventEmitter`
    /// via `with_writers`; tests that don't observe the event lifecycle
    /// take the default Noop.
    event_emitter: Arc<dyn DelegationEventEmitter>,
    /// DB fallback for `get_delegation_status` / `cancel_delegation` once a
    /// task's result aged out of the in-memory completed-cache. Defaults to a
    /// no-op ("unknown"); production wires `DbChildStatusLookup` via
    /// `with_status_lookup`.
    status_lookup: Arc<dyn ChildStatusLookup>,
    /// Peeks a still-running child's live session for a one-line progress hint,
    /// used to enrich `get_delegation_status`'s running report. Defaults to a
    /// no-op ("no hint"); production wires `ConnectionManagerLiveReplyLookup` via
    /// `with_live_reply_lookup`.
    live_reply_lookup: Arc<dyn ChildLiveReplyLookup>,
    pending: Arc<PendingCalls>,
    tool_calls: Arc<ToolCallTracker>,
    pre_canceled_handles: Arc<PreCanceledHandles>,
    config: Arc<Mutex<DelegationConfig>>,
    /// Woken after every terminal `record_completed` so a `get_delegation_status`
    /// long-poll wakes the instant its task finishes instead of busy-polling.
    result_notify: Arc<Notify>,
    /// How long an already-reported blocking prompt stays suppressed —
    /// [`BLOCK_RESURFACE_INTERVAL`] in production. A field only so tests can
    /// shrink it; nothing outside tests ever sets it.
    block_resurface: Duration,
}

impl DelegationBroker {
    pub fn new(
        spawner: Arc<dyn ConnectionSpawner>,
        depth_lookup: Arc<dyn ConversationDepthLookup>,
    ) -> Self {
        Self::with_writers(
            spawner,
            depth_lookup,
            Arc::new(NoopMetaWriter) as Arc<dyn DelegationMetaWriter>,
            Arc::new(NoopEventEmitter) as Arc<dyn DelegationEventEmitter>,
        )
    }

    /// Test-only constructor that injects a meta writer but keeps the
    /// default Noop event emitter. Retained so existing meta-focused
    /// tests don't have to mention the emitter parameter. New callsites
    /// (and production wiring) should prefer `with_writers`.
    pub fn with_meta_writer(
        spawner: Arc<dyn ConnectionSpawner>,
        depth_lookup: Arc<dyn ConversationDepthLookup>,
        meta_writer: Arc<dyn DelegationMetaWriter>,
    ) -> Self {
        Self::with_writers(
            spawner,
            depth_lookup,
            meta_writer,
            Arc::new(NoopEventEmitter) as Arc<dyn DelegationEventEmitter>,
        )
    }

    /// Production-grade constructor wiring the broker to both a real
    /// meta writer (`ConnectionManagerMetaWriter`) AND an event emitter
    /// (`ConnectionManagerEventEmitter`). Tests that observe the full
    /// lifecycle (meta writes + DelegationCompleted emits) should use
    /// this with `MockMetaWriter` + `MockEventEmitter`.
    pub fn with_writers(
        spawner: Arc<dyn ConnectionSpawner>,
        depth_lookup: Arc<dyn ConversationDepthLookup>,
        meta_writer: Arc<dyn DelegationMetaWriter>,
        event_emitter: Arc<dyn DelegationEventEmitter>,
    ) -> Self {
        Self {
            spawner,
            depth_lookup,
            meta_writer,
            event_emitter,
            status_lookup: Arc::new(NoopChildStatusLookup),
            live_reply_lookup: Arc::new(NoopChildLiveReplyLookup),
            pending: Arc::new(PendingCalls::default()),
            tool_calls: Arc::new(ToolCallTracker::default()),
            pre_canceled_handles: Arc::new(PreCanceledHandles::default()),
            config: Arc::new(Mutex::new(DelegationConfig::default())),
            result_notify: Arc::new(Notify::new()),
            block_resurface: BLOCK_RESURFACE_INTERVAL,
        }
    }

    /// Replace the DB status fallback used by `get_delegation_status` /
    /// `cancel_delegation` for tasks evicted from the in-memory completed-cache.
    /// Builder-style so the production wiring can layer it onto `with_writers`
    /// without growing that constructor's arity, and tests can opt in.
    pub fn with_status_lookup(mut self, status_lookup: Arc<dyn ChildStatusLookup>) -> Self {
        self.status_lookup = status_lookup;
        self
    }

    /// Replace the live-reply lookup used to enrich `get_delegation_status`'s
    /// running report with the child's latest one-line progress. Builder-style,
    /// layered onto `with_writers` by the production wiring; tests opt in with a
    /// `MockChildLiveReplyLookup`.
    pub fn with_live_reply_lookup(
        mut self,
        live_reply_lookup: Arc<dyn ChildLiveReplyLookup>,
    ) -> Self {
        self.live_reply_lookup = live_reply_lookup;
        self
    }

    /// Shrink [`BLOCK_RESURFACE_INTERVAL`] so a test can observe the
    /// delivery-failure valve without waiting minutes. Test-only by
    /// construction — production always keeps the constant.
    #[cfg(test)]
    fn with_block_resurface(mut self, interval: Duration) -> Self {
        self.block_resurface = interval;
        self
    }

    /// Record a parent ACP `tool_call_id` whose title indicates the LLM is
    /// invoking `delegate_to_agent`. The next broker round-trip from the
    /// same `parent_connection_id` will claim this id as its
    /// `parent_tool_use_id`. Bounded FIFO per connection.
    ///
    /// Two-tier dedupe against host re-emits of `sessionUpdate(tool_call)`
    /// (some hosts use the non-update variant to ship status flips and
    /// late-arriving `raw_input` chunks):
    ///
    /// 1. **In-queue**: if the id is still waiting to be claimed, drop
    ///    the re-emit — the first push will be consumed by the matching
    ///    MCP round-trip.
    /// 2. **Recently consumed**: if the id was already claimed for an
    ///    earlier delegation on the same parent, drop the re-emit —
    ///    otherwise it would sit in the queue as a stale id and mis-
    ///    bind the **next** delegation's MCP round-trip. The consumed
    ///    memory persists for the parent connection's lifetime (no
    ///    TTL, no cap) so a host re-emit at terminal status flip is
    ///    still rejected even if the delegation ran for hours.
    pub async fn register_pending_tool_call(
        &self,
        parent_connection_id: &str,
        tool_call_id: String,
    ) {
        self.register_pending_tool_call_with_key_at(
            parent_connection_id,
            tool_call_id,
            None,
            false,
            Instant::now(),
        )
        .await;
    }

    /// Register an id announced with NO identity at all — Cursor's
    /// `"MCP: tool"` + empty-input shape, where the wire never reveals which
    /// MCP tool the call is. The entry is unkeyed (claimable by the
    /// post-budget FIFO fallback, exactly like a keyless delegation
    /// invocation) and additionally marked `identityless`, which (a) makes it
    /// claimable by the status/cancel call-time rename
    /// ([`Self::rewrite_identityless_tool_call`]) and (b) triggers the
    /// identity write when a `delegate_to_agent` claim lands on it.
    pub async fn register_identityless_tool_call(
        &self,
        parent_connection_id: &str,
        tool_call_id: String,
    ) {
        self.register_pending_tool_call_with_key_at(
            parent_connection_id,
            tool_call_id,
            None,
            true,
            Instant::now(),
        )
        .await;
    }

    /// `register_pending_tool_call` that also records the
    /// `(agent_type, task, working_dir)` correlation key parsed from the
    /// tool_call's `raw_input`. The key lets
    /// the broker bind this id to its matching MCP round-trip deterministically
    /// for parallel `delegate_to_agent` calls that pure arrival-order FIFO can
    /// mis-assign. Production registration (from the ACP lifecycle dispatcher)
    /// goes through here.
    pub async fn register_pending_tool_call_with_key(
        &self,
        parent_connection_id: &str,
        tool_call_id: String,
        match_key: Option<DelegationMatchKey>,
    ) {
        self.register_pending_tool_call_with_key_at(
            parent_connection_id,
            tool_call_id,
            match_key,
            false,
            Instant::now(),
        )
        .await;
    }

    /// Core registration. Holds the [`ToolCallTracker`] mutex across both
    /// dedupe tiers AND the push so no concurrent `take` can split the
    /// "queue empty + not yet recorded as consumed" window where a host
    /// re-emit could otherwise inject a stale duplicate.
    ///
    /// Two-tier dedupe against host re-emits of `sessionUpdate(tool_call)`
    /// (some hosts use the non-update variant to ship status flips and
    /// late-arriving `raw_input` chunks):
    ///
    /// 1. **Recently consumed**: if the id was already claimed for an
    ///    earlier delegation on the same parent, drop the re-emit —
    ///    otherwise it would sit in the queue as a stale id and mis-bind
    ///    the **next** delegation's MCP round-trip. The consumed memory
    ///    persists for the parent connection's lifetime (no TTL, no cap)
    ///    so a host re-emit at terminal status flip is still rejected
    ///    even if the delegation ran for hours.
    /// 2. **In-queue**: if the id is still waiting to be claimed, drop the
    ///    re-emit rather than push a duplicate — EXCEPT we backfill the
    ///    `match_key` onto an entry registered without one. This is the common
    ///    case for hosts that emit an arg-less initial `ToolCall` and ship the
    ///    `agent_type`/`task` arguments on a following `ToolCallUpdate`: the
    ///    lifecycle dispatcher registers BOTH variants (see
    ///    `register_delegation_tool_call_from_event`), so the first call lands
    ///    here unkeyed and the later update re-enters and back-fills the key.
    ///    Keying the entry this way is what lets it survive past the unkeyed
    ///    GC TTL (see `take_matching_tool_call_at`'s retain block).
    async fn register_pending_tool_call_with_key_at(
        &self,
        parent_connection_id: &str,
        tool_call_id: String,
        match_key: Option<DelegationMatchKey>,
        identityless: bool,
        now: Instant,
    ) {
        let mut map = self.tool_calls.inner.lock().await;
        let bucket = map.entry(parent_connection_id.to_string()).or_default();
        if identityless {
            bucket.identityless_seen = true;
        }
        // Tier 1: recently consumed. No TTL — the consumed memory must
        // outlast the entire parent-side tool call lifetime (minutes
        // to hours) so a host re-emit at terminal status flip is
        // still rejected. See `ToolCallTrackerBucket` docs.
        if bucket.consumed.iter().any(|(id, _)| id == &tool_call_id) {
            tracing::info!(
                "[delegation] dropping ACP tool_call_id={tool_call_id} on conn={parent_connection_id} (already consumed by an earlier delegation)"
            );
            return;
        }
        // Tier 2: in-queue. A re-emit of an already-queued id: adopt the
        // LATEST parseable key rather than only back-filling a missing one.
        // Hosts stream `raw_input` incrementally and the MCP side keys on the
        // FINAL arguments, so a later `ToolCallUpdate` that completes the key
        // (e.g. adds an explicit `working_dir` the first parse lacked) must
        // REPLACE the earlier `(agent, task, None)` key — otherwise the MCP
        // claim keys on `(agent, task, Some(dir))`, fails to match the stale
        // `None`, refuses the keyed fallback, and orphans to a synthetic id
        // (the very dead-card failure this whole change fixes). An arg-less or
        // identical re-emit changes nothing and is dropped as a duplicate.
        if let Some(existing) = bucket
            .pending
            .iter_mut()
            .find(|p| p.tool_call_id == tool_call_id)
        {
            match match_key {
                Some(key) if existing.match_key.as_ref() != Some(&key) => {
                    existing.match_key = Some(key);
                }
                _ => {
                    tracing::info!(
                        "[delegation] dropping duplicate ACP tool_call_id={tool_call_id} on conn={parent_connection_id}"
                    );
                }
            }
            return;
        }
        bucket.pending.push_back(PendingToolCall {
            tool_call_id,
            match_key,
            identityless,
            registered_at: now,
        });
    }

    /// Pop the oldest pending `tool_call_id` for the given parent, if any.
    /// Skips entries older than [`PENDING_TOOL_CALL_TTL`] so an ACP id whose
    /// matching MCP round-trip never arrived cannot mis-bind a later
    /// delegation. Mutates the queue in-place; the bucket is removed once
    /// drained.
    pub async fn take_pending_tool_call(&self, parent_connection_id: &str) -> Option<String> {
        self.take_pending_tool_call_at(parent_connection_id, Instant::now())
            .await
            .map(|(id, _)| id)
    }

    /// `take_pending_tool_call` with an injected "as of" instant. The
    /// public entry point pins it to `Instant::now()`; tests can supply
    /// a future instant to exercise TTL eviction without sleeping past
    /// [`PENDING_TOOL_CALL_TTL`].
    ///
    /// Anonymous claim: returns the oldest *unkeyed* pending id (plus its
    /// `identityless` mark, so the delegate-claim path knows to restore the
    /// call's identity), GC'ing stale unkeyed entries along the way. KEYED
    /// entries are stepped over and left in place — they're reserved for
    /// their exact-key-match round-trip and must never be handed out by this
    /// arrival-order path (doing so would steal an in-flight delegation's
    /// id). Returns `None` when no unkeyed entry is claimable, even if keyed
    /// entries remain.
    async fn take_pending_tool_call_at(
        &self,
        parent_connection_id: &str,
        now: Instant,
    ) -> Option<(String, bool)> {
        self.take_unkeyed_tool_call_at(parent_connection_id, now, false)
            .await
    }

    /// Claim the oldest pending entry that was registered as IDENTITY-LESS
    /// (Cursor's `"MCP: tool"` announcements) — used by the status/cancel
    /// call-time rename. Unlike `take_pending_tool_call_at` this NEVER hands
    /// out a plain unkeyed entry: that one is a delegation invocation whose
    /// args merely didn't parse, and renaming it to a status tool would
    /// mislabel a real delegate card.
    async fn take_identityless_tool_call(&self, parent_connection_id: &str) -> Option<String> {
        self.take_unkeyed_tool_call_at(parent_connection_id, Instant::now(), true)
            .await
            .map(|(id, _)| id)
    }

    /// Shared FIFO claim over unkeyed entries. `identityless_only` restricts
    /// eligibility to entries carrying the identity-less mark; keyed entries
    /// are always stepped over (reserved for their exact-key-match
    /// round-trip), and stale unkeyed entries are GC'd along the way
    /// regardless of the filter so the queue stays bounded from either
    /// caller.
    async fn take_unkeyed_tool_call_at(
        &self,
        parent_connection_id: &str,
        now: Instant,
        identityless_only: bool,
    ) -> Option<(String, bool)> {
        let mut map = self.tool_calls.inner.lock().await;
        let bucket = map.get_mut(parent_connection_id)?;
        // Anonymous claim (post-budget last resort + legacy single-delegation
        // path): only UNKEYED entries are eligible. A keyed entry identifies a
        // specific in-flight delegation and is claimable ONLY by its
        // exact-key-match round-trip; grabbing it here would steal that
        // delegation's id and make IT the dead card. Walk oldest→newest,
        // GC'ing stale unkeyed entries and stepping over keyed ones, until we
        // find the oldest fresh eligible id. When only keyed siblings remain we
        // return `None` — the delegate caller then mints a synthetic id rather
        // than mis-binding a sibling.
        let mut claimed: Option<(String, bool)> = None;
        let mut idx = 0;
        while idx < bucket.pending.len() {
            if bucket.pending[idx].match_key.is_some() {
                idx += 1; // keyed: leave it for its exact-match round-trip
                continue;
            }
            if now.duration_since(bucket.pending[idx].registered_at) > PENDING_TOOL_CALL_TTL {
                if let Some(stale) = bucket.pending.remove(idx) {
                    let age_secs = now.duration_since(stale.registered_at).as_secs();
                    tracing::info!(
                        "[delegation] evicting stale UNKEYED ACP tool_call_id={} (age={age_secs}s) on conn={parent_connection_id}",
                        stale.tool_call_id
                    );
                }
                // `remove` shifted later entries left into `idx`; re-check it.
                continue;
            }
            if identityless_only && !bucket.pending[idx].identityless {
                idx += 1; // plain unkeyed: reserved for the delegate FIFO path
                continue;
            }
            claimed = bucket
                .pending
                .remove(idx)
                .map(|p| (p.tool_call_id, p.identityless));
            break;
        }
        // Same mutex span: record the claim into the consumed memory so
        // a concurrent re-register cannot observe "pending empty AND
        // consumed missing" and inject a stale duplicate. Consumed
        // entries persist for the whole parent connection lifetime
        // (no TTL, no cap — see `ToolCallTrackerBucket`) and are only
        // released when the parent disconnects.
        if let Some((id, _)) = &claimed {
            bucket.consumed.push_back((id.clone(), now));
        }
        if bucket.pending.is_empty() && bucket.consumed.is_empty() && !bucket.identityless_seen {
            map.remove(parent_connection_id);
        }
        claimed
    }

    /// Whether this parent has EVER registered an identity-less candidate —
    /// the zero-cost gate for [`Self::rewrite_identityless_tool_call`]'s poll.
    async fn has_seen_identityless_tool_calls(&self, parent_connection_id: &str) -> bool {
        self.tool_calls
            .inner
            .lock()
            .await
            .get(parent_connection_id)
            .is_some_and(|b| b.identityless_seen)
    }

    /// Restore the identity of a pending identity-less MCP tool call at
    /// companion round-trip time: claim the oldest such candidate for this
    /// parent and rewrite its live `title` + `raw_input` to the actual
    /// codeg-mcp tool (`get_delegation_status` / `cancel_delegation`) with
    /// its real arguments — flipping the card from the generic "MCP: tool"
    /// WHILE the call runs, instead of only at the completion-time result
    /// sniff (which, for a `wait_ms`-blocked status long-poll, can be minutes
    /// away).
    ///
    /// Best-effort by design: on hosts that ship real identities the sticky
    /// `identityless_seen` gate skips everything at zero cost; on an
    /// identity-less host whose announcement hasn't registered yet, a short
    /// bounded poll (well under the ACP-vs-companion race in practice)
    /// absorbs the ordering, and a miss simply leaves the rename to the
    /// completion sniff. Mis-pairing under PARALLEL identity-less calls in
    /// one burst is the same accepted arrival-order residual as the delegate
    /// FIFO fallback — Cursor executes tool calls sequentially, so announce
    /// order matches round-trip order in practice.
    pub async fn rewrite_identityless_tool_call(
        &self,
        parent_connection_id: &str,
        title: &str,
        raw_input: serde_json::Value,
    ) -> Option<String> {
        if !self
            .has_seen_identityless_tool_calls(parent_connection_id)
            .await
        {
            return None;
        }
        for attempt in 0..IDENTITYLESS_RENAME_POLL_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(CLAIM_POLL_INTERVAL).await;
            }
            if let Some(id) = self.take_identityless_tool_call(parent_connection_id).await {
                tracing::info!(
                    "[delegation] renaming identity-less tool_call_id={id} on conn={parent_connection_id} to {title}"
                );
                self.meta_writer
                    .write_tool_call_identity(parent_connection_id, &id, title, raw_input)
                    .await;
                return Some(id);
            }
        }
        None
    }

    /// Claim the pending `tool_call_id` for `parent_connection_id` whose
    /// recorded key matches `key` (exact `(agent_type, task, working_dir)`
    /// match). This is the ONLY claim this method makes — it never hands out an
    /// unkeyed entry, because an unkeyed entry may belong to a *different*
    /// parallel delegation whose round-trip simply hasn't registered (or keyed)
    /// its `tool_call` yet, and claiming it by arrival order would steal that
    /// sibling's id. Returns `None` (so the caller keeps polling) whenever no
    /// entry's key matches — whether keyed siblings or only unkeyed entries are
    /// present.
    ///
    /// Arrival-order FIFO for genuinely keyless hosts is deferred to the
    /// post-budget last resort `take_pending_tool_call`, which runs only after
    /// the caller has waited its full budget (see
    /// `claim_pending_tool_call_with_brief_wait`) — the correct clock for "no
    /// key is coming", since a host can serialize a round-trip arbitrarily far
    /// behind its `tool_call` registration, so the entry's own age can never
    /// prove a key won't still arrive. Evicts stale *unkeyed* entries along the
    /// way; keyed entries are retained regardless of age (their round-trip may
    /// be serialized far behind earlier delegations — see the retain block) and
    /// an exact key match claims them at any age.
    pub async fn take_matching_tool_call(
        &self,
        parent_connection_id: &str,
        key: &DelegationMatchKey,
    ) -> Option<String> {
        self.take_matching_tool_call_at(parent_connection_id, key, Instant::now())
            .await
    }

    /// `take_matching_tool_call` with an injected "as of"
    /// instant for TTL tests.
    async fn take_matching_tool_call_at(
        &self,
        parent_connection_id: &str,
        key: &DelegationMatchKey,
        now: Instant,
    ) -> Option<String> {
        let mut map = self.tool_calls.inner.lock().await;
        let bucket = map.get_mut(parent_connection_id)?;

        // Evict every stale UNKEYED entry up front. The key-match scan below
        // ignores unkeyed entries anyway (they carry no key to match), but
        // GC'ing here keeps the queue bounded during the poll loop and
        // consistent with `take_pending_tool_call_at`'s view, so the
        // post-budget last resort never hands out an aged-out id. Mirrors that
        // TTL skip but covers entries at any position (not just the front).
        bucket.pending.retain(|p| {
            // Keyed entries are NEVER aged out. Each identifies one specific
            // `delegate_to_agent` invocation and is claimable ONLY by an exact
            // key match (never by FIFO — see below), so it cannot mis-bind a
            // different delegation no matter how old it gets. And it MUST
            // survive until its MCP round-trip arrives, which the host may
            // serialize arbitrarily far behind earlier long-running
            // delegations: Claude Code runs parallel `delegate_to_agent` calls
            // SEQUENTIALLY, so the 2nd call's round-trip only fires after the
            // 1st child finishes. Observed in the wild — a 2nd delegation whose
            // tool_call registered, then waited 77s (past the old 60s TTL) for
            // its round-trip while the 1st ran; age-evicting it here orphaned
            // it to a synthetic id and left the parent card stuck on
            // "sub-agent running…". Only UNKEYED (anonymous, arrival-order
            // correlated) entries keep the age-based GC, since a stale one
            // could be mis-claimed via the FIFO path. Keyed memory stays bounded
            // by exact-match claim, terminal tombstoning, and
            // `drop_pending_tool_calls_for_parent` on connection teardown — not
            // by this TTL.
            if p.match_key.is_some() {
                return true;
            }
            let fresh = now.duration_since(p.registered_at) <= PENDING_TOOL_CALL_TTL;
            if !fresh {
                let age_secs = now.duration_since(p.registered_at).as_secs();
                tracing::info!(
                    "[delegation] evicting stale UNKEYED ACP tool_call_id={} (age={age_secs}s) on conn={parent_connection_id}",
                    p.tool_call_id
                );
            }
            fresh
        });

        let claimed = if let Some(pos) = bucket
            .pending
            .iter()
            .position(|p| p.match_key.as_ref() == Some(key))
        {
            // Exact (agent_type, task) match: deterministic correlation
            // regardless of ACP-vs-MCP arrival order or how many delegations
            // are in flight.
            bucket.pending.remove(pos).map(|p| p.tool_call_id)
        } else {
            // No exact key match. We deliberately do NOT claim an unkeyed entry
            // here — not even the oldest, not even the only one. An unkeyed
            // pending entry may belong to a DIFFERENT parallel delegation whose
            // own round-trip hasn't yet registered (or keyed) its `tool_call`,
            // and claiming it by arrival order would steal that sibling's id —
            // the mis-bind this machinery exists to prevent.
            //
            // Crucially, the ENTRY's age is the wrong clock for "no key is
            // coming": a host can serialize a round-trip arbitrarily far behind
            // its `tool_call` registration (see the retain block / the
            // `keyed_entry_survives_past_ttl` case), so even an old lone unkeyed
            // entry can still be a sibling's. The CALLER's own wait is the right
            // clock. So return `None` and let
            // `claim_pending_tool_call_with_brief_wait` poll: if this
            // delegation's key lands (initial register or a later backfill) we
            // bind by the exact match above; only after the caller has spent the
            // FULL budget does its post-budget last resort
            // (`take_pending_tool_call`) claim the oldest unkeyed id in arrival
            // order — the best a genuinely keyless host allows, and the point at
            // which waiting longer cannot improve correlation.
            None
        };

        if let Some(id) = &claimed {
            bucket.consumed.push_back((id.clone(), now));
        }
        if bucket.pending.is_empty() && bucket.consumed.is_empty() {
            map.remove(parent_connection_id);
        }
        claimed
    }

    /// Consume an explicit `parent_tool_use_id` that the MCP client supplied
    /// directly via `_meta.tool_use_id` (the precise-binding path; most clients
    /// omit it). In that case `handle_request` does NOT run the claim path, so
    /// the matching pending entry the lifecycle dispatcher registered off the
    /// parent's ACP stream would otherwise never be consumed — and because
    /// keyed entries are now retained indefinitely, it would linger and could
    /// be mis-claimed by a *later* delegation sharing the same
    /// `(agent_type, task, working_dir)` key, retargeting that delegation's
    /// writes/events at the wrong (already-handled) card.
    ///
    /// Remove the entry from the pending queue AND record the id as consumed.
    /// Recording consumed also covers the MCP-before-ACP race: a later ACP
    /// registration for the same id is dropped by the Tier-1 consumed check in
    /// `register_pending_tool_call_with_key_at`, so the entry can't reappear
    /// regardless of arrival order.
    async fn consume_explicit_tool_call(&self, parent_connection_id: &str, tool_call_id: &str) {
        let mut map = self.tool_calls.inner.lock().await;
        let bucket = map.entry(parent_connection_id.to_string()).or_default();
        bucket.pending.retain(|p| p.tool_call_id != tool_call_id);
        if !bucket.consumed.iter().any(|(id, _)| id == tool_call_id) {
            bucket
                .consumed
                .push_back((tool_call_id.to_string(), Instant::now()));
        }
    }

    /// Tombstone a parent `tool_call_id` whose `delegate_to_agent` reached a
    /// TERMINAL ACP status (`completed`/`failed`) so a stale keyed pending entry
    /// can't mis-bind a later delegation. The lifecycle dispatcher calls this
    /// from its terminal-`ToolCallUpdate` branch, keyed on `tool_call_id`
    /// (a bare terminal update carries no parseable key).
    ///
    /// The hazard: keyed pending entries are retained regardless of age (see the
    /// retain block in `take_matching_tool_call_at`), so if a `delegate_to_agent`
    /// tool call goes terminal without its MCP round-trip ever reaching the
    /// broker (the call failed, the turn was interrupted, the companion never
    /// dispatched), its entry would linger forever and a LATER delegation sharing
    /// the same `(agent_type, task, working_dir)` key would claim this dead id,
    /// retargeting its writes/events at the wrong card. Same hazard
    /// `consume_explicit_tool_call` guards on the explicit-id path; this is its
    /// terminal-status sibling.
    ///
    /// Safe synchronously, no grace window: a terminal `completed` can only
    /// arrive AFTER the round-trip's claim already removed the entry (the ack
    /// that claim produces is what lets the parent's tool call return), and a
    /// serialized sibling still awaiting its (observed 77s-late) round-trip is
    /// NON-terminal while it waits — so this never evicts a live entry.
    ///
    /// Records `consumed` ONLY when an entry was actually removed: this runs for
    /// EVERY terminal tool-call update (the vast majority are non-delegations),
    /// and `consumed` has no TTL/cap, so recording unconditionally would grow it
    /// with every completed tool call. Recording on a real removal still drops an
    /// out-of-order re-registration of the same id via the Tier-1 consumed check
    /// in `register_pending_tool_call_with_key_at`. Returns whether an entry was
    /// removed (for the dispatcher's gated log).
    pub async fn tombstone_pending_tool_call(
        &self,
        parent_connection_id: &str,
        tool_call_id: &str,
    ) -> bool {
        let mut map = self.tool_calls.inner.lock().await;
        // No bucket → nothing registered for this parent; nothing to tombstone
        // and nothing to record (unlike `consume_explicit_tool_call`, no
        // MCP-before-ACP race can land a terminal status before registration on
        // the single ordered ACP stream, so we never pre-create a bucket here).
        let Some(bucket) = map.get_mut(parent_connection_id) else {
            return false;
        };
        let before = bucket.pending.len();
        bucket.pending.retain(|p| p.tool_call_id != tool_call_id);
        let removed = bucket.pending.len() != before;
        if removed && !bucket.consumed.iter().any(|(id, _)| id == tool_call_id) {
            bucket
                .consumed
                .push_back((tool_call_id.to_string(), Instant::now()));
        }
        removed
    }

    /// Correlate an MCP `delegate_to_agent` round-trip to the parent's
    /// real ACP `tool_call_id`, polling briefly to absorb the race between
    /// two independent arrival paths for the same invocation:
    ///
    ///   * ACP `session/update(tool_call)` → in-process bus → lifecycle
    ///     dispatcher → `register_pending_tool_call_with_key`
    ///   * MCP `tools/call` → stdio round-trip → companion → `handle_request`
    ///
    /// Correlation is by the `(agent_type, task, working_dir)` key (carried in
    /// both the ACP `raw_input` and the MCP call), so several `delegate_to_agent`
    /// calls firing in parallel each bind to their own `tool_call_id`
    /// regardless of arrival order — pure FIFO mis-assigned them (swapping
    /// the child shown under each card) or, when one MCP round-trip out-raced
    /// its ACP event, orphaned the loser to a synthetic `delegation-<uuid>`
    /// (the parent UI then never paints "view session" and the card hangs on
    /// "sub-agent running…", because the frontend keys its binding map by
    /// the agent's real `tool_call_id`).
    ///
    /// As a last resort after the budget — and the ONLY place arrival-order
    /// FIFO is applied — claim the oldest unkeyed id, so a sibling whose
    /// registration was unusually delayed, or a genuinely keyless host, still
    /// yields a *real* id rather than a synthetic one. Deferring FIFO until the
    /// full budget has elapsed is what makes it safe: in-loop we bind ONLY by
    /// exact key match, so a round-trip can't FIFO-steal a sibling's
    /// not-yet-keyed id while that sibling's own registration is still in
    /// flight (the entry's age is no proof a key won't still arrive). A
    /// synthetic id only results when no unkeyed id is claimable for the whole
    /// budget — only keyed siblings remain, or the queue stays genuinely empty.
    /// Returns the claimed id plus its `identityless` mark — `true` only for
    /// the post-budget FIFO claim of an identity-less announcement (Cursor),
    /// which tells `start_delegation` to restore the call's identity on the
    /// live tool call. Exact-key claims are by definition NOT identity-less
    /// (the key was parsed from the wire's own `raw_input`).
    async fn claim_pending_tool_call_with_brief_wait(
        &self,
        parent_connection_id: &str,
        key: &DelegationMatchKey,
    ) -> Option<(String, bool)> {
        if let Some(id) = self
            .take_matching_tool_call(parent_connection_id, key)
            .await
        {
            return Some((id, false));
        }
        for _ in 0..CLAIM_POLL_ATTEMPTS {
            tokio::time::sleep(CLAIM_POLL_INTERVAL).await;
            if let Some(id) = self
                .take_matching_tool_call(parent_connection_id, key)
                .await
            {
                return Some((id, false));
            }
        }
        // Budget exhausted with no key match. As a last resort claim the
        // oldest UNKEYED pending id (a host that shipped no parseable
        // `raw_input`, or a mixed-shape race) — a real id beats a synthetic
        // placeholder that orphans the parent UI binding. Crucially this
        // never claims a KEYED entry: those belong to specific in-flight
        // delegations and are reserved for their own exact-key-match
        // round-trip, so when only keyed siblings remain the caller falls
        // through to a synthetic id rather than stealing a sibling's binding
        // (which would just move the dead card from one delegation to another).
        self.take_pending_tool_call_at(parent_connection_id, Instant::now())
            .await
    }

    /// Remove `handle` from the pre-cancel set, returning whether it was
    /// present. Used by `handle_request` at two checkpoints (entry + just
    /// after pending registration) so a cancel that lost the race with the
    /// MCP round-trip still wins. The set is single-shot per handle —
    /// taking it here means a subsequent `cancel_by_external_handle` will
    /// have to find the pending entry on its own.
    async fn take_pre_canceled_handle(&self, handle: &str) -> bool {
        let mut state = self.pre_canceled_handles.inner.lock().await;
        if state.set.remove(handle) {
            // Best-effort companion-side cleanup of `order` so a later
            // FIFO eviction doesn't burn a slot. Linear scan is fine —
            // PRE_CANCELED_CAP is small.
            if let Some(pos) = state.order.iter().position(|h| h == handle) {
                state.order.remove(pos);
            }
            true
        } else {
            false
        }
    }

    /// Insert `handle` into the pre-cancel set with FIFO eviction at
    /// [`PRE_CANCELED_CAP`]. Idempotent — re-inserting an existing handle
    /// is a no-op.
    async fn buffer_pre_canceled_handle(&self, handle: String) {
        let mut state = self.pre_canceled_handles.inner.lock().await;
        if !state.set.insert(handle.clone()) {
            return;
        }
        state.order.push_back(handle);
        while state.order.len() > PRE_CANCELED_CAP {
            if let Some(evicted) = state.order.pop_front() {
                state.set.remove(&evicted);
            }
        }
    }

    /// Forget every pending and recently-consumed tool_call id for the
    /// given parent. Called when the parent connection tears down so
    /// stale ids don't bind to a future reuse of the same connection_id
    /// (UUIDs make that unlikely but cheap to defend against), and so a
    /// fresh connection on the reused id is not blocked by the
    /// consumed memory of the previous one.
    pub async fn drop_pending_tool_calls_for_parent(&self, parent_connection_id: &str) {
        self.drop_tool_calls_for_parent(parent_connection_id, false)
            .await;
    }

    /// Core of the tool_call-tracker drop, shared by the two cancel scopes.
    ///
    /// * `keep_consumed == false` — genuine connection teardown: remove the
    ///   whole bucket (`pending` + `consumed`). The connection is going away,
    ///   so nothing it remembered can mis-bind a future delegation, and a
    ///   reused connection_id must start clean.
    /// * `keep_consumed == true` — turn/prompt cancel with the parent
    ///   connection STILL ALIVE: TOMBSTONE the cancelled turn's unclaimed
    ///   `pending` ids into `consumed` and RETAIN the existing `consumed`. Both
    ///   the already-claimed ids AND the just-cancelled turn's unclaimed ids
    ///   must keep rejecting a host re-emit (e.g. a terminal status-flip): the
    ///   Tier-1 consumed check in `register_pending_tool_call_with_key_at` drops
    ///   the re-emit, so a stale id can't re-register as fresh `pending` and
    ///   mis-bind the next same-key delegation on this live connection. Merely
    ///   CLEARING the unclaimed ids would leave them re-registerable, reopening
    ///   that hole for the unclaimed half (the claimed half was already safe via
    ///   `consumed`). Retention is connection-scoped and released on teardown —
    ///   the same unbounded-but-bounded-by-delegation-count envelope `consumed`
    ///   already lives in for normal end_turn delegations (see
    ///   [`ToolCallTrackerBucket`]).
    ///
    /// Tombstoning ALL of `pending` here is safe (no turn/generation tag
    /// needed): `run_conversation_loop` drives at most ONE `session/prompt`
    /// future per connection at a time (see `acp/connection.rs`), and a
    /// parent-side `tool_call` only streams while its prompt future is in
    /// flight, so every `pending` id belongs to the single active turn — the one
    /// being cancelled — or is a stale leftover from an earlier turn that should
    /// be tombstoned regardless. (The per-connection `prompt_lock` only
    /// serializes the prompt-SEND handshake, not the turn, so it is NOT the
    /// source of this invariant.) The cancelled turn's serialized MCP round-trip
    /// won't arrive after cancel, so nothing legitimate is lost.
    async fn drop_tool_calls_for_parent(&self, parent_connection_id: &str, keep_consumed: bool) {
        let mut map = self.tool_calls.inner.lock().await;
        if !keep_consumed {
            map.remove(parent_connection_id);
            return;
        }
        if let Some(bucket) = map.get_mut(parent_connection_id) {
            // Tombstone the cancelled turn's unclaimed pending ids into
            // `consumed` rather than just dropping them, so a later host re-emit
            // of one is rejected by the Tier-1 consumed check instead of
            // re-registering as a claimable stale entry. `drain` empties
            // `pending` first so the subsequent `consumed` borrow is disjoint.
            let now = Instant::now();
            let cleared: Vec<String> = bucket.pending.drain(..).map(|p| p.tool_call_id).collect();
            for id in cleared {
                if !bucket.consumed.iter().any(|(c, _)| c == &id) {
                    bucket.consumed.push_back((id, now));
                }
            }
            // Drop the now-empty bucket only when nothing consumed remains —
            // otherwise keep it so the retained `consumed` ids keep rejecting
            // re-emits for the rest of this connection's lifetime.
            if bucket.consumed.is_empty() {
                map.remove(parent_connection_id);
            }
        }
    }

    pub async fn set_config(&self, cfg: DelegationConfig) {
        let cap_bytes = cfg.completed_cache_cap_bytes;
        *self.config.lock().await = cfg;
        // Seed the byte cap into the pending-calls bucket so `insert_completed`
        // reads it lock-free (it already holds the pending lock). Acquired AFTER
        // the config guard above is dropped — sequential, never nested — so no
        // path locks `config` under `pending` or vice-versa (deadlock-free).
        // Then prune existing per-parent caches: a LOWERED cap must free memory
        // now, not lazily on each parent's next completion (which may never
        // arrive for an idle parent).
        let mut inner = self.pending.inner.lock().await;
        inner.completed_cap_bytes = cap_bytes;
        inner.enforce_completed_cap_all_parents();
    }

    pub async fn config_snapshot(&self) -> DelegationConfig {
        self.config.lock().await.clone()
    }

    /// If this in-flight setup has been flagged canceled by a parent cancel,
    /// deregister it and return true. One lock acquisition; used at the
    /// pre-spawn / post-spawn checkpoints in `handle_request`.
    async fn take_inflight_cancel(&self, inflight_id: u64) -> bool {
        let mut inner = self.pending.inner.lock().await;
        if inner.inflight_canceled(inflight_id) {
            inner.deregister_inflight(inflight_id);
            true
        } else {
            false
        }
    }

    /// Drop this setup's in-flight record. Called on each `handle_request`
    /// early-return that isn't a park hand-off (the park region deregisters
    /// inline, atomically with `calls.insert`).
    async fn drop_inflight(&self, inflight_id: u64) {
        self.pending
            .inner
            .lock()
            .await
            .deregister_inflight(inflight_id);
    }

    /// Async entry point for `delegate_to_agent`. Does the bounded setup
    /// (claim/depth checks → spawn → send first prompt), registers the task in
    /// `running`, and returns a `Running` ack [`DelegationTaskReport`] WITHOUT
    /// waiting for the child to finish. The child resolves later via the
    /// lifecycle → [`complete_call`] (or a cancel path), which migrates the task
    /// into `completed` and wakes any `get_delegation_status` long-poll.
    ///
    /// Returns a terminal report instead of a `Running` ack in three cases: the
    /// child finished during setup (fast/empty turn), a parent cancel reached it
    /// mid-setup, or setup itself failed (disabled / depth / spawn / send).
    ///
    /// All the setup-window race machinery (`setups` / `early_*` / `inflight`)
    /// is unchanged — it governs terminals that beat registration, which is
    /// orthogonal to whether the caller then blocks. The only change vs. the old
    /// `handle_request` is that "park a `oneshot` and await it" becomes "insert a
    /// [`RunningTask`] and return the ack."
    #[tracing::instrument(
        name = "delegation_task",
        skip_all,
        fields(
            parent_connection_id = %req.parent_connection_id,
            parent_tool_use_id = %req.parent_tool_use_id,
            agent_type = ?req.agent_type,
            working_dir = ?req.working_dir,
            child_connection_id = tracing::field::Empty,
            task_id = tracing::field::Empty,
        )
    )]
    pub async fn start_delegation(&self, mut req: DelegationRequest) -> DelegationTaskReport {
        // Register this setup as the VERY FIRST thing — before the pre-cancel
        // check's `.await` and the (possibly multi-second) claim poll — so a
        // parent cancel landing ANYWHERE from here to park reaches it, not just
        // after park (which is all the `cancel_by_parent*` parked-call drain
        // covers on its own). The only residual gap is a cancel firing before
        // the broker is even invoked for this request, which no
        // in-`handle_request` mechanism can observe. Deregistered on every exit
        // path below: each early-return via `drop_inflight` /
        // `take_inflight_cancel`, or inline at park (atomically with
        // `calls.insert`).
        let inflight_id = self
            .pending
            .inner
            .lock()
            .await
            .register_inflight(&req.parent_connection_id);
        // Pre-cancel short-circuit. If the MCP companion already received
        // `notifications/cancelled` for this `tools/call` before we even
        // started processing (cancel ran ahead of the UDS round-trip), we
        // claim the handle from the pre-cancel set and bail without
        // spawning anything — the caller will not be receiving our
        // response either way (the companion suppresses it per MCP spec).
        if let Some(handle) = req.external_handle.as_deref() {
            if self.take_pre_canceled_handle(handle).await {
                self.drop_inflight(inflight_id).await;
                // Bailing here BEFORE the claim path means this delegation never
                // consumes the ACP `tool_call_id` the lifecycle keyed for it. As
                // keyed entries are retained indefinitely, a leftover would let a
                // *later* same-`(agent_type, task, working_dir)` delegation claim
                // this canceled call's id and bind its writes/events to the wrong
                // card. Drain it now (idempotent; the turn-end tombstone is the
                // backstop if the ACP event hasn't registered yet).
                if req.parent_tool_use_id.is_empty() {
                    let key = DelegationMatchKey {
                        agent_type: req.agent_type,
                        task: req.task.clone(),
                        working_dir: req.requested_working_dir.clone(),
                    };
                    let _ = self
                        .take_matching_tool_call(&req.parent_connection_id, &key)
                        .await;
                } else {
                    self.consume_explicit_tool_call(
                        &req.parent_connection_id,
                        &req.parent_tool_use_id,
                    )
                    .await;
                }
                return report_err(
                    req.agent_type,
                    DelegationError::Canceled {
                        reason: "canceled before spawn".into(),
                    },
                    None,
                );
            }
        }
        // MCP clients usually don't populate `_meta.tool_use_id`, so the
        // listener will pass through an empty string. Claim the matching
        // ACP-side `tool_call_id` for this parent by task text — with a brief
        // poll loop so an MCP round-trip that out-races the in-process ACP
        // `session/update` doesn't fall back to a synthetic id (which breaks
        // the parent UI's `parent_tool_use_id` binding). Falls back to a UUID
        // placeholder only when no id arrives within the wait budget.
        if req.parent_tool_use_id.is_empty() {
            let match_key = DelegationMatchKey {
                agent_type: req.agent_type,
                task: req.task.clone(),
                working_dir: req.requested_working_dir.clone(),
            };
            let claimed = self
                .claim_pending_tool_call_with_brief_wait(&req.parent_connection_id, &match_key)
                .await;
            let claimed_identityless = matches!(claimed, Some((_, true)));
            req.parent_tool_use_id = claimed.map(|(id, _)| id).unwrap_or_else(|| {
                tracing::warn!(
                    "[delegation] synthetic fallback for parent_tool_use_id on conn={} (no ACP tool_call_id arrived within claim budget)",
                    req.parent_connection_id
                );
                format!("delegation-{}", uuid::Uuid::new_v4())
            });
            // The claimed announcement never carried an identity (Cursor's
            // "MCP: tool" + "{}" — the wire will not re-send title/arguments):
            // restore it NOW, before the multi-second spawn, so the live card
            // shows the canonical delegate tool with its real arguments while
            // the child is still starting. Hosts whose wire carried the
            // identity (keyed or explicit-id claims) are left untouched — a
            // reconstructed `raw_input` would clobber host-specific fields.
            if claimed_identityless {
                let mut raw_input = serde_json::Map::new();
                raw_input.insert("task".into(), serde_json::Value::String(req.task.clone()));
                if let Ok(at) = serde_json::to_value(req.agent_type) {
                    raw_input.insert("agent_type".into(), at);
                }
                if let Some(dir) = req.requested_working_dir.as_deref() {
                    raw_input.insert("working_dir".into(), serde_json::Value::String(dir.into()));
                }
                self.meta_writer
                    .write_tool_call_identity(
                        &req.parent_connection_id,
                        &req.parent_tool_use_id,
                        crate::acp::delegation::DELEGATE_TOOL_REWRITE_TITLE,
                        serde_json::Value::Object(raw_input),
                    )
                    .await;
            }
        } else {
            // The client gave us the real ACP tool_call_id directly
            // (`_meta.tool_use_id`), so we skip the claim path — but the
            // lifecycle dispatcher may already have registered that same id as
            // a (now indefinitely-retained) keyed pending entry. Consume it so
            // it can't linger and be mis-claimed by a later same-key
            // delegation. Idempotent and order-independent (see the method).
            self.consume_explicit_tool_call(&req.parent_connection_id, &req.parent_tool_use_id)
                .await;
        }
        let cfg = self.config_snapshot().await;
        if !cfg.enabled {
            self.drop_inflight(inflight_id).await;
            return report_err(
                req.agent_type,
                DelegationError::Canceled {
                    reason: "delegation disabled".into(),
                },
                None,
            );
        }

        // --- Depth pre-check ----------------------------------------------------
        // We walk up to `limit + 1` so we know whether the *new* child would
        // sit at >= limit. Cycles/dead chains saturate at the cap.
        let lookup = self.depth_lookup.clone();
        let parent_depth = match crate::acp::delegation::depth::compute_depth(
            req.parent_conversation_id,
            |id| {
                let lookup = lookup.clone();
                async move { lookup.parent_of(id).await }
            },
            cfg.depth_limit + 1,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                self.drop_inflight(inflight_id).await;
                return report_err(req.agent_type, e, None);
            }
        };
        // The child the broker is about to create would sit at `parent_depth + 1`.
        // Reject only when the *child* depth would strictly exceed the limit;
        // a child sitting exactly at `depth_limit` is allowed.
        if parent_depth + 1 > cfg.depth_limit {
            self.drop_inflight(inflight_id).await;
            return report_err(
                req.agent_type,
                DelegationError::DepthLimitExceeded {
                    current_depth: parent_depth,
                    limit: cfg.depth_limit,
                },
                None,
            );
        }

        // --- Spawn child connection --------------------------------------------
        // Pull per-agent overrides from the broker config (defaults to empty).
        // Cloning is cheap — `AgentDelegationDefaults` is at most one Option<String>
        // and a small BTreeMap, and the spawner consumes both fields by value.
        let (preferred_mode_id, preferred_config_values) = cfg
            .agent_defaults
            .get(&req.agent_type)
            .map(|d: &AgentDelegationDefaults| (d.mode_id.clone(), d.config_values.clone()))
            .unwrap_or((None, BTreeMap::new()));
        // Checkpoint #1 (opportunistic): if a parent cancel already landed
        // during the claim/depth phase, bail before spawning a child the parent
        // has abandoned. No child exists yet, so there's nothing to tear down.
        if self.take_inflight_cancel(inflight_id).await {
            return report_err(
                req.agent_type,
                DelegationError::Canceled {
                    reason: "parent canceled".into(),
                },
                None,
            );
        }
        let child_connection_id = match self
            .spawner
            .spawn(
                &req.parent_connection_id,
                req.agent_type,
                req.working_dir.clone(),
                preferred_mode_id,
                preferred_config_values,
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                self.drop_inflight(inflight_id).await;
                return report_err(
                    req.agent_type,
                    DelegationError::SpawnFailed(e.to_string()),
                    None,
                );
            }
        };

        // Checkpoint #2: a parent cancel that landed during spawn() — the child
        // now exists but no prompt has been sent, so disconnect it (mirroring
        // the send-failure path's disconnect-only teardown) and bail. This is
        // the primary guard for the spawn window, which can block while the
        // agent process starts up.
        if self.take_inflight_cancel(inflight_id).await {
            let _ = self.spawner.disconnect(&child_connection_id).await;
            return report_err(
                req.agent_type,
                DelegationError::Canceled {
                    reason: "parent canceled".into(),
                },
                None,
            );
        }

        // --- Send linked prompt ------------------------------------------------
        let call_id = uuid::Uuid::new_v4().to_string();
        // Bounded task label used by the started event and every meta write —
        // the frontend card's fallback when the parent tool call's `raw_input`
        // never carried the arguments (Cursor's identity-less announcements).
        let task_preview = truncate_on_char_boundary(&req.task, TASK_PREVIEW_CAP);
        // Now that the child connection and task id exist, fill the span's empty
        // fields so every subsequent log line in this delegation carries the
        // parent→child linkage (see the `delegation_task` span on this fn).
        tracing::Span::current().record("child_connection_id", child_connection_id.as_str());
        tracing::Span::current().record("task_id", call_id.as_str());
        let link = DelegationLink {
            parent_conversation_id: req.parent_conversation_id,
            parent_tool_use_id: req.parent_tool_use_id.clone(),
            delegation_call_id: call_id.clone(),
        };

        // Reserve this delegation (both ids) BEFORE sending its first prompt.
        // `send_prompt_linked_for_delegation` persists the delegation link onto
        // the child row (arming the lifecycle resolver) AND dispatches the
        // prompt — after which a fast/empty turn's `TurnComplete` OR an
        // immediate child-connection failure can fire before we park the pending
        // entry below. The reservation lets those terminal events buffer their
        // outcome (see `PendingInner`) for the park to drain, rather than
        // no-oping and stranding `rx.await`. There is no `.await` between this
        // reservation and `send_prompt` (so nothing the child does can be
        // observed before the reservation is in place); it's cleared at park or
        // on the send-failure path. Reserving by `call_id` AND
        // `child_connection_id` lets each resolver gate on the id it holds —
        // `complete_call` the `call_id`, `cancel_by_child_connection` the
        // `child_connection_id`.
        self.pending
            .inner
            .lock()
            .await
            .reserve(&call_id, &child_connection_id);

        let child_conversation_id = match self
            .spawner
            .send_prompt_linked_for_delegation(&child_connection_id, req.task.clone(), link)
            .await
        {
            Ok(cid) => cid,
            Err(e) => {
                // Setup failed before parking — release the reservation (and
                // discard any terminal that buffered against this delegation in
                // the window) so nothing lingers or mis-binds a future id, and
                // drop the in-flight record in the same lock acquisition.
                {
                    let mut inner = self.pending.inner.lock().await;
                    inner.unreserve(&call_id, &child_connection_id);
                    inner.deregister_inflight(inflight_id);
                }
                let _ = self.spawner.disconnect(&child_connection_id).await;
                return report_err(
                    req.agent_type,
                    DelegationError::SpawnFailed(e.to_string()),
                    None,
                );
            }
        };

        // The child is now running. Stamp the start so terminal paths can
        // report a real `duration_ms`.
        let started_at = Instant::now();

        // --- Mark the parent's tool call as in-flight -------------------------
        // The frontend's DelegationContext seeds its `parent_tool_use_id`-keyed
        // binding map from this meta on snapshot replay, so a page refresh
        // mid-delegation can reconstruct the child connection / conversation
        // ids without depending on the live `delegation_started` event having
        // been received.
        self.write_meta_if_real(
            &req.parent_connection_id,
            &req.parent_tool_use_id,
            build_delegation_meta(
                "running",
                Some(&child_connection_id),
                Some(child_conversation_id),
                None,
                None,
                // No meaningful elapsed yet — the child just started.
                None,
                Some(&task_preview),
                Some(&call_id),
            ),
        )
        .await;

        // Announce the live delegation on the PARENT's event stream so the
        // frontend `DelegationContext` binds the child inline and attaches its
        // live sub-thread. Symmetric with the terminal `emit_completed_if_real`,
        // and — unlike the removed child-stream emit in `send_prompt_linked` —
        // delivered on a stream the parent is already attached to in web/server
        // mode, carrying the real `parent_connection_id`.
        self.emit_started_if_real(
            &req.parent_connection_id,
            &req.parent_tool_use_id,
            &child_connection_id,
            child_conversation_id,
            req.agent_type,
            &task_preview,
            &call_id,
        )
        .await;

        // --- Register pending, or resolve a terminal that beat us -------------
        // Under a single lock, decide this delegation's fate atomically against
        // everything a concurrent resolver may have recorded while we were
        // setting up:
        //   * a child terminal buffered against the reservation — a
        //     `TurnComplete` via `complete_call` (keyed by `call_id`) OR a child
        //     failure via `cancel_by_child_connection` (keyed by
        //     `child_connection_id`); either can race ahead of this park; or
        //   * a parent cancel that flagged this in-flight setup
        //     (`mark_inflight_canceled_for_parent`, which runs in the SAME lock
        //     acquisition that drains the parked `calls`).
        // Precedence: strict first-terminal-wins by arrival stamp. Both a child
        // terminal and a parent cancel carry the `seq` clock value they were
        // recorded at, so whichever landed FIRST wins — a child that completed
        // before the cancel keeps its result; a cancel that beat the completion
        // discards it (the parent had already abandoned the turn). Ties are
        // impossible: every event draws a distinct stamp under this one lock.
        // Only when NOTHING beat us do we park for a future resolver,
        // deregistering the in-flight record adjacent to `calls.insert` with no
        // `.await` between — so a parent cancel serialized AFTER us finds the
        // entry in `calls` and drains it, while one serialized BEFORE us is seen
        // here via its stamp. When a terminal/cancel DID beat us we deliberately
        // DON'T park: resolving inline (never leaving an entry for a second
        // resolver to grab) rules out a double-finalize.
        enum Disposition {
            ChildTerminal(DelegationOutcome),
            ParentCanceled,
            Running,
        }
        // Near-zero elapsed for these setup-window races, but measured for
        // consistency with the normal terminal paths.
        let setup_duration_ms = started_at.elapsed().as_millis() as u64;
        let disposition = {
            let mut inner = self.pending.inner.lock().await;
            // Each buffered child terminal carries (arrival_stamp, outcome).
            let child_terminal: Option<(u64, DelegationOutcome)> =
                if let Some((stamp, outcome)) = inner.take_early_complete(&call_id) {
                    Some((stamp, outcome))
                } else {
                    inner
                        .take_early_cancel(&child_connection_id)
                        .map(|(stamp, reason)| {
                            (
                                stamp,
                                DelegationOutcome::from_err(
                                    DelegationError::Canceled { reason },
                                    Some(child_conversation_id),
                                ),
                            )
                        })
                };
            let parent_canceled_at = inner.inflight_canceled_at(inflight_id);
            inner.unreserve(&call_id, &child_connection_id);
            // For both terminal dispositions we record the completed result
            // INSIDE this lock (atomically with unreserve/deregister) so a
            // concurrent `get_delegation_status` can never observe the task as
            // neither running nor completed. The `Running` arm inserts the live
            // task instead of parking a `oneshot` — the caller returns the ack.
            let record = |inner: &mut PendingInner, outcome: &DelegationOutcome| {
                inner.insert_completed(
                    &call_id,
                    build_completed(
                        &req.parent_connection_id,
                        child_conversation_id,
                        req.agent_type,
                        setup_duration_ms,
                        outcome,
                    ),
                );
            };
            match (child_terminal, parent_canceled_at) {
                // Both raced in the setup window: the earlier arrival stamp wins.
                (Some((child_stamp, outcome)), Some(cancel_stamp)) => {
                    inner.deregister_inflight(inflight_id);
                    if child_stamp < cancel_stamp {
                        record(&mut inner, &outcome);
                        Disposition::ChildTerminal(outcome)
                    } else {
                        record(
                            &mut inner,
                            &canceled_outcome(child_conversation_id, "parent canceled"),
                        );
                        Disposition::ParentCanceled
                    }
                }
                // Only a child terminal fired.
                (Some((_, outcome)), None) => {
                    inner.deregister_inflight(inflight_id);
                    record(&mut inner, &outcome);
                    Disposition::ChildTerminal(outcome)
                }
                // Only a parent cancel fired.
                (None, Some(_)) => {
                    inner.deregister_inflight(inflight_id);
                    record(
                        &mut inner,
                        &canceled_outcome(child_conversation_id, "parent canceled"),
                    );
                    Disposition::ParentCanceled
                }
                // Nothing beat us — register the running task for a future
                // resolver, deregistering the in-flight record adjacent to the
                // insert with no `.await` between (so a parent cancel serialized
                // AFTER us finds it in `running` and drains it).
                (None, None) => {
                    inner.running.insert(
                        call_id.clone(),
                        RunningTask {
                            child_connection_id: child_connection_id.clone(),
                            child_conversation_id,
                            parent_connection_id: req.parent_connection_id.clone(),
                            parent_tool_use_id: req.parent_tool_use_id.clone(),
                            agent_type: req.agent_type,
                            task_preview: task_preview.clone(),
                            task_id: call_id.clone(),
                            external_handle: req.external_handle.clone(),
                            started_at,
                            last_surfaced_block: None,
                        },
                    );
                    inner.deregister_inflight(inflight_id);
                    Disposition::Running
                }
            }
        };

        match disposition {
            // A child terminal beat registration. Finalize (terminal meta +
            // DelegationCompleted event + child teardown) and return the
            // terminal report directly. The completed entry was recorded under
            // the disposition lock above; wake any long-poll waiter.
            Disposition::ChildTerminal(outcome) => {
                self.finalize_delegation(
                    &req.parent_connection_id,
                    &req.parent_tool_use_id,
                    &child_connection_id,
                    child_conversation_id,
                    req.agent_type,
                    setup_duration_ms,
                    &outcome,
                    &task_preview,
                    &call_id,
                )
                .await;
                self.result_notify.notify_waiters();
                return report_from_outcome(
                    Some(call_id),
                    Some(req.agent_type),
                    &outcome,
                    Some(setup_duration_ms),
                );
            }
            // A parent cancel reached this delegation mid-setup — after the
            // prompt was sent, before we registered. Tear the child down
            // ourselves (cancel + disconnect, since a turn is in flight) and
            // return a canceled report. The canceled result was recorded above.
            Disposition::ParentCanceled => {
                self.write_meta_if_real(
                    &req.parent_connection_id,
                    &req.parent_tool_use_id,
                    build_delegation_meta(
                        "failed",
                        Some(&child_connection_id),
                        Some(child_conversation_id),
                        Some("canceled"),
                        None,
                        Some(setup_duration_ms),
                        Some(&task_preview),
                        Some(&call_id),
                    ),
                )
                .await;
                self.emit_completed_if_real(
                    &req.parent_connection_id,
                    &req.parent_tool_use_id,
                    &child_connection_id,
                    child_conversation_id,
                    req.agent_type,
                    DelegationResultSummary::Err {
                        error_code: "canceled".to_string(),
                    },
                )
                .await;
                let _ = self.spawner.cancel(&child_connection_id).await;
                let _ = self.spawner.disconnect(&child_connection_id).await;
                self.result_notify.notify_waiters();
                return report_from_outcome(
                    Some(call_id),
                    Some(req.agent_type),
                    &canceled_outcome(child_conversation_id, "parent canceled"),
                    Some(setup_duration_ms),
                );
            }
            // Registered in `running` — fall through to the second pre-cancel
            // check, then return the ack.
            Disposition::Running => {}
        }

        // Second pre-cancel check: a `notifications/cancelled` may have landed
        // between the entry-side check and the `running` registration above. If
        // so, drain the task ourselves (so a racing `cancel_by_external_handle`
        // doesn't double-finalize), record the canceled result, and return a
        // canceled report instead of the Running ack.
        if let Some(handle) = req.external_handle.as_deref() {
            if self.take_pre_canceled_handle(handle).await {
                // Capture the elapsed ONCE at terminalization (under the lock,
                // when the running task is removed) so the completed-cache, the
                // parent-card meta, and the returned report all report the same
                // duration. `None` when nothing was drained.
                let canceled_duration_ms = {
                    let mut inner = self.pending.inner.lock().await;
                    if inner.running.remove(&call_id).is_some() {
                        let outcome =
                            canceled_outcome(child_conversation_id, "canceled before await");
                        let duration_ms = started_at.elapsed().as_millis() as u64;
                        inner.insert_completed(
                            &call_id,
                            build_completed(
                                &req.parent_connection_id,
                                child_conversation_id,
                                req.agent_type,
                                duration_ms,
                                &outcome,
                            ),
                        );
                        Some(duration_ms)
                    } else {
                        None
                    }
                };
                if let Some(duration_ms) = canceled_duration_ms {
                    self.write_meta_if_real(
                        &req.parent_connection_id,
                        &req.parent_tool_use_id,
                        build_delegation_meta(
                            "failed",
                            Some(&child_connection_id),
                            Some(child_conversation_id),
                            Some("canceled"),
                            None,
                            Some(duration_ms),
                            Some(&task_preview),
                            Some(&call_id),
                        ),
                    )
                    .await;
                    self.emit_completed_if_real(
                        &req.parent_connection_id,
                        &req.parent_tool_use_id,
                        &child_connection_id,
                        child_conversation_id,
                        req.agent_type,
                        DelegationResultSummary::Err {
                            error_code: "canceled".to_string(),
                        },
                    )
                    .await;
                    let _ = self.spawner.cancel(&child_connection_id).await;
                    let _ = self.spawner.disconnect(&child_connection_id).await;
                    self.result_notify.notify_waiters();
                    return report_from_outcome(
                        Some(call_id),
                        Some(req.agent_type),
                        &canceled_outcome(child_conversation_id, "canceled before await"),
                        Some(duration_ms),
                    );
                }
            }
        }

        // Registered and running in the background — return the ack. The child
        // resolves later via the lifecycle → `complete_call` (or a cancel path).
        running_ack(call_id, child_conversation_id, req.agent_type)
    }

    /// Called by the child-session lifecycle subscriber on `TurnComplete`
    /// (success path) or by error mappers (failure path).
    ///
    /// Migrates the task from `running` into `completed` (atomically, under one
    /// lock) and then finalizes (terminal meta + `DelegationCompleted` event +
    /// child teardown) and wakes any `get_delegation_status` long-poll.
    ///
    /// If no entry is in `running` under `call_id`, the outcome is buffered for
    /// a racing `start_delegation` to drain at registration — but ONLY while the
    /// delegation is still reserved (mid-setup). This closes the window where a
    /// fast/empty turn's `TurnComplete` propagates through the lifecycle while
    /// `start_delegation` is still between `send_prompt` and the `running`
    /// insert: the prompt is only *enqueued* by `send_prompt`, and the child
    /// loop emits `TurnComplete` independently, so a completion CAN beat it. When
    /// the `call_id` is no longer reserved the call was already resolved by
    /// another terminal path, so the buffer is skipped (silent no-op).
    pub async fn complete_call(&self, call_id: &str, outcome: DelegationOutcome) {
        let task = {
            let mut inner = self.pending.inner.lock().await;
            match inner.running.remove(call_id) {
                Some(task) => {
                    // Atomic running → completed so a concurrent status query
                    // never sees the task as neither running nor completed.
                    let duration_ms = task.started_at.elapsed().as_millis() as u64;
                    inner.insert_completed(
                        call_id,
                        build_completed(
                            &task.parent_connection_id,
                            task.child_conversation_id,
                            task.agent_type,
                            duration_ms,
                            &outcome,
                        ),
                    );
                    Some((task, duration_ms))
                }
                None => {
                    // Buffer for the racing `start_delegation` to drain iff still
                    // reserved (mid-setup); a no-op otherwise, so the clone only
                    // materializes on the genuine pre-registration race.
                    inner.buffer_early_complete(call_id, outcome.clone());
                    None
                }
            }
        };
        if let Some((task, duration_ms)) = task {
            self.finalize_delegation(
                &task.parent_connection_id,
                &task.parent_tool_use_id,
                &task.child_connection_id,
                task.child_conversation_id,
                task.agent_type,
                duration_ms,
                &outcome,
                &task.task_preview,
                &task.task_id,
            )
            .await;
            self.result_notify.notify_waiters();
        }
    }

    /// Write the terminal meta, emit `DelegationCompleted`, and tear down the
    /// child for a resolved delegation. Shared by `complete_call` and
    /// `start_delegation`'s early-terminal pickup. Mirrors the resolution onto
    /// the parent's `delegate_to_agent` ToolCallState meta (including a bounded
    /// `text_preview` on the completed path so a post-refresh snapshot renders
    /// the result inline) so snapshot recovery shows the final state without the
    /// live `delegation_completed` event. Does not touch the pending maps — the
    /// caller owns the `running` → `completed` migration.
    ///
    /// `duration_ms` is the broker-measured elapsed time (from `started_at`),
    /// carried onto the event summary so the parent UI shows a real duration.
    #[allow(clippy::too_many_arguments)]
    async fn finalize_delegation(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
        duration_ms: u64,
        outcome: &DelegationOutcome,
        task_preview: &str,
        task_id: &str,
    ) {
        let meta = match outcome {
            DelegationOutcome::Ok(ok) => build_delegation_meta(
                "completed",
                Some(child_connection_id),
                Some(child_conversation_id),
                None,
                build_text_preview(&ok.text).as_deref(),
                Some(duration_ms),
                Some(task_preview),
                Some(task_id),
            ),
            DelegationOutcome::Err { code, .. } => build_delegation_meta(
                "failed",
                Some(child_connection_id),
                Some(child_conversation_id),
                Some(code),
                None,
                Some(duration_ms),
                Some(task_preview),
                Some(task_id),
            ),
        };
        self.write_meta_if_real(parent_connection_id, parent_tool_use_id, meta)
            .await;
        self.emit_completed_if_real(
            parent_connection_id,
            parent_tool_use_id,
            child_connection_id,
            child_conversation_id,
            agent_type,
            outcome_to_summary(outcome, duration_ms),
        )
        .await;
        // v1 one-shot: always tear down the child.
        let _ = self.spawner.disconnect(child_connection_id).await;
    }

    /// Internal helper — apply the meta write iff the parent's
    /// `tool_use_id` refers to a real ACP `tool_call_id`. The
    /// broker-synthesized `"delegation-<uuid>"` placeholder targets no
    /// ToolCallState, so emitting a `ToolCallUpdate` against it would be
    /// noise that the frontend would route through `apply_tool_call_update`
    /// to a non-existent entry. See `meta_writer::is_synthetic_parent_tool_use_id`.
    async fn write_meta_if_real(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        meta: serde_json::Value,
    ) {
        if is_synthetic_parent_tool_use_id(parent_tool_use_id) {
            return;
        }
        self.meta_writer
            .write_meta(parent_connection_id, parent_tool_use_id, meta)
            .await;
    }

    /// Internal helper — emit `AcpEvent::DelegationStarted` on the parent's
    /// stream iff the `parent_tool_use_id` refers to a real ACP tool_call.
    /// Mirror of `emit_completed_if_real`: same synthetic-id skip, and the
    /// event rides the parent's stream so the frontend `DelegationContext`
    /// receives it via the parent's per-connection attach stream in
    /// web/server mode (not only via the desktop firehose).
    #[allow(clippy::too_many_arguments)]
    async fn emit_started_if_real(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
        task_preview: &str,
        task_id: &str,
    ) {
        if is_synthetic_parent_tool_use_id(parent_tool_use_id) {
            return;
        }
        self.event_emitter
            .emit_started(
                parent_connection_id,
                parent_tool_use_id,
                child_connection_id,
                child_conversation_id,
                agent_type,
                task_preview,
                task_id,
            )
            .await;
    }

    /// Internal helper — emit `AcpEvent::DelegationCompleted` on the parent's
    /// stream iff the `parent_tool_use_id` refers to a real ACP tool_call.
    /// Synthetic ids (the `"delegation-<uuid>"` UUID fallback) map to no
    /// live UI binding, so the emit would be wasted noise — same skip
    /// criterion as `write_meta_if_real`.
    async fn emit_completed_if_real(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
        result: DelegationResultSummary,
    ) {
        if is_synthetic_parent_tool_use_id(parent_tool_use_id) {
            return;
        }
        self.event_emitter
            .emit_completed(
                parent_connection_id,
                parent_tool_use_id,
                child_connection_id,
                child_conversation_id,
                agent_type,
                result,
            )
            .await;
    }

    /// Cancel the pending delegation whose `external_handle` matches.
    /// Called by the MCP listener on receipt of `notifications/cancelled`
    /// from a companion. When no matching pending entry exists (the
    /// cancel arrived before `handle_request` reached the
    /// pending-registration phase) the handle is stashed in
    /// `pre_canceled_handles` so the in-flight request can drain itself
    /// when it tries to register or shortly after.
    pub async fn cancel_by_external_handle(&self, external_handle: &str, reason: String) {
        let drained = {
            let mut inner = self.pending.inner.lock().await;
            let keys: Vec<String> = inner
                .running
                .iter()
                .filter(|(_, v)| v.external_handle.as_deref() == Some(external_handle))
                .map(|(k, _)| k.clone())
                .collect();
            drain_and_record_canceled(&mut inner, keys, &reason)
        };
        if drained.is_empty() {
            // Race: the cancel beat the handle's `running` registration. Buffer
            // it (capped, FIFO-evicted) so `start_delegation` can drain itself on
            // the next checkpoint instead of proceeding to spawn the child.
            self.buffer_pre_canceled_handle(external_handle.to_string())
                .await;
            return;
        }
        for (task, duration_ms) in drained {
            // A turn is in flight, so cancel + disconnect.
            self.teardown_canceled_child(&task, duration_ms, true).await;
        }
        self.result_notify.notify_waiters();
    }

    /// Resolve the pending delegation whose child matches
    /// `child_connection_id` with a `canceled` outcome. Used when a child
    /// session disconnects or errors out without firing a clean
    /// TurnComplete — the parent's `tool_use_id` shouldn't dangle.
    /// No-op when no matching entry exists.
    ///
    /// `terminal_error` carries the child connection's last `AcpEvent::Error`
    /// detail when the lifecycle worker is dispatching off an `Error` event
    /// (vs. a bare `Disconnected`). When present, it gets appended to the
    /// `Canceled { reason }` string so the parent agent's tool-call result
    /// surfaces the real cause (e.g. "Authentication required",
    /// "transport closed") instead of the opaque default. Falls back to
    /// the default reason when `None`.
    pub async fn cancel_by_child_connection(
        &self,
        child_connection_id: &str,
        terminal_error: Option<&str>,
    ) {
        let reason = child_canceled_reason(terminal_error);
        let drained = {
            let mut inner = self.pending.inner.lock().await;
            let keys: Vec<String> = inner
                .running
                .iter()
                .filter(|(_, v)| v.child_connection_id == child_connection_id)
                .map(|(k, _)| k.clone())
                .collect();
            if keys.is_empty() {
                // No running entry. If the child is still reserved,
                // `start_delegation` is mid-setup and this failure beat the
                // `running` insert — buffer its detail for it to drain at
                // registration instead of no-oping. `buffer_child_failure` is a
                // no-op when the child isn't reserved, so a normal
                // post-resolution child teardown accumulates nothing.
                inner.buffer_child_failure(
                    child_connection_id,
                    terminal_error.map(|s| s.to_string()),
                );
                Vec::new()
            } else {
                drain_and_record_canceled(&mut inner, keys, &reason)
            }
        };
        for (task, duration_ms) in drained {
            // The child already disconnected/errored — disconnect-only teardown
            // (no spawner `cancel`, there's no live turn to interrupt).
            self.teardown_canceled_child(&task, duration_ms, false).await;
        }
        self.result_notify.notify_waiters();
    }

    /// Cascade-cancel every pending delegation owned by `parent_connection_id`
    /// when the parent **connection tears down** (disconnect / `run_connection`
    /// exit). Drops the parent's entire tool_call tracker bucket (`pending` +
    /// `consumed`) since the connection is going away. Runs fully inline — the
    /// connection is already exiting, so there is no next prompt to unblock.
    pub async fn cancel_by_parent(&self, parent_connection_id: &str) {
        let drained = self
            .drain_for_parent_cancel(parent_connection_id, false)
            .await;
        self.finalize_parent_cancel(drained).await;
    }

    /// Cascade-cancel every pending delegation owned by `parent_connection_id`
    /// for a **turn/prompt cancel** where the parent connection STAYS ALIVE
    /// (a non-`end_turn` turn end, or a user Cancel between/within prompts).
    ///
    /// The fast, turn-scoped part — tombstoning the tool_call tracker and
    /// removing this parent's parked calls — runs SYNCHRONOUSLY: the caller
    /// awaits it before the connection loop accepts the next prompt, so it can't
    /// race a next-turn registration and tombstone/cancel that turn's legitimate
    /// entries (the safety the `drop_tool_calls_for_parent` invariant relies
    /// on). Only the slow child teardown (meta/emit + spawner `cancel` /
    /// `disconnect`, which can block on slow agents) is backgrounded, so the
    /// user-visible Cancel path stays responsive.
    ///
    /// RETAINS the parent's `consumed` tool_call memory (and tombstones the
    /// cancelled turn's unclaimed `pending` ids into it): dropping it would let
    /// a host re-emit of an already-handled `tool_call_id` re-register and
    /// mis-bind the next same-key delegation on this live connection — see
    /// `drop_tool_calls_for_parent`.
    pub async fn cancel_by_parent_turn(&self, parent_connection_id: &str) {
        let drained = self
            .drain_for_parent_cancel(parent_connection_id, true)
            .await;
        // The fast drain above already ran inline (scoped to the just-ended
        // turn); background only the slow child teardown.
        let broker = self.clone();
        tokio::spawn(async move {
            broker.finalize_parent_cancel(drained).await;
        });
    }

    /// Fast, lock-guarded part of a parent cancel: drop/tombstone this parent's
    /// tool_call tracker (per `keep_consumed`, see `drop_tool_calls_for_parent`)
    /// and remove every running task it owns, returning them for the (slow)
    /// child teardown. Touches only the two broker mutexes — no spawner I/O — so
    /// it is safe to await inline in the connection loop before the next prompt
    /// is accepted.
    ///
    /// `keep_consumed` also governs the completed-cache: a **turn** cancel
    /// (`true`) records each drained task as `Canceled` so the still-alive
    /// connection's LLM can still query it; a **connection teardown** (`false`)
    /// drops the parent's whole completed-cache instead — the parent is gone, so
    /// nothing will query it.
    async fn drain_for_parent_cancel(
        &self,
        parent_connection_id: &str,
        keep_consumed: bool,
    ) -> Vec<(RunningTask, u64)> {
        // Also drain any tool_call ids captured ahead of an MCP round-trip that
        // never arrived — keeps the map bounded across parent reconnects.
        // Teardown drops the whole bucket; a turn cancel keeps `consumed` so a
        // later re-emit can't mis-bind the next delegation.
        self.drop_tool_calls_for_parent(parent_connection_id, keep_consumed)
            .await;
        let drained = {
            let mut inner = self.pending.inner.lock().await;
            // Flag every still-in-flight setup this parent owns in the SAME lock
            // acquisition that drains its running tasks: a delegation is then
            // caught either here (mid-setup → `start_delegation` tears its child
            // down at the next checkpoint) or by the running drain below (already
            // registered) — there is no interleaving where both miss it.
            inner.mark_inflight_canceled_for_parent(parent_connection_id);
            let keys: Vec<String> = inner
                .running
                .iter()
                .filter(|(_, v)| v.parent_connection_id == parent_connection_id)
                .map(|(k, _)| k.clone())
                .collect();
            if keep_consumed {
                // Turn cancel: connection stays alive → keep each canceled
                // result queryable.
                drain_and_record_canceled(&mut inner, keys, "parent canceled")
            } else {
                // Connection teardown: just remove the running tasks and drop the
                // whole completed-cache for this parent. No completed entry to
                // match, but still capture the elapsed once (at drain time) so
                // the teardown meta doesn't recompute it later.
                let drained: Vec<(RunningTask, u64)> = keys
                    .into_iter()
                    .map(|k| {
                        let task = inner.running.remove(&k).expect("key just observed");
                        let duration_ms = task.started_at.elapsed().as_millis() as u64;
                        (task, duration_ms)
                    })
                    .collect();
                inner.drop_completed_for_parent(parent_connection_id);
                drained
            }
        };
        self.result_notify.notify_waiters();
        drained
    }

    /// Slow part of a parent cancel: for each drained task, patch the parent
    /// meta, emit `DelegationCompleted`, and tear the child down. The canceled
    /// result was already recorded into `completed` (turn cancel) by
    /// `drain_for_parent_cancel` under the lock, so this is pure I/O. Split out
    /// so a turn cancel can background it without delaying the fast, turn-scoped
    /// drain.
    async fn finalize_parent_cancel(&self, drained: Vec<(RunningTask, u64)>) {
        for (task, duration_ms) in drained {
            // A turn was in flight → cancel + disconnect.
            self.teardown_canceled_child(&task, duration_ms, true).await;
        }
    }

    /// Shared canceled-child teardown: best-effort `failed`/`canceled` meta
    /// patch (so a parent-side snapshot post-cancel shows the delegation as
    /// canceled rather than stuck on "running"), a `DelegationCompleted` err
    /// event, then child teardown. `cancel_turn` is `true` when a turn is in
    /// flight (cancel + disconnect) and `false` when the child already
    /// disconnected/errored (disconnect only). Does NOT touch the pending maps —
    /// the caller already migrated the task into `completed`.
    ///
    /// `duration_ms` is the elapsed captured by `drain_and_record_canceled` at
    /// drain time — reused here (not recomputed) so the parent-card meta matches
    /// the completed-cache duration the status/cancel cards report, even when
    /// this teardown is backgrounded.
    async fn teardown_canceled_child(
        &self,
        task: &RunningTask,
        duration_ms: u64,
        cancel_turn: bool,
    ) {
        self.write_meta_if_real(
            &task.parent_connection_id,
            &task.parent_tool_use_id,
            build_delegation_meta(
                "failed",
                Some(&task.child_connection_id),
                Some(task.child_conversation_id),
                Some("canceled"),
                None,
                Some(duration_ms),
                Some(&task.task_preview),
                Some(&task.task_id),
            ),
        )
        .await;
        self.emit_completed_if_real(
            &task.parent_connection_id,
            &task.parent_tool_use_id,
            &task.child_connection_id,
            task.child_conversation_id,
            task.agent_type,
            DelegationResultSummary::Err {
                error_code: "canceled".to_string(),
            },
        )
        .await;
        if cancel_turn {
            let _ = self.spawner.cancel(&task.child_connection_id).await;
        }
        let _ = self.spawner.disconnect(&task.child_connection_id).await;
    }

    /// Backs the `get_delegation_status` tool for a single task id — a thin
    /// wrapper over [`Self::get_tasks_status`] so the single- and batch-poll
    /// paths share one snapshot/wait implementation. A one-id batch's
    /// "any task settled" wake condition is exactly "this task settled", so the
    /// blocking semantics are identical to the historical single-task loop.
    pub async fn get_task_status(
        &self,
        parent_connection_id: &str,
        parent_conversation_id: Option<i32>,
        task_id: &str,
        wait: StatusWait,
    ) -> DelegationTaskReport {
        let ids = [task_id.to_string()];
        self.get_tasks_status(parent_connection_id, parent_conversation_id, &ids, wait)
            .await
            .pop()
            .unwrap_or_else(|| unknown_report(task_id))
    }

    /// Wake every parked status long-poll so it can re-probe its children.
    ///
    /// Called by the lifecycle dispatcher when ANY connection raises or resolves
    /// a blocking prompt — permission, question, or plan approval. Cheap and
    /// unconditional: these events are rare, and a wake for a connection that
    /// isn't anybody's delegation child just re-snapshots and re-parks, exactly
    /// like the spurious wakes `get_tasks_status` already tolerates.
    pub fn note_blocking_changed(&self) {
        self.result_notify.notify_waiters();
    }

    /// Backs the batch `get_delegation_status` tool. Resolves the status of one
    /// or many task ids in a single pass — each from the completed-cache, then
    /// the running set, then the DB fallback — scoped to the calling parent (a
    /// task owned by another parent reports `Unknown`, never leaking it). Returns
    /// one report per requested id, in request order.
    ///
    /// Blocking obeys [`StatusWait`]: `Immediate` returns the first snapshot.
    /// `Bounded`/`Infinite` return as soon as ANY requested task is terminal —
    /// INCLUDING one already terminal at entry, so a completed result is never
    /// held hostage to a long-running sibling (the caller re-polls the
    /// still-running ids to collect the rest) — **or as soon as one becomes
    /// blocked on a user decision**. Only an all-running, nothing-newly-blocked
    /// batch parks: it wakes when a task settles (the running count drops below
    /// the total), when a child raises a blocking prompt, or — for `Bounded` —
    /// when the deadline elapses. An all-settled batch returns immediately even
    /// under `Infinite`, so it never parks forever.
    ///
    /// The blocked-return is what closes #447. Without it, a child that hits a
    /// permission prompt parks the parent's `wait_ms: 0` call for as long as the
    /// user takes to notice — indefinitely, for an unattended run — and the
    /// orchestrating LLM cannot even report the stall, because "working" and
    /// "waiting on a human" look identical from here. Each blocking prompt is
    /// surfaced ONCE (see `RunningTask::last_surfaced_block`), so re-polling an
    /// already-reported block goes back to waiting rather than spinning.
    pub async fn get_tasks_status(
        &self,
        parent_connection_id: &str,
        parent_conversation_id: Option<i32>,
        task_ids: &[String],
        wait: StatusWait,
    ) -> Vec<DelegationTaskReport> {
        if task_ids.is_empty() {
            return Vec::new();
        }
        // A bounded wait gets a single fixed deadline; Immediate and Infinite
        // carry none — Immediate returns on the first pass, Infinite parks on
        // `result_notify` until a task is terminal or newly blocked.
        let deadline = match wait {
            StatusWait::Bounded(ms) => Some(Instant::now() + Duration::from_millis(ms)),
            StatusWait::Immediate | StatusWait::Infinite => None,
        };
        loop {
            // Arm the notify BEFORE the snapshot so a completion landing between
            // the snapshot and the await isn't lost (enable() registers now).
            let notified = self.result_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            // One lock acquisition classifies every requested id. The async
            // resolution of running (live reply) / not-in-memory (DB) ids is
            // deferred to `assemble_reports`, OUTSIDE this lock.
            let classes: Vec<StatusClass> = {
                let inner = self.pending.inner.lock().await;
                task_ids
                    .iter()
                    .map(|id| classify_locked(&inner, parent_connection_id, id))
                    .collect()
            };
            let running_count = classes
                .iter()
                .filter(|c| matches!(c, StatusClass::Running { .. }))
                .count();

            // Probe each running child for a blocking prompt. Done HERE, after
            // the pending lock was dropped at the end of the block above and
            // before any early return, because it takes the child's
            // `SessionState` lock — the same ordering `attach_live_reply` has
            // always used. The result rides along to `assemble_reports` so a
            // pass never probes the same child twice.
            let blocked = self.probe_blocked(&classes).await;

            // Return now when the poll is Immediate, OR when at least one
            // requested task is already (or now) terminal — i.e. not EVERY task
            // is still running. This honors the contract "returns as soon as ANY
            // requested task reaches a terminal state": a mixed [terminal,
            // running] batch surfaces the terminal report immediately instead of
            // holding it hostage to a long-running sibling, and the caller
            // re-polls (narrowing to the still-running ids) to collect the rest.
            // `running_count == 0` (all settled) is the special case that also
            // makes Infinite safe. The id set is fixed and a task can only LEAVE
            // the running map during a wait (never (re)enter), so once a parked
            // all-running batch is woken by a settle the count has dropped below
            // the total and this returns; a spurious wake (another parent's task)
            // re-snapshots all-running and re-parks.
            if matches!(wait, StatusWait::Immediate) || running_count < task_ids.len() {
                return self
                    .assemble_reports(parent_conversation_id, task_ids, classes, blocked)
                    .await;
            }
            // Every task is still running, but one of them just parked on the
            // user. That is not progress and no completion signal is coming
            // until a human acts, so stop waiting and say so. The claim marks
            // each prompt as surfaced, so the NEXT poll on the same prompt falls
            // through to the park below instead of returning instantly.
            let claimed = self.claim_new_blocks(task_ids, &blocked).await;
            if claimed.any_claimed {
                return self
                    .assemble_reports(parent_conversation_id, task_ids, classes, blocked)
                    .await;
            }
            // A `Bounded` wait gives up at its deadline and returns the running
            // snapshot.
            let now = Instant::now();
            if deadline.is_some_and(|d| now >= d) {
                return self
                    .assemble_reports(parent_conversation_id, task_ids, classes, blocked)
                    .await;
            }
            // Park until the next completion signal — bounded by the deadline
            // when there is one, and ALWAYS bounded by `retry_at` when a
            // suppressed block is waiting to become reportable again. Without
            // that second bound an `Infinite` wait would park on a notify that
            // nothing is going to fire: a child parked on a permission emits
            // nothing further by itself, so the resurface valve would never get
            // a chance to run and a report lost in transit would strand the
            // caller for good.
            let wake_at = match (deadline, claimed.retry_at) {
                (Some(d), Some(r)) => Some(d.min(r)),
                (d, r) => d.or(r),
            };
            match wake_at {
                Some(t) => {
                    let remaining = t.saturating_duration_since(now);
                    tokio::select! {
                        _ = &mut notified => {}
                        _ = tokio::time::sleep(remaining) => {}
                    }
                }
                None => {
                    notified.await;
                }
            }
            // Loop: re-snapshot (a task likely just completed, the deadline
            // passed and the next pass returns the running snapshot, or a
            // suppressed block is now reportable again).
        }
    }

    /// Ask each `Running` entry's child what it is blocked on, position-aligned
    /// with `classes` (non-running slots are `None`). One probe per child per
    /// pass; must be called with the pending lock RELEASED (it takes the child's
    /// `SessionState` lock).
    async fn probe_blocked(&self, classes: &[StatusClass]) -> Vec<Option<BlockedOn>> {
        let mut out = Vec::with_capacity(classes.len());
        for class in classes {
            let blocked = match class {
                StatusClass::Running {
                    child_connection_id,
                    ..
                } => self.live_reply_lookup.blocked_on(child_connection_id).await,
                _ => None,
            };
            out.push(blocked);
        }
        out
    }

    /// Claim the blocking prompts in `blocked` that are reportable right now,
    /// returning whether any was — and, when none was, the earliest [`Instant`]
    /// at which one becomes reportable again.
    ///
    /// This is the ratchet: a `wait_ms: 0` poll returns the first time a child
    /// parks, but a re-poll while the SAME prompt is still unanswered finds
    /// nothing new and goes back to waiting — otherwise the caller's wait loop
    /// would return instantly forever and burn a round-trip per iteration while
    /// the user is simply slow to click. A second, different prompt is new
    /// again and returns, and so does the same prompt once
    /// [`BLOCK_RESURFACE_INTERVAL`] has passed (the delivery-failure valve — see
    /// [`RunningTask::last_surfaced_block`]).
    ///
    /// Positionally aligned with `task_ids`. Silently skips ids no longer in the
    /// running map (settled between the snapshot and here) — those return via
    /// the terminal path on the next pass anyway.
    async fn claim_new_blocks(
        &self,
        task_ids: &[String],
        blocked: &[Option<BlockedOn>],
    ) -> ClaimedBlocks {
        let mut out = ClaimedBlocks::default();
        if blocked.iter().all(|b| b.is_none()) {
            return out;
        }
        let now = Instant::now();
        let mut inner = self.pending.inner.lock().await;
        for (id, blocked) in task_ids.iter().zip(blocked) {
            let Some(blocked) = blocked else { continue };
            let Some(task) = inner.running.get_mut(id) else {
                continue;
            };
            if let Some(prev) = task.last_surfaced_block.as_ref() {
                if prev.request_id == blocked.request_id {
                    let due = prev.at + self.block_resurface;
                    if now < due {
                        // Not reportable yet. Deliberately does NOT refresh
                        // `at` — that would push the valve out forever under
                        // repeated polling.
                        out.retry_at = Some(out.retry_at.map_or(due, |t: Instant| t.min(due)));
                        continue;
                    }
                }
            }
            task.last_surfaced_block = Some(SurfacedBlock {
                request_id: blocked.request_id.clone(),
                at: now,
            });
            out.any_claimed = true;
        }
        out
    }

    /// Finish a batch status pass: resolve each [`StatusClass`] into a final
    /// report AFTER the pending lock is released. `Running` ids get their latest
    /// live reply (or blocking prompt) attached; `NotInMemory` ids fall back to
    /// the DB status lookup. Reports come back in `task_ids` order.
    ///
    /// `blocked` is the already-probed result from [`Self::probe_blocked`] for
    /// this same pass, position-aligned with `classes`.
    async fn assemble_reports(
        &self,
        parent_conversation_id: Option<i32>,
        task_ids: &[String],
        classes: Vec<StatusClass>,
        blocked: Vec<Option<BlockedOn>>,
    ) -> Vec<DelegationTaskReport> {
        let mut out = Vec::with_capacity(classes.len());
        let mut blocked = blocked.into_iter().chain(std::iter::repeat_with(|| None));
        for (id, class) in task_ids.iter().zip(classes) {
            let blocked = blocked.next().flatten();
            let report = match class {
                StatusClass::Settled(report) => report,
                StatusClass::Running {
                    mut report,
                    child_connection_id,
                } => {
                    match blocked {
                        // A blocked child's last reply is stale by definition —
                        // it is what it said BEFORE it stopped — so the block
                        // replaces it rather than competing with it.
                        Some(blocked) => attach_blocked(&mut report, blocked),
                        None => {
                            self.attach_live_reply(&mut report, &child_connection_id)
                                .await
                        }
                    }
                    report
                }
                StatusClass::NotInMemory => self.status_from_db(parent_conversation_id, id).await,
            };
            out.push(report);
        }
        out
    }

    /// Upgrade a running report's bare `"Running."` message with the child's
    /// latest one-line activity, so the parent LLM gets a concrete sign of
    /// progress it can report in one shot (instead of polling-and-narrating).
    /// Called only on the actual running-return paths, AFTER the pending lock is
    /// released. A no-op when the lookup has nothing (default Noop lookup, child
    /// gone, or no live output yet) — the report stays `"Running."`.
    ///
    /// The hint goes on its OWN line (`"Running.\nLatest sub-agent reply: …"`),
    /// not appended to the marker line. On hosts that persist only the
    /// `CallToolResult` content text (e.g. Claude Code), the frontend recognizes
    /// a still-running poll by the standalone first line `"Running."` — keeping
    /// the child-controlled reply text on a separate line means a *completed*
    /// result that merely starts with "Running. …" can never be misread as
    /// running. See `textRunningStatus` in `src/lib/delegation-status.ts`.
    async fn attach_live_reply(
        &self,
        report: &mut DelegationTaskReport,
        child_connection_id: &str,
    ) {
        if let Some(reply) = self
            .live_reply_lookup
            .latest_reply(child_connection_id)
            .await
        {
            report.message = Some(format!("Running.\nLatest sub-agent reply: {reply}"));
        }
    }

    /// Backs the `cancel_delegation` tool. Cancels a running task owned by the
    /// caller (recording it `Canceled` + tearing the child down) and returns the
    /// resulting report. A task that already finished returns its terminal
    /// report; one not in memory falls back to the DB status (a finished task
    /// can't be canceled). Parent-scoped like `get_task_status`.
    pub async fn cancel_task_by_id(
        &self,
        parent_connection_id: &str,
        parent_conversation_id: Option<i32>,
        task_id: &str,
    ) -> DelegationTaskReport {
        let drained = {
            let mut inner = self.pending.inner.lock().await;
            if let Some(c) = inner.completed.get(task_id) {
                if c.parent_connection_id == parent_connection_id {
                    return completed_report(task_id, c);
                }
                return unknown_report(task_id);
            }
            match inner.running.get(task_id) {
                Some(r) if r.parent_connection_id == parent_connection_id => {
                    drain_and_record_canceled(
                        &mut inner,
                        vec![task_id.to_string()],
                        "canceled by request",
                    )
                    .pop()
                }
                Some(_) => return unknown_report(task_id),
                None => None,
            }
        };
        match drained {
            Some((task, duration_ms)) => {
                // A turn is in flight → cancel + disconnect. Reuse the duration
                // captured at drain time for both the teardown meta and the
                // report, so all three (completed-cache, meta, report) agree.
                self.teardown_canceled_child(&task, duration_ms, true).await;
                self.result_notify.notify_waiters();
                report_from_outcome(
                    Some(task_id.to_string()),
                    Some(task.agent_type),
                    &canceled_outcome(task.child_conversation_id, "canceled by request"),
                    Some(duration_ms),
                )
            }
            None => self.status_from_db(parent_conversation_id, task_id).await,
        }
    }

    /// DB status fallback for a task evicted from / never in the in-memory maps.
    /// Scopes to the caller's conversation: a child whose `parent_id` doesn't
    /// match (or when the caller has no active conversation) reports `Unknown`.
    async fn status_from_db(
        &self,
        parent_conversation_id: Option<i32>,
        task_id: &str,
    ) -> DelegationTaskReport {
        match self.status_lookup.find_by_call_id(task_id).await {
            Some(rec)
                if parent_conversation_id.is_some() && rec.parent_id == parent_conversation_id =>
            {
                db_report(task_id, &rec)
            }
            _ => unknown_report(task_id),
        }
    }

    /// Backs the `resume_delegation` tool: bring an INTERRUPTED delegation task
    /// back to life under its original `task_id`.
    ///
    /// Resumable states — and nothing else:
    ///   * `Canceled` — explicit `cancel_delegation`, parent teardown, a child
    ///     process dying without `TurnComplete`, or app shutdown; the child row
    ///     records `cancelled`.
    ///   * A stale `in_progress` child row with NO live connection — the
    ///     leftover of a hard crash that never ran any cancel path.
    ///
    /// Everything else is refused with `error_code: "not_resumable"` and the
    /// task's actual status: a running task keeps running (poll it), and a
    /// `Completed` / `Failed` task ended by its own doing — resuming it would
    /// be a new iteration on the original task, which this tool deliberately
    /// does not do (that is `delegate_to_agent`'s job). The same contract is
    /// why there is no task-text parameter: the child continues the ORIGINAL
    /// task in its own resumed session (`session/load` via the row's
    /// `external_id`), nudged by a fixed continuation prompt that may carry
    /// only the bounded `reason` as interruption context.
    ///
    /// The resumed run KEEPS the task id: the child row already persists it as
    /// `delegation_call_id`, so the lifecycle's TurnComplete routing
    /// (`forward_turn_complete_to_broker` → [`Self::complete_call`]) re-arms
    /// automatically once the continuation prompt lands in the same row — and
    /// the caller's `get_delegation_status` / `cancel_delegation` keep working
    /// on the id they already hold. Ownership is re-anchored to the CALLER:
    /// the registered [`RunningTask`] carries the calling connection (the
    /// original one is usually gone — that is why the task needed resuming),
    /// while meta writes / events reuse the row's original
    /// `parent_tool_use_id` so the parent's original delegation card re-binds
    /// to the resumed child.
    #[tracing::instrument(
        name = "delegation_resume",
        skip_all,
        fields(
            parent_connection_id = %req.parent_connection_id,
            task_id = %req.task_id,
            child_connection_id = tracing::field::Empty,
        )
    )]
    pub async fn resume_delegation(&self, req: ResumeDelegationRequest) -> DelegationTaskReport {
        // Same parent-cancel coverage contract as `start_delegation`: register
        // the in-flight setup FIRST so a parent cancel landing anywhere from
        // here to park reaches it; deregistered on every exit path.
        let inflight_id = self
            .pending
            .inner
            .lock()
            .await
            .register_inflight(&req.parent_connection_id);
        // Pre-cancel short-circuit — the companion already received
        // `notifications/cancelled` for this `tools/call` before we started.
        // No tool-call claim to drain here: the resume tool never claims
        // delegate-keyed ACP ids.
        if let Some(handle) = req.external_handle.as_deref() {
            if self.take_pre_canceled_handle(handle).await {
                self.drop_inflight(inflight_id).await;
                return report_from_outcome(
                    Some(req.task_id.clone()),
                    None,
                    &DelegationOutcome::from_err(
                        DelegationError::Canceled {
                            reason: "canceled before resume".into(),
                        },
                        None,
                    ),
                    None,
                );
            }
        }
        let cfg = self.config_snapshot().await;
        if !cfg.enabled {
            self.drop_inflight(inflight_id).await;
            return report_from_outcome(
                Some(req.task_id.clone()),
                None,
                &DelegationOutcome::from_err(
                    DelegationError::Canceled {
                        reason: "delegation disabled".into(),
                    },
                    None,
                ),
                None,
            );
        }

        // --- In-memory gate ---------------------------------------------------
        // One lock pass over the live maps. Only a CANCELED, caller-owned cached
        // entry may proceed (its removal is deferred to the registration lock so
        // a failed resume leaves the record answering status queries); a running
        // task and fresher terminal states refuse, and cross-parent hits stay
        // opaque — exactly the `classify_locked` scoping rules.
        //
        // A proceed also CLAIMS the id in `resuming` (same lock), which is what
        // stops two concurrent resumes of one task from both passing this gate
        // and spawning two children onto the same agent session. In the
        // not-in-memory branch ownership isn't proven yet, so a failed claim is
        // carried as `claimed: false` and only refused AFTER the DB ownership
        // check — refusing it here would leak the task's existence to a caller
        // who merely guessed the id.
        enum MemGate {
            Proceed { claimed: bool },
            Refuse(Box<DelegationTaskReport>),
        }
        let gate = {
            let mut inner = self.pending.inner.lock().await;
            if let Some(c) = inner.completed.get(&req.task_id) {
                if c.parent_connection_id != req.parent_connection_id {
                    MemGate::Refuse(Box::new(unknown_report(&req.task_id)))
                } else {
                    // Copy the report fields out so the `completed` borrow ends
                    // before the Canceled arm's mutable `resuming` claim.
                    let (status, child_conversation_id, agent_type) =
                        (c.status, c.child_conversation_id, c.agent_type);
                    match status {
                        TaskStatus::Canceled => {
                            // Caller-owned — safe to name the contention.
                            if inner.resuming.insert(req.task_id.clone()) {
                                MemGate::Proceed { claimed: true }
                            } else {
                                MemGate::Refuse(Box::new(not_resumable_report(
                                    &req.task_id,
                                    status,
                                    Some(child_conversation_id),
                                    Some(agent_type),
                                    "a resume of this task is already in progress.",
                                )))
                            }
                        }
                        TaskStatus::Completed => MemGate::Refuse(Box::new(not_resumable_report(
                            &req.task_id,
                            status,
                            Some(child_conversation_id),
                            Some(agent_type),
                            "the task already completed — resume only applies to a \
                             canceled or interrupted task. Start a new \
                             delegate_to_agent call for follow-up work.",
                        ))),
                        _ => MemGate::Refuse(Box::new(not_resumable_report(
                            &req.task_id,
                            status,
                            Some(child_conversation_id),
                            Some(agent_type),
                            "the task ended in failure rather than being \
                             interrupted — resume only applies to a canceled or \
                             interrupted task. Start a new delegate_to_agent call \
                             to retry it.",
                        ))),
                    }
                }
            } else if let Some(r) = inner.running.get(&req.task_id) {
                if r.parent_connection_id != req.parent_connection_id {
                    MemGate::Refuse(Box::new(unknown_report(&req.task_id)))
                } else {
                    MemGate::Refuse(Box::new(not_resumable_report(
                        &req.task_id,
                        TaskStatus::Running,
                        Some(r.child_conversation_id),
                        Some(r.agent_type),
                        "the task is still running — resume only applies to a \
                         canceled or interrupted task. Poll it with \
                         get_delegation_status (or stop it with cancel_delegation) \
                         instead.",
                    )))
                }
            } else {
                let claimed = inner.resuming.insert(req.task_id.clone());
                MemGate::Proceed { claimed }
            }
        };
        let claimed = match gate {
            MemGate::Refuse(report) => {
                self.drop_inflight(inflight_id).await;
                return *report;
            }
            MemGate::Proceed { claimed } => claimed,
        };

        // Everything past the gate runs in a helper so this single exit point
        // can release the claim on EVERY path — success (the task is in
        // `running` by then, which re-arms the gate's normal refusals) and
        // failure alike. A `claimed: false` outcome belongs to the concurrent
        // resume that holds it; never released here.
        let claim_key = req.task_id.clone();
        let report = self
            .resume_delegation_gated(req, inflight_id, cfg, claimed)
            .await;
        if claimed {
            self.pending.inner.lock().await.resuming.remove(&claim_key);
        }
        report
    }

    /// The post-gate body of [`Self::resume_delegation`]: DB context + status
    /// gates, child re-spawn, continuation send, and registration. Split out so
    /// the caller can release the `resuming` claim at one exit point.
    async fn resume_delegation_gated(
        &self,
        req: ResumeDelegationRequest,
        inflight_id: u64,
        cfg: DelegationConfig,
        claimed: bool,
    ) -> DelegationTaskReport {
        // --- DB context + status gate -----------------------------------------
        // The DB row is authoritative for resumability even when the in-memory
        // cache still remembers the cancel: the user may have driven the child
        // session onward in the meantime, and resuming over that would be a new
        // iteration in disguise.
        let Some(ctx) = self
            .status_lookup
            .find_resume_context_by_call_id(&req.task_id)
            .await
        else {
            self.drop_inflight(inflight_id).await;
            return unknown_report(&req.task_id);
        };
        if ctx.parent_id != Some(req.parent_conversation_id) {
            self.drop_inflight(inflight_id).await;
            return unknown_report(&req.task_id);
        }
        // Deferred claim-contention refusal (see the gate): the caller owns the
        // task, so naming the in-flight resume leaks nothing.
        if !claimed {
            self.drop_inflight(inflight_id).await;
            return not_resumable_report(
                &req.task_id,
                ctx.status,
                Some(ctx.child_conversation_id),
                Some(ctx.agent_type),
                "a resume of this task is already in progress.",
            );
        }
        match ctx.status {
            // Recorded canceled — the canonical interrupted state.
            TaskStatus::Canceled => {}
            // `in_progress` row: a live connection means the child session is
            // genuinely active (either as somebody's running delegation or
            // driven by the user in a tab) — hands off. No live connection
            // means a hard crash stranded the row mid-run: the exact
            // "unexpected termination" this tool exists for.
            TaskStatus::Running => {
                if self
                    .spawner
                    .has_live_connection_for_conversation(ctx.child_conversation_id)
                    .await
                {
                    self.drop_inflight(inflight_id).await;
                    return not_resumable_report(
                        &req.task_id,
                        TaskStatus::Running,
                        Some(ctx.child_conversation_id),
                        Some(ctx.agent_type),
                        "the child session is currently active — resume only \
                         applies to a canceled or interrupted task.",
                    );
                }
            }
            TaskStatus::Completed => {
                self.drop_inflight(inflight_id).await;
                return not_resumable_report(
                    &req.task_id,
                    TaskStatus::Completed,
                    Some(ctx.child_conversation_id),
                    Some(ctx.agent_type),
                    "the task already completed — resume only applies to a \
                     canceled or interrupted task. Start a new delegate_to_agent \
                     call for follow-up work.",
                );
            }
            TaskStatus::Failed | TaskStatus::Unknown => {
                self.drop_inflight(inflight_id).await;
                return unknown_report(&req.task_id);
            }
        }
        let Some(external_id) = ctx.external_id.clone() else {
            self.drop_inflight(inflight_id).await;
            return not_resumable_report(
                &req.task_id,
                ctx.status,
                Some(ctx.child_conversation_id),
                Some(ctx.agent_type),
                "no agent session id was recorded for the child session, so \
                 there is nothing to load. Start a new delegate_to_agent call \
                 instead.",
            );
        };
        let Some(working_dir) = ctx.working_dir.clone() else {
            self.drop_inflight(inflight_id).await;
            return not_resumable_report(
                &req.task_id,
                ctx.status,
                Some(ctx.child_conversation_id),
                Some(ctx.agent_type),
                "the child session's project folder no longer exists. Start a \
                 new delegate_to_agent call in a valid directory instead.",
            );
        };

        // --- Re-spawn the child, resuming its agent session --------------------
        // Same per-agent defaults as a fresh spawn; no depth re-check — the
        // child row already exists at its chain position, resume adds no node.
        let (preferred_mode_id, preferred_config_values) = cfg
            .agent_defaults
            .get(&ctx.agent_type)
            .map(|d: &AgentDelegationDefaults| (d.mode_id.clone(), d.config_values.clone()))
            .unwrap_or((None, BTreeMap::new()));
        // Checkpoint #1: a parent cancel during the gates — nothing spawned yet.
        if self.take_inflight_cancel(inflight_id).await {
            return report_from_outcome(
                Some(req.task_id.clone()),
                Some(ctx.agent_type),
                &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                None,
            );
        }
        let spawned = match self
            .spawner
            .spawn_for_resume(
                &req.parent_connection_id,
                ctx.agent_type,
                Some(working_dir),
                &external_id,
                preferred_mode_id,
                preferred_config_values,
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                self.drop_inflight(inflight_id).await;
                return report_from_outcome(
                    Some(req.task_id.clone()),
                    Some(ctx.agent_type),
                    &DelegationOutcome::from_err(
                        DelegationError::SpawnFailed(format!("resume: {e}")),
                        Some(ctx.child_conversation_id),
                    ),
                    None,
                );
            }
        };
        let child_connection_id = spawned.connection_id.clone();
        // Checkpoint #2: a parent cancel during spawn — the connection exists
        // but no prompt went out; disconnect-only teardown. A DEDUP-REUSED
        // connection (the user has the child session open — see
        // `ResumedSpawn::reused`) is not ours to tear down: nothing was sent
        // on it, so abandoning the resume leaves it exactly as found.
        if self.take_inflight_cancel(inflight_id).await {
            if !spawned.reused {
                let _ = self.spawner.disconnect(&child_connection_id).await;
            }
            return report_from_outcome(
                Some(req.task_id.clone()),
                Some(ctx.agent_type),
                &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                None,
            );
        }

        let call_id = req.task_id.clone();
        tracing::Span::current().record("child_connection_id", child_connection_id.as_str());
        // Label for meta writes / the started event: the child row's title was
        // seeded from the original task text, so it is the closest thing to the
        // original `task_preview` the broker no longer holds.
        let task_preview = ctx
            .title
            .as_deref()
            .map(|t| truncate_on_char_boundary(t, TASK_PREVIEW_CAP))
            .unwrap_or_else(|| "(resumed delegation)".to_string());
        // The resumed run reuses the ORIGINAL parent-side tool_use_id persisted
        // on the row so the parent's original delegation card re-binds; a row
        // without one gets a synthetic id, which `write_meta_if_real` /
        // `emit_*_if_real` skip.
        let parent_tool_use_id = ctx
            .parent_tool_use_id
            .clone()
            .unwrap_or_else(|| format!("delegation-{}", uuid::Uuid::new_v4()));

        // Reserve BEFORE sending — identical race contract to
        // `start_delegation`: once the continuation prompt is dispatched, a
        // fast TurnComplete (`complete_call`, keyed by `call_id` — live again
        // because the row's `delegation_call_id` never changed) or an immediate
        // child failure (`cancel_by_child_connection`, keyed by connection) can
        // fire before the entry is parked; the reservation buffers either for
        // the disposition block to drain.
        self.pending
            .inner
            .lock()
            .await
            .reserve(&call_id, &child_connection_id);

        if let Err(e) = self
            .spawner
            .send_resume_prompt(
                &child_connection_id,
                build_resume_prompt(req.reason.as_deref()),
                ctx.folder_id,
                ctx.child_conversation_id,
            )
            .await
        {
            {
                let mut inner = self.pending.inner.lock().await;
                inner.unreserve(&call_id, &child_connection_id);
                inner.deregister_inflight(inflight_id);
            }
            // Tear the connection down iff WE spawned it. A fresh connection is
            // ours to disconnect even when the failed send got far enough to
            // bind it to the conversation (a post-link status-write / enqueue
            // failure) — skipping it there would leak a live, bound connection
            // that blocks every future resume behind the "currently active"
            // gate. A DEDUP-REUSED connection is the opposite: it belonged to
            // whoever was already driving it (the user's open tab, having
            // raced past the live-connection gate), so it is left alone. The
            // spawn-time `reused` fact is the only reliable discriminator — a
            // post-hoc liveness probe can't tell our own just-bound connection
            // from a foreign one.
            if !spawned.reused {
                let _ = self.spawner.disconnect(&child_connection_id).await;
            }
            return report_from_outcome(
                Some(call_id),
                Some(ctx.agent_type),
                &DelegationOutcome::from_err(
                    DelegationError::SpawnFailed(format!("resume send: {e}")),
                    Some(ctx.child_conversation_id),
                ),
                None,
            );
        }

        let started_at = Instant::now();

        // Re-mark the parent's original card in-flight and re-announce the live
        // delegation so the frontend re-binds the child sub-thread.
        self.write_meta_if_real(
            &req.parent_connection_id,
            &parent_tool_use_id,
            build_delegation_meta(
                "running",
                Some(&child_connection_id),
                Some(ctx.child_conversation_id),
                None,
                None,
                None,
                Some(&task_preview),
                Some(&call_id),
            ),
        )
        .await;
        self.emit_started_if_real(
            &req.parent_connection_id,
            &parent_tool_use_id,
            &child_connection_id,
            ctx.child_conversation_id,
            ctx.agent_type,
            &task_preview,
            &call_id,
        )
        .await;

        // --- Register running, or resolve a terminal that beat us --------------
        // Same atomic disposition as `start_delegation` (see the long comment
        // there), with one resume-specific addition: the STALE canceled cache
        // entry (if any) is removed inside this same lock — after this point the
        // task is either running again or freshly terminal, and either way the
        // old record must not shadow it (`classify_locked` consults `completed`
        // first) nor double-count the byte valve on re-insert.
        enum Disposition {
            ChildTerminal(DelegationOutcome),
            ParentCanceled,
            Running,
        }
        let setup_duration_ms = started_at.elapsed().as_millis() as u64;
        let disposition = {
            let mut inner = self.pending.inner.lock().await;
            inner.remove_completed(&call_id);
            let child_terminal: Option<(u64, DelegationOutcome)> =
                if let Some((stamp, outcome)) = inner.take_early_complete(&call_id) {
                    Some((stamp, outcome))
                } else {
                    inner
                        .take_early_cancel(&child_connection_id)
                        .map(|(stamp, reason)| {
                            (
                                stamp,
                                DelegationOutcome::from_err(
                                    DelegationError::Canceled { reason },
                                    Some(ctx.child_conversation_id),
                                ),
                            )
                        })
                };
            let parent_canceled_at = inner.inflight_canceled_at(inflight_id);
            inner.unreserve(&call_id, &child_connection_id);
            let record = |inner: &mut PendingInner, outcome: &DelegationOutcome| {
                inner.insert_completed(
                    &call_id,
                    build_completed(
                        &req.parent_connection_id,
                        ctx.child_conversation_id,
                        ctx.agent_type,
                        setup_duration_ms,
                        outcome,
                    ),
                );
            };
            match (child_terminal, parent_canceled_at) {
                (Some((child_stamp, outcome)), Some(cancel_stamp)) => {
                    inner.deregister_inflight(inflight_id);
                    if child_stamp < cancel_stamp {
                        record(&mut inner, &outcome);
                        Disposition::ChildTerminal(outcome)
                    } else {
                        record(
                            &mut inner,
                            &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                        );
                        Disposition::ParentCanceled
                    }
                }
                (Some((_, outcome)), None) => {
                    inner.deregister_inflight(inflight_id);
                    record(&mut inner, &outcome);
                    Disposition::ChildTerminal(outcome)
                }
                (None, Some(_)) => {
                    inner.deregister_inflight(inflight_id);
                    record(
                        &mut inner,
                        &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                    );
                    Disposition::ParentCanceled
                }
                (None, None) => {
                    inner.running.insert(
                        call_id.clone(),
                        RunningTask {
                            child_connection_id: child_connection_id.clone(),
                            child_conversation_id: ctx.child_conversation_id,
                            parent_connection_id: req.parent_connection_id.clone(),
                            parent_tool_use_id: parent_tool_use_id.clone(),
                            agent_type: ctx.agent_type,
                            task_preview: task_preview.clone(),
                            task_id: call_id.clone(),
                            external_handle: req.external_handle.clone(),
                            started_at,
                            last_surfaced_block: None,
                        },
                    );
                    inner.deregister_inflight(inflight_id);
                    Disposition::Running
                }
            }
        };

        match disposition {
            Disposition::ChildTerminal(outcome) => {
                self.finalize_delegation(
                    &req.parent_connection_id,
                    &parent_tool_use_id,
                    &child_connection_id,
                    ctx.child_conversation_id,
                    ctx.agent_type,
                    setup_duration_ms,
                    &outcome,
                    &task_preview,
                    &call_id,
                )
                .await;
                self.result_notify.notify_waiters();
                return report_from_outcome(
                    Some(call_id),
                    Some(ctx.agent_type),
                    &outcome,
                    Some(setup_duration_ms),
                );
            }
            Disposition::ParentCanceled => {
                self.write_meta_if_real(
                    &req.parent_connection_id,
                    &parent_tool_use_id,
                    build_delegation_meta(
                        "failed",
                        Some(&child_connection_id),
                        Some(ctx.child_conversation_id),
                        Some("canceled"),
                        None,
                        Some(setup_duration_ms),
                        Some(&task_preview),
                        Some(&call_id),
                    ),
                )
                .await;
                self.emit_completed_if_real(
                    &req.parent_connection_id,
                    &parent_tool_use_id,
                    &child_connection_id,
                    ctx.child_conversation_id,
                    ctx.agent_type,
                    DelegationResultSummary::Err {
                        error_code: "canceled".to_string(),
                    },
                )
                .await;
                let _ = self.spawner.cancel(&child_connection_id).await;
                let _ = self.spawner.disconnect(&child_connection_id).await;
                self.result_notify.notify_waiters();
                return report_from_outcome(
                    Some(call_id),
                    Some(ctx.agent_type),
                    &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                    Some(setup_duration_ms),
                );
            }
            Disposition::Running => {}
        }

        // Second pre-cancel check — a `notifications/cancelled` that landed
        // between the entry-side check and the registration above. Mirror of
        // `start_delegation`'s second check.
        if let Some(handle) = req.external_handle.as_deref() {
            if self.take_pre_canceled_handle(handle).await {
                let canceled_duration_ms = {
                    let mut inner = self.pending.inner.lock().await;
                    if inner.running.remove(&call_id).is_some() {
                        let outcome = canceled_outcome(
                            ctx.child_conversation_id,
                            "canceled before await",
                        );
                        let duration_ms = started_at.elapsed().as_millis() as u64;
                        inner.insert_completed(
                            &call_id,
                            build_completed(
                                &req.parent_connection_id,
                                ctx.child_conversation_id,
                                ctx.agent_type,
                                duration_ms,
                                &outcome,
                            ),
                        );
                        Some(duration_ms)
                    } else {
                        None
                    }
                };
                if let Some(duration_ms) = canceled_duration_ms {
                    self.write_meta_if_real(
                        &req.parent_connection_id,
                        &parent_tool_use_id,
                        build_delegation_meta(
                            "failed",
                            Some(&child_connection_id),
                            Some(ctx.child_conversation_id),
                            Some("canceled"),
                            None,
                            Some(duration_ms),
                            Some(&task_preview),
                            Some(&call_id),
                        ),
                    )
                    .await;
                    self.emit_completed_if_real(
                        &req.parent_connection_id,
                        &parent_tool_use_id,
                        &child_connection_id,
                        ctx.child_conversation_id,
                        ctx.agent_type,
                        DelegationResultSummary::Err {
                            error_code: "canceled".to_string(),
                        },
                    )
                    .await;
                    let _ = self.spawner.cancel(&child_connection_id).await;
                    let _ = self.spawner.disconnect(&child_connection_id).await;
                    self.result_notify.notify_waiters();
                    return report_from_outcome(
                        Some(call_id),
                        Some(ctx.agent_type),
                        &canceled_outcome(ctx.child_conversation_id, "canceled before await"),
                        Some(duration_ms),
                    );
                }
            }
        }

        resume_ack(call_id, ctx.child_conversation_id, ctx.agent_type)
    }

    /// Test-only shim preserving the old blocking `handle_request` contract over
    /// the async path: start the delegation, then block until it reaches a
    /// terminal state (driven by the test's `complete_call` / cancel), mapping
    /// the terminal report back to a `DelegationOutcome`. Keeps the broker's
    /// extensive setup-window race tests exercising the same lifecycle without
    /// each rewriting to the start/poll/collect shape.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn handle_request(&self, req: DelegationRequest) -> DelegationOutcome {
        let parent_connection_id = req.parent_connection_id.clone();
        let parent_conversation_id = Some(req.parent_conversation_id);
        let ack = self.start_delegation(req).await;
        let task_id = match ack.task_id.clone() {
            Some(id) => id,
            // Setup failed before a task existed — the ack itself is terminal.
            None => return report_to_outcome(&ack),
        };
        if ack.status != TaskStatus::Running {
            return report_to_outcome(&ack);
        }
        // Block until terminal via the long-poll path (re-issued so an
        // indefinitely-pending task in a test simply parks here, mirroring the
        // old unbounded `rx.await`).
        loop {
            let report = self
                .get_task_status(
                    &parent_connection_id,
                    parent_conversation_id,
                    &task_id,
                    StatusWait::Bounded(3_600_000),
                )
                .await;
            if report.status != TaskStatus::Running {
                return report_to_outcome(&report);
            }
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn peek_first_pending_call_id(&self) -> Option<String> {
        self.pending
            .inner
            .lock()
            .await
            .running
            .keys()
            .next()
            .cloned()
    }

    /// How many delegation calls are parked awaiting a child's `TurnComplete`
    /// right now. Surfaced by the codeg-mcp service-status indicator so the
    /// popover can say the service is actually carrying traffic, not just
    /// listening.
    pub async fn running_delegation_count(&self) -> usize {
        self.pending.inner.lock().await.running.len()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn pending_count(&self) -> usize {
        self.running_delegation_count().await
    }

    /// Count of cached completed results across all parents.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn completed_count(&self) -> usize {
        self.pending.inner.lock().await.completed.len()
    }

    /// Count of in-flight (registered-at-entry, not-yet-parked / not-yet-exited)
    /// `handle_request` setups. Should return to 0 on every exit path.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn inflight_count(&self) -> usize {
        self.pending.inner.lock().await.inflight.len()
    }

    /// Count of in-setup (reserved, not-yet-parked) delegations. Each holds one
    /// child and one call_id, so this counts both.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn reserved_child_count(&self) -> usize {
        self.pending.inner.lock().await.setups.len()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn reserved_call_count(&self) -> usize {
        self.pending.inner.lock().await.setups.len()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn early_cancel_count(&self) -> usize {
        self.pending.inner.lock().await.early_cancels.len()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn early_complete_count(&self) -> usize {
        self.pending.inner.lock().await.early_completes.len()
    }

    /// First reserved (mid-setup) `call_id`, if any — lets a test resolve a
    /// delegation via `complete_call` while it's pinned in the reserve→park
    /// window (its entry isn't parked yet, so `peek_first_pending_call_id`
    /// can't see it).
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn peek_reserved_call_id(&self) -> Option<String> {
        self.pending
            .inner
            .lock()
            .await
            .setups
            .keys()
            .next()
            .cloned()
    }
}

/// `ConversationDepthLookup` over the live `AppDatabase`. Used by the
/// production wiring; tests use the in-module `MockDepth`.
pub struct DbDepthLookup {
    pub db: Arc<crate::db::AppDatabase>,
}

#[async_trait]
impl ConversationDepthLookup for DbDepthLookup {
    async fn parent_of(&self, conversation_id: i32) -> Result<Option<i32>, DelegationError> {
        use sea_orm::EntityTrait;
        let row = crate::db::entities::conversation::Entity::find_by_id(conversation_id)
            .one(&self.db.conn)
            .await
            .map_err(|e| DelegationError::SubagentRuntimeError(format!("db: {e}")))?;
        Ok(row.and_then(|r| r.parent_id))
    }
}

/// `ChildStatusLookup` over the live `AppDatabase`. Recovers a delegation
/// task's terminal status (NOT its text — child output isn't in codeg's DB)
/// from the child conversation row once its in-memory result was evicted.
pub struct DbChildStatusLookup {
    pub db: Arc<crate::db::AppDatabase>,
}

/// Map the serialized `ConversationStatus` string on a child conversation row
/// onto the delegation [`TaskStatus`] vocabulary. Shared by the status fallback
/// and the resume-context lookup so the two can't drift.
fn task_status_from_row(status: &str) -> TaskStatus {
    match status {
        "in_progress" => TaskStatus::Running,
        "pending_review" | "completed" => TaskStatus::Completed,
        "cancelled" => TaskStatus::Canceled,
        _ => TaskStatus::Unknown,
    }
}

#[async_trait]
impl ChildStatusLookup for DbChildStatusLookup {
    async fn find_by_call_id(&self, call_id: &str) -> Option<ChildStatusRecord> {
        let summary = crate::db::service::conversation_service::get_by_delegation_call_id(
            &self.db.conn,
            call_id,
        )
        .await
        .ok()
        .flatten()?;
        Some(ChildStatusRecord {
            child_conversation_id: summary.id,
            status: task_status_from_row(&summary.status),
            agent_type: summary.agent_type,
            parent_id: summary.parent_id,
        })
    }

    async fn find_resume_context_by_call_id(&self, call_id: &str) -> Option<ChildResumeContext> {
        let summary = crate::db::service::conversation_service::get_by_delegation_call_id(
            &self.db.conn,
            call_id,
        )
        .await
        .ok()
        .flatten()?;
        // The row's folder supplies the resumed connection's working_dir. A
        // deleted folder yields `working_dir: None`, which the broker turns
        // into an explicit "folder no longer exists" refusal.
        let working_dir =
            crate::db::service::folder_service::get_folder_by_id(&self.db.conn, summary.folder_id)
                .await
                .ok()
                .flatten()
                .map(|f| f.path);
        Some(ChildResumeContext {
            child_conversation_id: summary.id,
            status: task_status_from_row(&summary.status),
            agent_type: summary.agent_type,
            parent_id: summary.parent_id,
            parent_tool_use_id: summary.parent_tool_use_id,
            external_id: summary.external_id,
            folder_id: summary.folder_id,
            working_dir,
            title: summary.title,
        })
    }
}

#[cfg(test)]
mod tests;
