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

// Architecture submodules — the public surface `broker::…` is unchanged:
// glob re-exports below keep every previously-visible name reachable.
mod cancel;
mod lifecycle;
mod lookups;
mod pending;
mod reports;
mod resume;
mod status;
mod tool_calls;

pub use self::lookups::*;
pub(crate) use self::pending::*;
pub(crate) use self::reports::*;
pub use self::resume::*;
pub use self::status::*;
pub use self::tool_calls::*;


/// Default per-parent byte budget for cached completed-task result text. The
/// completed-cache lets `get_delegation_status` / `cancel_delegation` return a
/// finished task's result after the lifecycle resolved it; once a parent's
/// retained result text exceeds this budget the OLDEST results are FIFO-evicted
/// (evicted tasks fall back to the DB status lookup, which carries status only).
/// This is the seed value baked into `DelegationConfig::default()`; the live
/// value is user-configurable from the settings page (in MB) and `0` disables
/// eviction entirely. See `PendingInner::completed_cap_bytes`.
const DEFAULT_COMPLETED_CACHE_CAP_BYTES: usize = 512 * 1024 * 1024;


/// Lookup the `parent_id` for a conversation. Abstracted so the broker can be
/// unit-tested against an in-memory chain without touching SeaORM.
#[async_trait]
pub trait ConversationDepthLookup: Send + Sync {
    async fn parent_of(&self, conversation_id: i32) -> Result<Option<i32>, DelegationError>;
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


/// How long an already-reported blocking prompt stays suppressed before a
/// status long-poll will surface it again. Two jobs: it keeps a `wait_ms: 0`
/// caller from spinning on a block it has already been told about, and it
/// bounds the damage when a report is generated but never delivered (see
/// [`RunningTask::last_surfaced_block`]) — a lost report costs a delayed notice
/// instead of a permanent stall. Long relative to a human noticing a prompt,
/// short relative to walking away from the machine.
const BLOCK_RESURFACE_INTERVAL: Duration = Duration::from_secs(300);


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

#[cfg(test)]
mod tests;
