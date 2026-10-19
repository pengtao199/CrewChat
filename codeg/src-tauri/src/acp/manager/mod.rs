use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ActiveValue::Set, DatabaseConnection, EntityTrait,
    TransactionTrait,
};

use crate::acp::connection::{
    spawn_agent_connection, AgentConnection, ConnectionCommand, GoalControlAction, SteerOutcome,
};
use crate::acp::agent_mentions::strip_route_separator_from_prompt;
use crate::acp::error::AcpError;
use crate::acp::feedback::{
    bounded_feedback_batch, FeedbackItem, FeedbackStatus, PendingFeedback,
    SessionFeedbackAccess, MAX_FEEDBACK_CHARS, MAX_FEEDBACK_RESPONSE_BYTES,
};
use crate::acp::plan_approval::{
    PlanApprovalAnswer, RegisteredPlanApproval, SessionPlanApprovalAccess,
};
use crate::acp::question::{
    build_outcome, QuestionAnswer, QuestionOutcome, QuestionSpec, RegisteredQuestion,
    SessionQuestionAccess,
};
use crate::acp::terminal_runtime::TerminalShellRuntimeConfig;
use crate::acp::types::{
    AcpEvent, AgentOptionsSnapshot, ConfigStaleKind, ConnectionInfo, ConnectionStatus,
    ForkResultInfo, PromptCapabilitiesInfo, PromptInputBlock,
};
use crate::db::entities::conversation::{self, ConversationKind, ConversationStatus};
use crate::db::service::conversation_service;
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::web::event_bridge::{emit_with_state, emit_with_state_gated, EventEmitter};

/// Cap on the number of prompt-text chars kept in the `user_prompt_sent`
/// preview. Past this, `truncate_str` keeps this many chars and appends a short
/// `...` marker (so the rendered string can be a few chars longer). Bounds the
/// event payload so a large paste can't bloat the ring buffer, the per-channel
/// IM message, or the webhook body.
const USER_PROMPT_PREVIEW_MAX_CHARS: usize = 500;

/// Grace window `disconnect_all` waits after firing every `Disconnect` before
/// hard-killing surviving agent process trees. Long enough for a driver thread
/// to unwind and run its own post-loop cleanup (delegation/question/plan-approval
/// reclaim), short enough not to stall a quit noticeably. It is NOT there to
/// make the agent's death gentler — the graceful path ends in the same
/// `kill_tree`.
const DISCONNECT_ALL_GRACE: Duration = Duration::from_millis(500);

/// How long the polite signal gets before [`kill_tree_and_wait`] escalates to
/// `SIGKILL`. Counted from the first signal, which itself only lands after
/// [`DISCONNECT_ALL_GRACE`] of graceful shutdown, so anything still here has
/// already had a second to leave of its own accord.
const ESCALATE_TO_SIGKILL_AFTER: Duration = Duration::from_millis(500);

/// How often that sweep re-checks while it waits.
const CONFIRM_GONE_POLL: Duration = Duration::from_millis(50);

/// How long that sweep waits after `SIGKILL` for the reap to land. `kill(2)`
/// returns once the signal is queued, not once the target has been descheduled,
/// so on another core it can still be executing — briefly. Bounded tightly
/// because the wait can also end in no answer at all: a child that has already
/// exited but that nobody has reaped yet never zeroes its cell.
const SIGKILL_SETTLE: Duration = Duration::from_millis(200);

/// What one pass of [`kill_tree_pass`] established about the target itself.
#[derive(Debug, PartialEq, Eq)]
enum KillPass {
    /// The OS says there is no such process — the only report that positively
    /// means "not running".
    AlreadyGone,
    /// The signal was delivered to the target.
    Signalled,
    /// Neither could be established.
    Failed,
}

/// One signal pass over the process tree rooted at `pid`, reporting what
/// happened to the target itself (its descendants are killed either way).
///
/// `signal` is a `kill_tree` signal name; Windows ignores it and terminates
/// unconditionally. Blocking — call from `spawn_blocking`.
fn kill_tree_pass(pid: u32, signal: &str) -> KillPass {
    let config = kill_tree::Config {
        signal: signal.to_string(),
        ..Default::default()
    };
    match kill_tree::blocking::kill_tree_with_config(pid, &config) {
        Ok(outputs) => outputs
            .iter()
            .find_map(|o| match o {
                kill_tree::Output::MaybeAlreadyTerminated { process_id, .. }
                    if *process_id == pid =>
                {
                    Some(KillPass::AlreadyGone)
                }
                kill_tree::Output::Killed { process_id, .. } if *process_id == pid => {
                    Some(KillPass::Signalled)
                }
                _ => None,
            })
            .unwrap_or(KillPass::Failed),
        Err(e) => {
            tracing::debug!("[ACP] kill_tree pid={pid} signal={signal}: {e}");
            KillPass::Failed
        }
    }
}

/// Wait for `on_exit` to zero a connection's pid cell. `true` if it did within
/// roughly `budget` — the check follows the sleep, so the transition it sees
/// can be up to one poll late. Costs nothing per poll: no signal, no
/// process-table walk.
fn wait_for_reap(cell: &std::sync::atomic::AtomicU32, budget: Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < budget {
        std::thread::sleep(CONFIRM_GONE_POLL);
        if cell.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            return true;
        }
    }
    false
}

/// Kill the process tree behind a connection's live pid cell and wait for the
/// target to stop running. Two signals at most: `SIGTERM`, then `SIGKILL` if
/// that was not enough.
///
/// `kill_tree` returns as soon as the signal is delivered, and its default
/// `SIGTERM` is both catchable and ignorable, so "the call returned" is not
/// "the agent is finished". That does not matter at quit — the OS cleans up
/// whatever is left — but it does for a caller that is about to touch state the
/// agent also writes (see [`ConnectionManager::disconnect_by_agent_type`]). So:
/// signal, give it the grace it deserves, then escalate.
///
/// Between the signals it waits on the pid CELL rather than re-signalling.
/// A second `SIGTERM` buys nothing — the first was delivered — while each extra
/// pass re-walks the whole process table and re-aims at a pid NUMBER that, once
/// the target exits, the OS is free to hand to something else.
///
/// The pid is loaded from the cell HERE, at the last possible moment, for the
/// reasons [`ConnectionManager::disconnect_all`] spells out. A cell reading
/// zero needs nothing: `on_exit` only zeroes it on a real reap, and a child
/// that never spawned has nothing to kill.
///
/// What `true` means, exactly, weakest case first:
/// - `SIGKILL` was delivered and [`SIGKILL_SETTLE`] passed without the reap
///   landing. `SIGKILL` cannot be caught, ignored or blocked, so the process
///   cannot run userspace code again; it is either unreaped, or stuck in an
///   uninterruptible syscall and will die on the way out of it. An in-flight
///   write already issued to the kernel may still complete.
/// - Or the reap landed, or the OS reported no such process. Then it is gone
///   outright.
///
/// `false` means the ESCALATION did not land: the `SIGKILL` pass came back with
/// neither a delivery nor a no-such-process. An earlier `SIGTERM` may well have
/// been delivered — see the call site for what is done about it.
///
/// One hazard survives all of this, and cannot be closed from here: between the
/// target exiting and its cell being zeroed, the pid NUMBER may be recycled, and
/// the escalation would then signal whatever now holds it. It is the same
/// exposure `disconnect_all` accepts — narrowed here to a single signal, where
/// re-probing in the wait loop would have spent one per poll — and closing it
/// properly needs a handle rather than a number: a pidfd, a process group, a job
/// object, all of them decided at spawn time.
///
/// Note the asymmetry: "the pid is out of the process table" is deliberately
/// NOT the bar, because it is not waitable. A child that has already exited but
/// has not been reaped yet answers signals exactly like a live one, so a
/// wait-for-the-pid-to-vanish loop would burn its whole window on a process
/// that died instantly.
///
/// Scope: this tracks the ROOT. Each pass re-walks and signals the whole tree,
/// but a descendant that ignores `SIGTERM` and outlives its parent is
/// reparented out of that tree and left to the OS — the same limit
/// `disconnect_all` has always had. Fine for the sign-out this exists for: the
/// credential lives in the agent process itself, not in the MCP servers it
/// starts. Closing it properly would mean giving every agent a process group or
/// a job object at spawn time.
///
/// Blocking — call from `spawn_blocking`.
fn kill_tree_and_wait(cell: &std::sync::atomic::AtomicU32) -> bool {
    let pid = cell.load(std::sync::atomic::Ordering::SeqCst);
    if pid == 0 {
        return true;
    }
    if kill_tree_pass(pid, "SIGTERM") == KillPass::AlreadyGone {
        return true;
    }
    if wait_for_reap(cell, ESCALATE_TO_SIGKILL_AFTER) {
        return true;
    }

    if kill_tree_pass(pid, "SIGKILL") == KillPass::Failed {
        // Neither gone nor reachable: someone else owns that pid now (recycled
        // onto another user's process, say), so it is not ours to wait for.
        // Reported, not acted on — retrying would not change the answer.
        tracing::warn!("[ACP] pid={pid} could not be signalled or confirmed gone");
        return false;
    }
    wait_for_reap(cell, SIGKILL_SETTLE);
    true
}

/// True for ids in the parsers' turn-id namespace (`turn-<digits>`), which every
/// parser assigns via `format!("turn-{}", n)`. A broadcast `message_id` must
/// never land here: it would collide with a persisted transcript turn id and let
/// id-keyed cross-client dedup suppress or hide a prompt. Used to reject an
/// untrusted client-supplied `message_id` of that shape.
fn is_reserved_turn_id(id: &str) -> bool {
    matches!(id.strip_prefix("turn-"), Some(rest)
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Build the bounded preview string for a `user_prompt_sent` notification from
/// the `Text` blocks of a user prompt. Joins the (trimmed, non-empty) text
/// blocks with a space and caps the kept text at `USER_PROMPT_PREVIEW_MAX_CHARS`
/// chars (a `...` marker is appended past the cap). Returns `None` when the
/// prompt carries no text (e.g. image-only) — the notification fires for text
/// messages only.
fn user_prompt_text_preview(blocks: &[PromptInputBlock]) -> Option<String> {
    let joined = blocks
        .iter()
        .filter_map(|b| match b {
            PromptInputBlock::Text { text } => {
                let t = text.trim();
                (!t.is_empty()).then_some(t)
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(crate::parsers::truncate_str(
            trimmed,
            USER_PROMPT_PREVIEW_MAX_CHARS,
        ))
    }
}

/// Seed title for a freshly-created delegation child row, derived from the
/// delegating prompt's text blocks (the sub-agent's task). Uses the parser's own
/// `title_from_user_text` (folds reference links, caps at 100 chars) so the value
/// matches what `refresh_auto_title` would later compute from that same first
/// turn — the conditional UPDATE then sees no change and doesn't churn. Kept
/// unlocked by the caller so an AI-generated title can still replace it later.
///
/// A prompt with no prose at all — one dropped-in file and nothing else — is
/// named after what it carries instead. That row would otherwise read
/// "Untitled" forever: for an agent with no store parser, ACP has no title
/// channel and the history parse honestly reports no title. This value only
/// ever reaches `seed_auto_title_if_empty` / a fresh row's `title`, never
/// `refresh_auto_title`, so it cannot displace a name the agent published.
fn delegation_child_title_seed(blocks: &[PromptInputBlock]) -> Option<String> {
    let joined = blocks
        .iter()
        .filter_map(|b| match b {
            PromptInputBlock::Text { text } => {
                let t = text.trim();
                (!t.is_empty()).then_some(t)
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = joined.trim();
    if !trimmed.is_empty() {
        return Some(crate::parsers::title_from_user_text(trimmed));
    }
    crate::acp::types::attachment_names_from_prompt(blocks)
        .map(|names| crate::parsers::title_from_user_text(&names))
}

/// Composite key identifying a logical agent session for spawn-time dedup.
/// Two `acp_connect` calls with the same triple race for the same `Mutex`,
/// so the second one observes the first's freshly-spawned connection in
/// `find_connection_for_reuse` instead of starting a duplicate process.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SpawnDedupKey {
    agent_type: AgentType,
    working_dir: Option<PathBuf>,
    session_id: String,
}

/// Default upper bound on how long `spawn_agent` will hold the per-session
/// dedup lock waiting for `SessionStarted`. Picked to comfortably cover
/// cold-start agents (claude-code/codex warm: <2s; npx-fetched cold: 10–30s)
/// without deadlocking the next concurrent acp_connect when an agent is
/// genuinely broken.
pub(crate) const SPAWN_HANDSHAKE_TIMEOUT_SECS: u64 = 60;

/// Whether the turn a steer was admitted against is no longer the turn now in
/// flight — the guard `submit_feedback_native` applies across attachment
/// hydration, the one await between admission and the enqueue.
///
/// Both halves are needed. `turn_in_flight` alone cannot see "turn N ended and
/// N+1 started while we hydrated" — it reads true both times.
/// `SessionState.turns_completed` closes exactly that: it moves only on
/// `TurnComplete`, so it is stable for a turn's whole life and differs across
/// turns, whatever the new turn did to the flag. It is also independent of
/// whether the turn ever published a user message, which
/// `pending_user_message_started_at` is not (`user_message` is `None` for
/// delegation children and unbound conversations, so those turns would have
/// carried no identity at all).
fn steered_turn_changed(
    admitted_turns_completed: u64,
    now_in_flight: bool,
    now_turns_completed: u64,
) -> bool {
    !now_in_flight || now_turns_completed != admitted_turns_completed
}

/// Read the spawn-handshake timeout from `CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS`,
/// falling back to `SPAWN_HANDSHAKE_TIMEOUT_SECS`. Returns the configured
/// `Duration`. Tests can construct the manager with a custom value via
/// `with_spawn_handshake_timeout` instead of mutating env.
fn spawn_handshake_timeout_from_env() -> Duration {
    let secs = std::env::var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(SPAWN_HANDSHAKE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Outcome of the `spawn_agent` dedup wait. Logged so production can audit
/// how often the timeout fires vs. the agent handshake completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandshakeWaitOutcome {
    /// `SessionStarted` applied; `external_id` is now set on the state.
    Ready,
    /// Sender was dropped before SessionStarted fired (typically the
    /// connection died during init — `run_connection` returned Err).
    Aborted,
    /// Timeout elapsed before either of the above. Releases the dedup lock
    /// so the next caller can proceed; the slow agent is no worse off.
    TimedOut,
}

impl HandshakeWaitOutcome {
    fn as_str(self) -> &'static str {
        match self {
            HandshakeWaitOutcome::Ready => "ready",
            HandshakeWaitOutcome::Aborted => "aborted",
            HandshakeWaitOutcome::TimedOut => "timeout",
        }
    }
}

/// Wait for the spawn-time `SessionStarted` signal, bounded by `timeout`.
/// Extracted so the outcome enum can be unit-tested without spawning a
/// real agent process.
async fn wait_for_session_started(
    rx: tokio::sync::oneshot::Receiver<()>,
    timeout: Duration,
) -> (HandshakeWaitOutcome, Duration) {
    let start = std::time::Instant::now();
    let outcome = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(())) => HandshakeWaitOutcome::Ready,
        Ok(Err(_)) => HandshakeWaitOutcome::Aborted,
        Err(_) => HandshakeWaitOutcome::TimedOut,
    };
    (outcome, start.elapsed())
}

/// A connection that has left the map while its process may still be running.
struct DrainingChild {
    agent: AgentType,
    /// The connection's live pid cell. `on_spawn` publishes the pid and
    /// `on_exit` zeroes it on a real reap.
    pid: Arc<std::sync::atomic::AtomicU32>,
    parked_at: std::time::Instant,
}

type DrainingChildren = Vec<DrainingChild>;

/// How long a child whose pid reads zero is still treated as possibly running.
///
/// Zero is ambiguous: it means "reaped" AND "not spawned yet". A connection
/// torn down mid-spawn can publish a live pid moments later, so dropping it
/// immediately would hide a real writer. The grace resolves the ambiguity in
/// the safe direction — a false positive only downgrades a restore to the side
/// location, while a false negative unlinks a file an agent is writing.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Drop entries that are provably finished: pid back to zero and long enough
/// ago that it cannot be a spawn still in flight.
fn prune_reaped(draining: &mut DrainingChildren) {
    draining.retain(|c| {
        c.pid.load(std::sync::atomic::Ordering::SeqCst) != 0
            || c.parked_at.elapsed() < DRAIN_GRACE
    });
}

pub struct ConnectionManager {
    pub(crate) connections: Arc<Mutex<HashMap<String, AgentConnection>>>,
    /// Connections whose teardown was requested but whose child process has
    /// not been reaped yet.
    ///
    /// `disconnect` drops the map entry immediately and only then signals the
    /// child, so without this an agent that is still exiting — and still able
    /// to append to its transcript — is invisible to the backup restore gate,
    /// which would then unlink files it is still writing.
    ///
    /// Each entry holds the connection's live `child_pid` cell. `on_exit`
    /// zeroes it on a REAL reap; a connection that merely ended keeps its pid,
    /// because `ChildGuard::drop` signals the tree without waiting. So
    /// "non-zero" is exactly the codebase's existing definition of "this
    /// process may still be running", the same one the shutdown backstop
    /// relies on. Pruned on every push and every read, so it stays small.
    draining: Arc<Mutex<DrainingChildren>>,
    /// Read-held for the whole of `spawn_agent`, write-held while a backup
    /// restore writes transcripts back to the agents' own directories.
    ///
    /// Enumerating live connections and then writing is a check-then-use with
    /// a gap wide enough to drive through: staging a large archive takes
    /// minutes, and automations, the work-task engine and MCP delegation all
    /// start connections without a user clicking anything. Taking the write
    /// lock BEFORE enumerating closes it — no connection can appear between
    /// the count and the writes.
    external_restore_lock: Arc<tokio::sync::RwLock<()>>,
    /// Per-(agent, working_dir, session_id) async mutex. Held across the
    /// dedup-lookup + spawn + SessionStarted-wait critical section so two
    /// concurrent `spawn_agent` calls for the same logical session can't
    /// both miss dedup during the handshake window. Entries persist for
    /// process lifetime — bounded by the number of distinct sessions ever
    /// connected.
    spawn_locks: Arc<Mutex<HashMap<SpawnDedupKey, Arc<Mutex<()>>>>>,
    /// Bound on how long `spawn_agent` waits for the agent's handshake
    /// before releasing the dedup lock. Configurable per-instance for
    /// tests; in production initialized from env via
    /// `spawn_handshake_timeout_from_env`.
    spawn_handshake_timeout: Duration,
    /// Shared General Settings shell used by ACP terminal fallbacks. Cloned
    /// into each connection runtime so a setting update applies to existing
    /// model sessions as well as newly spawned ones.
    terminal_shell_config: TerminalShellRuntimeConfig,
    /// Delegation broker + token registry + UDS path installed during app
    /// bootstrap (`install_delegation`). When present, `spawn_agent` propagates
    /// the injection to `spawn_agent_connection`, which makes
    /// `codeg-mcp` appear in the agent's MCP server list during ACP
    /// init. `Arc<OnceLock>` so the inner `Self` cloned from `clone_ref` sees
    /// the install too — the lock is set once at startup and never mutated.
    delegation_injection: Arc<std::sync::OnceLock<crate::acp::connection::DelegationInjection>>,
    /// Chat-channel manager installed during bootstrap so live title writes
    /// can sync Telegram topic names without threading the manager through
    /// every `send_prompt_linked` caller. Optional in tests.
    chat_channel: Arc<std::sync::OnceLock<crate::chat_channel::manager::ChatChannelManager>>,
    /// Per-agent-type serialization for `probe_agent_options`. Without
    /// this, rapid agent-tab clicks in the settings UI would fan out one
    /// real CLI process per click — each one running up to 60s. The
    /// mutex bounds concurrent probes for the same agent_type to one;
    /// different agent_types remain parallel.
    probe_locks: Arc<Mutex<HashMap<AgentType, Arc<tokio::sync::Mutex<()>>>>>,
    /// In-flight `ask_user_question` calls awaiting the user's answer, keyed by
    /// the globally-unique `question_id`. The listener parks on the receiver;
    /// the answer / cancel path resolves (and removes) the matching sender.
    /// Shared across `clone_ref` clones so the listener-facing
    /// `register_question` and the command-facing `answer_question` touch the
    /// same map. Size tracks live concurrency (the agent is blocked per ask) —
    /// no cap, no cumulative growth; entries are removed on answer / cancel /
    /// connection teardown.
    pending_questions: Arc<Mutex<HashMap<String, PendingQuestionEntry>>>,
    /// In-flight Grok `exit_plan_mode` approvals awaiting the user's decision,
    /// keyed by the globally-unique `approval_id`. The connection's ext handler
    /// parks on the receiver; the answer / cancel path resolves (and removes) the
    /// matching sender. Shared across `clone_ref` clones so the connection-facing
    /// `register_plan_approval` and the command-facing `answer_plan_approval`
    /// touch the same map. At most one per connection (the agent is blocked in
    /// its `exit_plan_mode` call) — no cap, no cumulative growth.
    pending_plan_approvals: Arc<Mutex<HashMap<String, PendingPlanApprovalEntry>>>,
}

/// A parked `ask_user_question` awaiting its answer. The `sender` resolves the
/// blocked listener round-trip; `questions` is retained so `answer_question` can
/// build the self-describing outcome without a `SessionState` read (race-free).
struct PendingQuestionEntry {
    parent_connection_id: String,
    questions: Vec<QuestionSpec>,
    sender: tokio::sync::oneshot::Sender<QuestionOutcome>,
}

/// A parked Grok `exit_plan_mode` approval awaiting the user's decision. The
/// `sender` resolves the blocked ext-request round-trip in the connection's
/// handler. `parent_connection_id` routes the resolved event + answer.
struct PendingPlanApprovalEntry {
    parent_connection_id: String,
    sender: tokio::sync::oneshot::Sender<PlanApprovalAnswer>,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            external_restore_lock: Arc::new(tokio::sync::RwLock::new(())),
            draining: Arc::new(Mutex::new(Vec::new())),
            spawn_locks: Arc::new(Mutex::new(HashMap::new())),
            spawn_handshake_timeout: spawn_handshake_timeout_from_env(),
            terminal_shell_config: TerminalShellRuntimeConfig::new(),
            delegation_injection: Arc::new(std::sync::OnceLock::new()),
            chat_channel: Arc::new(std::sync::OnceLock::new()),
            probe_locks: Arc::new(Mutex::new(HashMap::new())),
            pending_questions: Arc::new(Mutex::new(HashMap::new())),
            pending_plan_approvals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns a shallow clone sharing the same underlying connection map.
    pub fn clone_ref(&self) -> Self {
        Self {
            connections: self.connections.clone(),
            external_restore_lock: self.external_restore_lock.clone(),
            draining: self.draining.clone(),
            spawn_locks: self.spawn_locks.clone(),
            spawn_handshake_timeout: self.spawn_handshake_timeout,
            terminal_shell_config: self.terminal_shell_config.clone(),
            delegation_injection: self.delegation_injection.clone(),
            chat_channel: self.chat_channel.clone(),
            probe_locks: self.probe_locks.clone(),
            pending_questions: self.pending_questions.clone(),
            pending_plan_approvals: self.pending_plan_approvals.clone(),
        }
    }

    /// Set the delegation injection context exactly once during bootstrap.
    /// Calling twice is a no-op — protects against accidental re-init in
    /// the unlikely event a second `build_delegation_stack` runs.
    pub fn install_delegation(&self, injection: crate::acp::connection::DelegationInjection) {
        let _ = self.delegation_injection.set(injection);
    }

    /// Install the chat-channel manager exactly once during bootstrap so
    /// live title writes can sync bound forum topics. Calling twice is a
    /// no-op. Tests leave this unset and skip the remote sync.
    pub fn install_chat_channel(
        &self,
        manager: crate::chat_channel::manager::ChatChannelManager,
    ) {
        let _ = self.chat_channel.set(manager);
    }

    pub(crate) fn chat_channel(
        &self,
    ) -> Option<crate::chat_channel::manager::ChatChannelManager> {
        self.chat_channel.get().map(|c| c.clone_ref())
    }

    fn delegation_snapshot(&self) -> Option<crate::acp::connection::DelegationInjection> {
        self.delegation_injection.get().cloned()
    }

    /// Returns the shared terminal-shell setting consumed by ACP terminal
    /// runtimes. Keeping the handle shared makes saves apply immediately to
    /// connections that are already running.
    pub fn terminal_shell_config(&self) -> TerminalShellRuntimeConfig {
        self.terminal_shell_config.clone()
    }

    /// Test-only constructor that overrides the spawn-handshake timeout.
    /// Production code should use `new()`.
    #[cfg(test)]
    fn with_spawn_handshake_timeout(timeout: Duration) -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            external_restore_lock: Arc::new(tokio::sync::RwLock::new(())),
            draining: Arc::new(Mutex::new(Vec::new())),
            spawn_locks: Arc::new(Mutex::new(HashMap::new())),
            spawn_handshake_timeout: timeout,
            terminal_shell_config: TerminalShellRuntimeConfig::new(),
            delegation_injection: Arc::new(std::sync::OnceLock::new()),
            chat_channel: Arc::new(std::sync::OnceLock::new()),
            probe_locks: Arc::new(Mutex::new(HashMap::new())),
            pending_questions: Arc::new(Mutex::new(HashMap::new())),
            pending_plan_approvals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Insert a synthetic `AgentConnection` for tests that need to exercise
    /// downstream code (attach, event broadcast, conversation linking)
    /// without spawning a real agent process. The returned connection is
    /// marked `Connected` and has a dropped `cmd_tx` receiver, so any
    /// attempt to send a prompt resolves to `ProcessExited` — fine for
    /// tests asserting on event-bus or session-state behavior.
    ///
    /// Gated behind `cfg(test)` (in-crate unit tests) and the `test-utils`
    /// feature (integration tests in `tests/*.rs`); the item is physically
    /// uncompiled in release builds so no production caller can reach it.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn insert_test_connection(
        &self,
        id: &str,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
        emitter: EventEmitter,
    ) {
        use crate::acp::session_state::SessionState;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut state = SessionState::new(
            id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: id.to_string(),
            agent_type,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(tokio::sync::RwLock::new(state)),
            emitter,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_fingerprint: String::new(),
            last_observed_fingerprint: String::new(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mut map = self.connections.lock().await;
        map.insert(id.to_string(), conn);
    }

    /// As [`insert_test_connection`], but keeps the command receiver ALIVE and
    /// returns it, so `send_prompt` can reach the concurrency gate (a dropped
    /// receiver fails `reserve()` with `ProcessExited` BEFORE the gate check,
    /// making the `TurnInProgress` branch untestable). Hold the returned
    /// receiver for the test's duration; drop it to simulate the process dying.
    ///
    /// Gated identically to [`insert_test_connection`] so it never compiles into
    /// a release build.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn insert_test_connection_live(
        &self,
        id: &str,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
        emitter: EventEmitter,
    ) -> tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand> {
        use crate::acp::session_state::SessionState;
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut state = SessionState::new(
            id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: id.to_string(),
            agent_type,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(tokio::sync::RwLock::new(state)),
            emitter,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_fingerprint: String::new(),
            last_observed_fingerprint: String::new(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        self.connections.lock().await.insert(id.to_string(), conn);
        rx
    }

    #[allow(clippy::too_many_arguments)]
    // `role` is the [crewchat] member-role context (Phases 2.3 / 3.1); every
    // non-delegation caller passes `None`.
    pub async fn spawn_agent(
        &self,
        agent_type: AgentType,
        working_dir: Option<String>,
        session_id: Option<String>,
        runtime_env: BTreeMap<String, String>,
        owner_window_label: String,
        emitter: EventEmitter,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        role: Option<crate::acp::delegation::spawner::RoleSpawnContext>,
    ) -> Result<String, AcpError> {
        // [crewchat] The session's ACP config values are exactly what the
        // caller asked for (`agent_defaults` picks, or a delegation's explicit
        // model request). The Agent Host recipe deliberately contributes NO
        // config options — see `agent_host`: claude cannot switch models at
        // runtime, and Phase 3.1's isolated role home is what gives a member its
        // model at process start.
        let role_mcp = role
            .as_ref()
            .map(|role| role.mcp.clone())
            .filter(|mcp| !mcp.is_empty());
        // Held for the whole establishment. A restore writing back to the
        // agents' own directories takes the write side, so it can never see an
        // empty connection list and then have one appear underneath it. Not
        // re-entrant: nothing reachable from here calls `spawn_agent` again.
        let _restore_guard = self.external_restore_lock.read().await;

        // Connection dedup: when resuming an agent session (session_id is
        // Some), look for a live AgentConnection that already represents
        // the same external session in the same working_dir for the same
        // agent_type and is not torn down. If found, reuse it instead of
        // spawning a fresh process — this is what makes a browser refresh
        // mid-turn re-attach to the existing live state rather than orphan it.
        let working_dir_path = working_dir.as_ref().map(PathBuf::from);

        // Acquire a per-(agent, working_dir, session_id) async mutex so two
        // concurrent connects for the same logical session can't both miss
        // dedup during the handshake window. The lookup → spawn → wait-for-
        // SessionStarted critical section runs under this lock; the second
        // waiter, on entry, observes the first call's connection with
        // `state.external_id` already populated and returns its id via
        // `find_connection_for_reuse`. Skipped entirely when `session_id`
        // is None (fresh sessions can't dedup — by design — since the
        // agent assigns the id).
        let session_id_for_log = session_id.clone();
        let dedup_lock = if let Some(sid) = session_id.as_deref() {
            let key = SpawnDedupKey {
                agent_type,
                working_dir: working_dir_path.clone(),
                session_id: sid.to_string(),
            };
            let mu = {
                let mut locks = self.spawn_locks.lock().await;
                locks
                    .entry(key)
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            };
            Some(mu.lock_owned().await)
        } else {
            None
        };

        if let Some(existing) = self
            .find_connection_for_reuse(agent_type, working_dir_path.as_ref(), session_id.as_deref())
            .await
        {
            tracing::info!(
                "[ACP] reusing connection id={} for session_id={}",
                existing,
                session_id.as_deref().unwrap_or("")
            );
            return Ok(existing);
        }

        let connection_id = uuid::Uuid::new_v4().to_string();
        tracing::info!(
            "[ACP] spawning connection id={} owner_window={} agent={:?}",
            connection_id, owner_window_label, agent_type
        );

        // `spawn_agent_connection` inserts the entry into `self.connections`,
        // installs the SessionStarted dedup signal on the state, registers
        // a cleanup hook, and returns the rx half of the signal. Any spawn
        // failure short-circuits before we touch the rx wait.
        let session_started_rx = spawn_agent_connection(
            connection_id.clone(),
            agent_type,
            working_dir,
            session_id,
            runtime_env,
            owner_window_label,
            emitter,
            self.connections.clone(),
            preferred_mode_id,
            preferred_config_values,
            role_mcp,
            self.delegation_snapshot(),
            self.terminal_shell_config.clone(),
        )
        .await?;

        // When dedup is active, hold the lock until the agent's
        // SessionStarted has applied (so external_id is populated for the
        // next waiter), aborted (connection died), or the timeout fires.
        // Logged on every wait so production can audit real-world handshake
        // latencies and tune `CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS`.
        if dedup_lock.is_some() {
            let timeout = self.spawn_handshake_timeout;
            let (outcome, elapsed) = wait_for_session_started(session_started_rx, timeout).await;
            tracing::info!(
                "[ACP] dedup_wait connection_id={} session_id={} outcome={} \
                 elapsed_ms={} timeout_ms={}",
                connection_id,
                session_id_for_log.as_deref().unwrap_or(""),
                outcome.as_str(),
                elapsed.as_millis(),
                timeout.as_millis(),
            );
        }
        // session_started_rx (in the no-dedup branch) is dropped here. tx
        // staying inside SessionState gets dropped naturally when the
        // connection terminates, no leak.

        drop(dedup_lock);

        Ok(connection_id)
    }

    /// Bump `last_activity_at` for a live connection so the idle sweep
    /// won't reap it. Used by the frontend keepalive loop to protect
    /// connections backing currently-open conversation tabs (the
    /// frontend is the only side that knows which tabs the user has
    /// open). Silently no-ops if the connection is missing or already
    /// in a terminal state — touch must never resurrect a dead
    /// connection or contend with the spawn/disconnect paths.
    pub async fn touch(&self, conn_id: &str) -> bool {
        let state_arc = {
            let connections = self.connections.lock().await;
            match connections.get(conn_id) {
                Some(conn) => conn.state.clone(),
                None => return false,
            }
        };
        let mut state = state_arc.write().await;
        if matches!(
            state.status,
            ConnectionStatus::Disconnected | ConnectionStatus::Error
        ) {
            return false;
        }
        state.last_activity_at = chrono::Utc::now();
        true
    }

    /// Disconnect connections that have been idle longer than `idle_timeout`.
    /// "Idle" means: status is `Connected`, no `pending_permission`, no
    /// launched-but-unresolved background work (async sub-agent / background
    /// shell — disconnecting kills the agent CLI and the background work with
    /// it), and no activity (no events, no commands) for at least
    /// `idle_timeout`. `Prompting` connections are always preserved (a turn is
    /// in flight). Returns the number of connections that were disconnected.
    pub async fn sweep_idle(&self, idle_timeout: Duration) -> usize {
        let now = chrono::Utc::now();
        let timeout = match chrono::Duration::from_std(idle_timeout) {
            Ok(d) => d,
            Err(_) => return 0,
        };
        let to_disconnect: Vec<String> = {
            let connections = self.connections.lock().await;
            let mut victims = Vec::new();
            for (id, conn) in connections.iter() {
                let Ok(state) = conn.state.try_read() else {
                    // Per-state writer holds the lock; a future tick will
                    // re-evaluate this entry. Don't block the connections
                    // mutex on it.
                    continue;
                };
                if state.status != ConnectionStatus::Connected {
                    continue;
                }
                if state.pending_permission.is_some() {
                    continue;
                }
                if state.has_active_background_work(now) {
                    continue;
                }
                let elapsed = now.signed_duration_since(state.last_activity_at);
                if elapsed >= timeout {
                    victims.push(id.clone());
                }
            }
            victims
        };
        let mut disconnected = 0;
        for id in to_disconnect {
            tracing::info!("[ACP] idle sweep disconnecting connection={}", id);
            if self.disconnect(&id).await.is_ok() {
                disconnected += 1;
            }
        }
        disconnected
    }

    /// Compare each running connection's spawn-time config fingerprint against a
    /// freshly recomputed one (keyed by agent type in `fresh`) and notify those
    /// that drifted. Drives the conversation-side "restart to apply" banner after
    /// a settings save.
    ///
    /// Emit policy, per connection:
    /// - emit `SessionConfigStale { stale }` only when the current fingerprint
    ///   differs from the one we last observed for it — a no-op save (identical
    ///   values) stays silent, while a second real change re-emits so a dismissed
    ///   banner reappears.
    /// - `stale = (current != spawn)`, so reverting a setting back to its
    ///   launch-time value emits `stale = false` and clears the banner.
    ///
    /// Returns the count of running connections currently on stale config across
    /// the affected agents (for the settings-side "N sessions need restart"
    /// toast). Connections whose agent type isn't in `fresh` are left untouched.
    ///
    /// `emit_with_state` is deferred until AFTER the connections-map lock is
    /// released (we collect targets first) so the SessionState write lock is
    /// never taken while holding the map lock.
    pub async fn refresh_connection_staleness(
        &self,
        fresh: &HashMap<AgentType, String>,
        kind: ConfigStaleKind,
    ) -> usize {
        let mut targets = Vec::new();
        let mut stale_count = 0usize;
        {
            let mut connections = self.connections.lock().await;
            for conn in connections.values_mut() {
                let Some(current) = fresh.get(&conn.agent_type) else {
                    continue;
                };
                let stale = *current != conn.config_fingerprint;
                if stale {
                    stale_count += 1;
                }
                if *current != conn.last_observed_fingerprint {
                    conn.last_observed_fingerprint = current.clone();
                    targets.push((Arc::clone(&conn.state), conn.emitter.clone(), stale));
                }
            }
        }
        for (state, emitter, stale) in targets {
            emit_with_state(&state, &emitter, AcpEvent::SessionConfigStale { stale, kind }).await;
        }
        stale_count
    }

    /// Look up an existing live connection that we can reuse instead of
    /// spawning a new process. Reuse criteria, ALL must hold:
    /// - `session_id` is Some (we never dedup speculative / fresh connects)
    /// - the connection's `state.external_id` equals `session_id`
    /// - the connection's `agent_type` equals the requested one
    /// - the connection's `working_dir` equals the requested one (compared as
    ///   `Option<PathBuf>` so canonicalization is the caller's concern)
    /// - the connection's `state.status` is neither `Disconnected` nor `Error`
    ///
    /// Per-session state is acquired via `read().await` rather than `try_read`:
    /// the only writer is `emit_with_state`, whose critical section is
    /// microseconds (apply_event + seq++ + broadcast::send), so contention
    /// resolves quickly and the previous "skip on writer" behavior was just
    /// trading correctness (false-negative dedup → duplicate process spawn)
    /// for an imperceptible latency win. The connections-map mutex is held
    /// across the awaits — fine because no path takes `state.write()` while
    /// holding the connections mutex (no lock-cycle).
    pub(crate) async fn find_connection_for_reuse(
        &self,
        agent_type: AgentType,
        working_dir: Option<&PathBuf>,
        session_id: Option<&str>,
    ) -> Option<String> {
        // No session_id → caller is opening a fresh session; never dedup.
        let session_id = session_id?;
        let connections = self.connections.lock().await;
        for (id, conn) in connections.iter() {
            if conn.agent_type != agent_type {
                continue;
            }
            let state = conn.state.read().await;
            if state.external_id.as_deref() != Some(session_id) {
                continue;
            }
            if state.working_dir.as_ref() != working_dir {
                continue;
            }
            if matches!(
                state.status,
                ConnectionStatus::Disconnected | ConnectionStatus::Error
            ) {
                continue;
            }
            return Some(id.clone());
        }
        None
    }

    /// Forwards a prompt to the connection's command channel without
    /// touching `prompt_lock`. Internal helper — both `send_prompt` and
    /// `send_prompt_linked` acquire the lock externally and then call
    /// this. Re-entering through `send_prompt` from `send_prompt_linked`
    /// while holding the lock would deadlock, hence the split.
    async fn send_prompt_inner(
        &self,
        conn_id: &str,
        mut blocks: Vec<PromptInputBlock>,
        user_message: Option<(String, Vec<crate::acp::UserMessageBlock>)>,
    ) -> Result<(), AcpError> {
        // Reject an empty prompt BEFORE touching the concurrency gate. An empty
        // prompt produces no turn — and thus no `TurnComplete` to clear the gate
        // — so enqueuing one with the gate set would wedge the connection into
        // rejecting every future send. `map_prompt_blocks` is 1:1, so empty
        // input blocks is the only way the loop could see an empty prompt; we
        // stop it here at the single shared enqueue path.
        if blocks.is_empty() {
            return Err(AcpError::protocol(
                "prompt must contain at least one content block".to_string(),
            ));
        }
        if strip_route_separator_from_prompt(&mut blocks) {
            tracing::debug!(
                "[ACP][{conn_id}] removed the reserved routing separator from an outgoing prompt"
            );
        }
        let (cmd_tx, state_arc) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (conn.cmd_tx.clone(), conn.state.clone())
        };
        // Concurrency gate: reject a second prompt while a turn is already in
        // flight on this connection. Reserve channel capacity FIRST — that
        // `reserve().await` is the only point that can block or be cancelled.
        // Then check+set the gate and hand the command to the permit
        // synchronously: because there is NO await between setting
        // `turn_in_flight` and the infallible `permit.send`, a dropped/cancelled
        // future can never strand the flag true (it is either never set — when
        // cancelled during reserve or the lock acquisition — or always followed
        // by the enqueue). The flag is set BEFORE the enqueue so it is in place
        // before the loop can dequeue and clear it on `TurnComplete`. Without
        // the gate the second `Prompt` would queue behind the active turn and be
        // silently dropped by the loop's in-turn handler (`_ => {}`) while the
        // caller saw success. `send_prompt` callers (e.g. the chat channel)
        // rely on this gate alone (no `prompt_lock`).
        let permit = cmd_tx
            .reserve()
            .await
            .map_err(|_| AcpError::ProcessExited)?;
        {
            let mut s = state_arc.write().await;
            if s.turn_in_flight {
                // Names the gate, not just the outcome: the linked path checks
                // the same flag once before its side effects and again here, and
                // the two lines share a module target with no file/line in the
                // default format — identical text would be unattributable.
                tracing::debug!(
                    connection_id = %conn_id,
                    "[ACP] prompt rejected at the send gate: a turn is already in flight"
                );
                return Err(AcpError::TurnInProgress);
            }
            s.turn_in_flight = true;
        }
        permit.send(ConnectionCommand::Prompt {
            blocks,
            user_message,
        });
        Ok(())
    }

    /// Clone the connection's `prompt_lock` under a short connections-map lock.
    /// Returned Arc allows the caller to hold the prompt lock without
    /// keeping the connections map locked.
    async fn clone_prompt_lock(
        &self,
        conn_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, AcpError> {
        let connections = self.connections.lock().await;
        let conn = connections
            .get(conn_id)
            .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
        Ok(conn.prompt_lock.clone())
    }

    pub async fn send_prompt(
        &self,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
    ) -> Result<(), AcpError> {
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _guard = prompt_lock.lock_owned().await;
        self.send_prompt_inner(conn_id, blocks, None).await
    }

    /// Send a prompt while ensuring a `Conversation` DB row is bound to this
    /// connection. On the first call (when `state.conversation_id` is None),
    /// either:
    /// - **Caller-supplied path** — if `conversation_id` is `Some(id)`, the
    ///   caller (the frontend) has already created the row and we adopt it via
    ///   `ConversationLinked`. Requires `folder_id` to be `Some` so the event
    ///   carries both ids without forcing subscribers to re-query the DB.
    /// - **Backend-creates path** — if `conversation_id` is `None`, we create
    ///   the row from `folder_id` (required) and emit `ConversationLinked`.
    ///   Returns an error if `folder_id` is also `None`.
    ///
    /// Subsequent calls (when state is already linked) ignore both
    /// `folder_id` and `conversation_id` and just forward the prompt.
    ///
    /// Back-compat wrapper for callers that don't supply a client message id
    /// (the delegation broker, internal/test paths). The UI send path uses
    /// [`send_prompt_linked_with_message_id`] so the sender's optimistic turn
    /// dedups against the broadcast `UserMessage` echo by exact id.
    pub async fn send_prompt_linked(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
        delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
    ) -> Result<Option<i32>, AcpError> {
        self.send_prompt_linked_with_message_id(
            db,
            conn_id,
            blocks,
            folder_id,
            conversation_id,
            delegation,
            None,
        )
        .await
    }

    /// As [`send_prompt_linked`], plus an optional `client_message_id`: the
    /// id the sending UI assigned to its own optimistic user turn. When the
    /// user prompt is broadcast as [`AcpEvent::UserMessage`] (for cross-client
    /// viewers), this id becomes the event's `message_id`, so the sender's
    /// runtime dedups the echo against its optimistic turn by EXACT id rather
    /// than a heuristic — and an unrelated optimistic turn on another client
    /// never suppresses a different sender's prompt. `None` falls back to a
    /// connection-scoped id for non-UI senders.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_prompt_linked_with_message_id(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        mut blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
        delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
        client_message_id: Option<String>,
    ) -> Result<Option<i32>, AcpError> {
        // Reject an empty prompt up front, BEFORE any side effects: linking /
        // creating the conversation row, flipping it to InProgress, or emitting
        // events. An empty prompt is never accepted, so it must not mutate
        // persisted state (create a row, or flip an existing one — which would
        // then be rolled back to Cancelled). `send_prompt_inner` keeps a
        // defensive copy of this guard for the non-linked `send_prompt` path.
        if blocks.is_empty() {
            return Err(AcpError::protocol(
                "prompt must contain at least one content block".to_string(),
            ));
        }
        // Scrub the reserved separator HERE, before the conversation row, the
        // optimistic broadcast, and the ledger all take their copy of `blocks`,
        // so every persisted / displayed / on-the-wire copy is byte-identical.
        if strip_route_separator_from_prompt(&mut blocks) {
            tracing::debug!(
                "[ACP][{conn_id}] removed the reserved routing separator from an outgoing prompt"
            );
        }
        // Caller-supplied conversation_id requires folder_id (we include it in
        // the emitted ConversationLinked event so subscribers don't have to
        // re-query the DB). Validate before touching any state.
        if conversation_id.is_some() && folder_id.is_none() {
            return Err(AcpError::protocol(
                "conversation_id provided without folder_id".to_string(),
            ));
        }
        // Delegation is only meaningful on the create-new-row branch — adopting
        // an existing caller-supplied row already has its own (or no) parent
        // linkage. Reject the combination loudly so a misuse from the broker
        // doesn't silently drop the linkage.
        if delegation.is_some() && conversation_id.is_some() {
            return Err(AcpError::protocol(
                "delegation link is incompatible with caller-supplied conversation_id".to_string(),
            ));
        }

        // Acquire the per-connection prompt lock for the entire link-check
        // + DB write + emit + cmd_tx.send sequence. Two concurrent prompts
        // (multiple browser tabs of the same conversation; chat-channel
        // racing the UI) are now strictly serialized — the second waiter
        // observes `already_linked == true` after the first commits, so
        // it can't double-create a conversation row.
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let _prompt_guard = prompt_lock.lock_owned().await;

        // Snapshot what we need from the connection map under one short lock.
        // The conversation-linked check happens INSIDE the prompt lock so
        // any racing send sees a consistent post-link state.
        let (state_arc, emitter, agent_type, already_linked, turn_in_flight) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            let (already, in_flight) = {
                let s = conn.state.read().await;
                (s.conversation_id.is_some(), s.turn_in_flight)
            };
            (
                conn.state.clone(),
                conn.emitter.clone(),
                conn.agent_type,
                already,
                in_flight,
            )
        };

        // Reject a concurrent prompt while a turn is already in flight, BEFORE
        // any side effects (row creation, InProgress emit, user-message
        // broadcast). `send_prompt_inner` re-checks and sets the flag
        // authoritatively below; doing it here too — while still holding
        // `prompt_lock`, so the value can't change underneath us (the loop only
        // ever clears it) — keeps a rejected prompt from flipping the row to
        // InProgress or broadcasting a phantom user message. The frontend turns
        // this rejection into a queued message above the input box.
        if turn_in_flight {
            // Distinct from `send_prompt_inner`'s send-gate line so a log reader
            // can tell which of the two checks bounced the prompt.
            tracing::debug!(
                connection_id = %conn_id,
                "[ACP] prompt rejected before admission: a turn is already in flight"
            );
            return Err(AcpError::TurnInProgress);
        }

        // Re-hydrate uploaded image attachments (web / remote-workspace mode
        // sends empty-payload marker blocks with a `file://` uri into the
        // uploads root; see `prompt_hydration`). Deliberately placed AFTER
        // admission — the connection-exists check (`clone_prompt_lock` above)
        // and the busy reject — so garbage or concurrent prompts never
        // trigger file reads, and the prompt lock we hold serializes
        // hydration per connection (a natural concurrency bound). Still
        // BEFORE any side effect (linking / row creation / status flip) and
        // before the `user_blocks_from_prompt` projection below, so a failure
        // aborts cleanly and the viewer broadcast, the sender echo, and the
        // agent all see the full bytes.
        crate::acp::prompt_hydration::hydrate_prompt_blocks(
            &mut blocks,
            &crate::paths::codeg_uploads_root(),
        )
        .await?;

        // CANCELLATION SHIELD around the first link, same shape and for the same
        // reason as `fork_session` below.
        //
        // Everything above this point is side-effect-free: drop this future and
        // `_prompt_guard` drops with it, having changed nothing. What follows is
        // not. `bind_external_id` COMMITS ownership of the agent session to a
        // row, and only the publication after it makes that row reachable —
        // `ConversationLinked` latches `state.conversation_id` (which is exactly
        // what `already_linked` reads), and the upsert puts the row in every
        // client's sidebar. Leaving that pair tied to this future means a
        // dropped caller (server mode: an HTTP client disconnecting
        // mid-request) can commit the bind and never publish, stranding a row
        // that owns the session but that nothing was told about — after which
        // Branch B fails EVERY retry with a `Conflict` naming that invisible
        // row, until the connection is re-established.
        //
        // Reordering does not solve it, it only moves the damage: publishing
        // first means a drop during the bind leaves the link latched onto a row
        // that does NOT hold the session, so the next prompt takes the
        // `already_linked` fast path and silently misfiles into the holder's
        // transcript. Both orderings are unsafe under cancellation, so the
        // phase must not be cancellable at all. It runs in a DETACHED task that
        // OWNS the guard and hands it back: dropping this future no longer
        // aborts the phase, it runs to completion and releases the lock when its
        // result is dropped. The handle is awaited only to hand the outcome to a
        // caller that is still there.
        let _prompt_guard = if already_linked {
            _prompt_guard
        } else {
            // Seed a delegation child's title from the task prompt so the
            // sidebar shows a meaningful label immediately. `list_children`
            // returns the raw DB title, so a child born with NULL reads
            // "Untitled" until the first detail load backfills it. Roots keep
            // `None` here and get the same first-prompt seed after the send
            // succeeds (so a failed enqueue does not name a cancelled row).
            // Computed out here so `blocks` stays with the caller for the
            // actual prompt.
            let seed_title = if delegation.is_some() {
                delegation_child_title_seed(&blocks)
            } else {
                None
            };
            let db_conn = db.conn.clone();
            let task_state = state_arc.clone();
            let task_emitter = emitter.clone();
            let task_conn_id = conn_id.to_string();
            let task_delegation = delegation.clone();
            let handle = tokio::spawn(async move {
                // Holding the owned guard for the whole task is what shields the
                // bind + publication from caller cancellation.
                let guard = _prompt_guard;
                let outcome: Result<(), AcpError> = async {
                    // Resolve which row this prompt belongs to WITHOUT
                    // announcing it yet — the announce is held back until the
                    // session id has actually been bound to that row below.
                    let (
                        linked_conversation_id,
                        linked_folder_id,
                        parent_conv_for_event,
                        parent_tool_for_event,
                    ) = match (conversation_id, folder_id) {
                        // Branch A: caller already owns a row — adopt it. No DB write.
                        (Some(caller_conv_id), Some(caller_folder_id)) => {
                            (caller_conv_id, caller_folder_id, None, None)
                        }
                        // Function-entry guard rejects this combination.
                        (Some(_), None) => unreachable!(
                            "conversation_id without folder_id should have been rejected at function entry"
                        ),
                        // Branch B: backend creates the row from caller-supplied
                        // folder_id. Phase 3c-1 made folder_id required here —
                        // every production caller that reaches this branch passes
                        // one, and silent fallback to working_dir-based
                        // find-or-create masked contract violations.
                        (None, Some(folder_id)) => {
                            // Snapshot the delegation link before
                            // move-into-create: we still need the parent ids for
                            // the ConversationLinked event payload.
                            let parent_conversation_id_for_event =
                                task_delegation.as_ref().map(|d| d.parent_conversation_id);
                            let parent_tool_use_id_for_event = task_delegation
                                .as_ref()
                                .map(|d| d.parent_tool_use_id.clone());
                            let row = conversation_service::create_with_delegation(
                                &db_conn,
                                folder_id,
                                agent_type,
                                seed_title,
                                None,
                                task_delegation,
                            )
                            .await
                            .map_err(|e| AcpError::protocol(e.to_string()))?;
                            (
                                row.id,
                                folder_id,
                                parent_conversation_id_for_event,
                                parent_tool_use_id_for_event,
                            )
                        }
                        (None, None) => {
                            return Err(AcpError::protocol(
                                "folder_id required for new conversation row".to_string(),
                            ));
                        }
                    };

                    // UI new-conversation path: SessionStarted applied
                    // state.external_id back during acp_connect, but
                    // conversation_id was None then so the lifecycle
                    // subscriber's SessionStarted handler skipped the DB write.
                    // Now that we have resolved the row in the same prompt_lock
                    // critical section, snapshot external_id and persist it
                    // synchronously — no dependence on broadcaster eventual
                    // consistency. The chat_channel reverse-order path (link
                    // before SessionStarted) is unaffected and continues to be
                    // handled by the lifecycle subscriber.
                    let eid_opt = task_state.read().await.external_id.clone();
                    let preserved = if let Some(eid) = eid_opt {
                        // THE codeg#500 write. Branch A above adopts a row the
                        // caller supplied, which may still be bound to an older
                        // session — a connection spawned with `session_id = None`
                        // (a reconnect that lost the id, or an unclassified
                        // `session/load` failure that fell through to
                        // `session/new`) arrives here holding a session that has
                        // nothing to do with the row's history.
                        // `bind_external_id` splits that history onto its own row
                        // instead of overwriting it.
                        let continues = crate::acp::continued_session_ids(agent_type, &eid);
                        match conversation_service::bind_external_id(
                            &db_conn,
                            linked_conversation_id,
                            &eid,
                            &continues,
                        )
                        .await
                        {
                            Ok(preserved) => preserved,
                            Err(e) => {
                                // Refused, or a genuine DB failure. Either way
                                // this row does not hold the session, so the
                                // prompt must not go out and the link must not
                                // stick.
                                //
                                // Branch B minted a row moments ago that no
                                // client has been told about — it was never
                                // announced, because the announce is below this
                                // bind. Drop it, or every refused attempt would
                                // leave another empty conversation behind for the
                                // sidebar's next full refetch to surface. Branch
                                // A creates nothing and has nothing to undo.
                                if conversation_id.is_none() {
                                    if let Err(cleanup) = conversation_service::soft_delete(
                                        &db_conn,
                                        linked_conversation_id,
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            conversation_id = linked_conversation_id,
                                            error = %cleanup,
                                            "[manager] failed to drop the unannounced row left \
                                             by a refused bind"
                                        );
                                    }
                                }
                                return Err(AcpError::protocol(e.to_string()));
                            }
                        }
                    } else {
                        tracing::info!(
                            "[manager] send_prompt_linked: conversation resolved but \
                             external_id not yet on state (conn={task_conn_id}); lifecycle \
                             subscriber will catch up when SessionStarted arrives"
                        );
                        None
                    };

                    // Bound (or nothing to bind). NOW the link is real, so
                    // announce it: this is the write that makes `already_linked`
                    // true for every subsequent prompt on this connection.
                    emit_with_state(
                        &task_state,
                        &task_emitter,
                        AcpEvent::ConversationLinked {
                            conversation_id: linked_conversation_id,
                            folder_id: linked_folder_id,
                            parent_conversation_id: parent_conv_for_event,
                            parent_tool_use_id: parent_tool_for_event,
                        },
                    )
                    .await;
                    // Sidebar sync: a conversation born here (agent path — a
                    // prompt sent without a pre-created row, not the create
                    // button) must reach every client immediately via the global
                    // `conversation://changed` channel. Roots land in the sidebar
                    // list; delegation children (parent set) are routed into
                    // their parent's expanded sub-session subtree and bump its
                    // chevron. Emitted after the bind, so — unlike before, when
                    // Branch B broadcast an `external_id: null` summary and a
                    // second upsert corrected it — this carries the complete
                    // summary first time.
                    crate::commands::conversations::emit_conversation_upsert(
                        &task_emitter,
                        &db_conn,
                        linked_conversation_id,
                    )
                    .await;
                    // A new delegation child changes its parent's child_count
                    // (0 → >0 makes the parent's expand chevron appear). Re-emit
                    // the parent so every client converges its count from the
                    // authoritative DB aggregate rather than a drift-prone
                    // per-client increment. The parent may itself be a root or a
                    // nested child — the upsert routes correctly either way by
                    // its own parent_id.
                    if let Some(parent_id) = parent_conv_for_event {
                        crate::commands::conversations::emit_conversation_upsert(
                            &task_emitter,
                            &db_conn,
                            parent_id,
                        )
                        .await;
                    }
                    crate::commands::conversations::emit_preserved_conversation(
                        &task_emitter,
                        &db_conn,
                        preserved,
                    )
                    .await;
                    Ok(())
                }
                .await;
                // Logged HERE, not just returned. Once the phase is detached the
                // caller may be gone by now, and `handle.await`'s result is then
                // dropped unread — a `Conflict` (or any failure) would leave no
                // trace at all in exactly the disconnect case this shield exists
                // to handle.
                if let Err(e) = &outcome {
                    tracing::warn!(
                        connection_id = %task_conn_id,
                        error = %e,
                        "[manager] conversation link phase failed"
                    );
                }
                (outcome, guard)
            });
            let (outcome, guard) = handle.await.map_err(|e| {
                AcpError::protocol(format!("conversation link phase failed to join: {e}"))
            })?;
            outcome?;
            guard
        };

        // Centralized status transition: every prompt send flips the
        // conversation row to InProgress. This MUST happen on every call
        // (including the already-linked path) so that a follow-up turn whose
        // row is currently `pending_review` correctly transitions back. The
        // DB write precedes the event emit so any subscriber observing
        // `ConversationStatusChanged` can assume the row is consistent.
        // `update_status` is a single UPDATE — idempotent with respect to
        // the same status value, so re-writing `InProgress` is a benign no-op
        // on the row (touches `updated_at` only).
        let conversation_id_for_status = state_arc.read().await.conversation_id;
        if let Some(cid) = conversation_id_for_status {
            conversation_service::update_status(&db.conn, cid, ConversationStatus::InProgress)
                .await
                .map_err(|e| AcpError::protocol(e.to_string()))?;
            emit_with_state(
                &state_arc,
                &emitter,
                AcpEvent::ConversationStatusChanged {
                    conversation_id: cid,
                    status: ConversationStatus::InProgress,
                },
            )
            .await;
        }

        // Capture a bounded preview of the user's message BEFORE `blocks` is
        // moved into `send_prompt_inner`. Only on the genuine UI path
        // (`delegation.is_none()`): delegation / sub-agent prompts are not user
        // messages. Emitted after the send succeeds (below) so a prompt that
        // never reached the agent produces no "user message" notification.
        let user_prompt_preview = if delegation.is_none() {
            user_prompt_text_preview(&blocks)
        } else {
            None
        };
        // Seed an unlocked title from the first prompt so the sidebar is not
        // "Untitled" while the agent is still working. Native ACP titles
        // (session_info_update) replace this later via refresh_auto_title.
        // Delegation children are already seeded at row create.
        let first_prompt_title = if delegation.is_none() {
            delegation_child_title_seed(&blocks)
        } else {
            None
        };

        // Project the user's prompt blocks for the cross-client viewer
        // broadcast BEFORE `send_prompt_inner` consumes `blocks`, and hand the
        // payload to the connection loop (via `ConnectionCommand::Prompt`) so it
        // emits the `UserMessage` event in-order, right before the agent
        // request — guaranteeing its seq precedes the turn's agent events and
        // that it only fires for a prompt actually processed (a failed enqueue
        // delivers no command, so nothing strands a `pending_user_message`).
        // Gated on `delegation.is_none()` (children surface kickoff text
        // separately) and a bound conversation row (a sidebar-visible turn). The
        // `message_id` prefers the sender's client-supplied id (exact echo
        // dedup), falling back to a connection-scoped id for non-UI senders.
        let user_message: Option<(String, Vec<crate::acp::UserMessageBlock>)> =
            if delegation.is_none() && conversation_id_for_status.is_some() {
                let user_blocks = crate::acp::user_blocks_from_prompt(&blocks);
                if user_blocks.is_empty() {
                    None
                } else {
                    // A client-supplied id in the parsers' turn-id namespace
                    // (`turn-<digits>`, which every parser assigns) would collide
                    // with a persisted transcript turn id and break id-keyed dedup
                    // — a colliding id can suppress or hide a prompt. The id is
                    // untrusted (the web/Tauri prompt API accepts it verbatim), so
                    // reject that shape and fall back to a connection-scoped id;
                    // legitimate UI senders use `optimistic-<uuid>`.
                    let message_id = match client_message_id {
                        Some(id) if !is_reserved_turn_id(&id) => id,
                        _ => format!("user-{}-{}", conn_id, state_arc.read().await.event_seq),
                    };
                    Some((message_id, user_blocks))
                }
            } else {
                None
            };

        // We hold `_prompt_guard` here, so call the lock-free inner helper —
        // re-entering `send_prompt` would try to acquire the same mutex and
        // deadlock. The helper reserves channel capacity FIRST and only then
        // sets the turn-in-flight gate, with no await before the infallible
        // `permit.send`; so a failure (channel closed / process exited) happens
        // at the reserve step, BEFORE the gate is set — there is nothing to roll
        // back. On that failure we still flip the row to `Cancelled` so the UI
        // doesn't strand on `in_progress`: no `TurnComplete` will ever arrive
        // for a prompt that never reached the agent, so without this the
        // lifecycle subscriber's PendingReview write also never fires and the
        // row would be stuck until a follow-up `send_prompt_linked` re-flipped it.
        match self
            .send_prompt_inner(conn_id, blocks, user_message)
            .await
        {
            Ok(()) => {
                // The prompt reached the agent: surface it to the chat-channel
                // "user message" event feed. Notification-only — never gates the
                // send result.
                if let Some(text_preview) = user_prompt_preview {
                    emit_with_state(
                        &state_arc,
                        &emitter,
                        AcpEvent::UserPromptSent { text_preview },
                    )
                    .await;
                }
                if let (Some(cid), Some(title)) =
                    (conversation_id_for_status, first_prompt_title)
                {
                    match conversation_service::seed_auto_title_if_empty(&db.conn, cid, title)
                        .await
                    {
                        Ok(true) => {
                            crate::commands::conversations::emit_conversation_upsert(
                                &emitter, &db.conn, cid,
                            )
                            .await;
                            if let Some(ccm) = self.chat_channel() {
                                crate::commands::conversations::spawn_sync_conversation_title_until_current(
                                    db.conn.clone(),
                                    ccm,
                                    cid,
                                );
                            }
                        }
                        Ok(false) => {}
                        Err(e) => tracing::warn!(
                            conversation_id = cid,
                            error = %e,
                            "[manager] first-prompt title seed failed"
                        ),
                    }
                }
                Ok(conversation_id_for_status)
            }
            Err(send_err) => {
                if let Some(cid) = conversation_id_for_status {
                    match conversation_service::update_status(
                        &db.conn,
                        cid,
                        ConversationStatus::Cancelled,
                    )
                    .await
                    {
                        Ok(_) => {
                            emit_with_state(
                                &state_arc,
                                &emitter,
                                AcpEvent::ConversationStatusChanged {
                                    conversation_id: cid,
                                    status: ConversationStatus::Cancelled,
                                },
                            )
                            .await;
                        }
                        Err(rollback_err) => {
                            // Best-effort: original send error is the load-bearing
                            // signal; rollback failure is logged but not surfaced.
                            tracing::error!(
                                "[ACP][ERROR] failed to mark conversation {cid} cancelled \
                                 after send failure (original={send_err}): {rollback_err}"
                            );
                        }
                    }
                }
                Err(send_err)
            }
        }
    }

    pub async fn set_mode(&self, conn_id: &str, mode_id: String) -> Result<(), AcpError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            conn.cmd_tx.clone()
        };
        cmd_tx
            .send(ConnectionCommand::SetMode { mode_id })
            .await
            .map_err(|_| AcpError::ProcessExited)
    }

    pub async fn set_config_option(
        &self,
        conn_id: &str,
        config_id: String,
        value_id: String,
    ) -> Result<(), AcpError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            conn.cmd_tx.clone()
        };
        cmd_tx
            .send(ConnectionCommand::SetConfigOption {
                config_id,
                value_id,
            })
            .await
            .map_err(|_| AcpError::ProcessExited)
    }

    /// Pause or clear the session's active goal via the connection loop
    /// (codex-acp #293; the provider-neutral `_session/goal` since codex 1.2 /
    /// claude 0.66). Looked up by connectionId; the loop sources the sessionId
    /// from the live session, so callers only supply the action.
    ///
    /// STOPS THE RUNNING TURN TOO, for adapters whose control request travels
    /// out of band (see `registry::goal_control_is_out_of_band`). Without that,
    /// the button is a lie: codex's pause/clear are pure app-server metadata
    /// that only take hold at the next idle point, so the user clicks 暂停 and
    /// watches the agent keep working — the whole reason the goal card's
    /// controls were once ripped out.
    ///
    /// Two conditions guard it, and neither is "is a turn running?":
    /// * the goal must have been ACTIVE when the user clicked
    ///   (`SessionState.goal_active`, read before the request goes out). An
    ///   active goal is the thing driving the work, so stopping the work is
    ///   what the click means. A PAUSED goal drives nothing — clearing one is
    ///   housekeeping, and must never abort a turn the user started themselves
    ///   in the meantime.
    /// * the control must have LANDED. The loop reports that back over a
    ///   oneshot; a rejected request (or a closed channel) leaves the turn
    ///   alone, so a goal that is still `active` never gets its work killed
    ///   only to have codex resume it at the next idle point. The round-trip
    ///   also makes the interrupt strictly second: the goal is already
    ///   non-active when the abort lands, so nothing auto-continues on the way
    ///   out.
    ///
    /// Liveness deliberately does NOT gate it. `ConnectionStatus::Prompting`
    /// (and `turn_in_flight` with it) tracks turns CODEG started, and a goal
    /// loop's continuations are started by codex itself — detached turns no host
    /// request owns — so gating on them would skip the interrupt in exactly the
    /// case that motivated this. Any post-hoc read is unsound anyway: by the
    /// time the manager acts on it the turn it described may have ended. With
    /// the goal known active, the interrupt means precisely "press Stop on the
    /// user's behalf": `cancel()`'s row write is CAS'd from `InProgress`, a
    /// `session/cancel` with nothing running is a no-op, and its permission
    /// drain / delegation cascade are the semantics the Stop button already has
    /// (including its own turn-boundary race, which is not made worse here).
    ///
    /// Awaiting the round-trip is deliberate and cheap — an out-of-band control
    /// is a plain RPC that returns without waiting for the turn. It is NOT
    /// cancellation-shielded (unlike `submit_feedback_native`): a caller that
    /// disappears mid-await simply doesn't get the follow-up interrupt, which is
    /// the pre-existing behavior, never worse. The control itself is owned by
    /// the loop from the moment it is enqueued.
    pub async fn goal_control(
        &self,
        db: &DatabaseConnection,
        conn_id: &str,
        action: GoalControlAction,
    ) -> Result<(), AcpError> {
        let (cmd_tx, agent_type, state_arc) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (conn.cmd_tx.clone(), conn.agent_type, conn.state.clone())
        };
        // Read the goal state as it was when the user clicked — reading it after
        // the round-trip would see the transition we just asked for.
        let interrupts = crate::acp::registry::goal_control_is_out_of_band(agent_type)
            && state_arc.read().await.goal_active;
        // Only ask for an answer when it would change what we do next.
        let (reply_tx, reply_rx) = if interrupts {
            let (tx, rx) = tokio::sync::oneshot::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        cmd_tx
            .send(ConnectionCommand::GoalControl {
                action,
                reply: reply_tx,
            })
            .await
            .map_err(|_| AcpError::ProcessExited)?;

        let Some(reply_rx) = reply_rx else {
            return Ok(());
        };
        if !matches!(reply_rx.await, Ok(true)) {
            return Ok(());
        }
        tracing::info!(
            "[ACP] goal {:?} landed; interrupting so the work stops now connection={}",
            action,
            conn_id
        );
        self.cancel(db, conn_id).await
    }

    /// Stop one AIR async task (`_session/async_task/stop`).
    ///
    /// Returns the adapter's own verdict, not "the request went through": it
    /// answers `false` for a task it declines to stop. Unshielded, unlike
    /// `submit_feedback_native` — there is nothing to persist on this path, so a
    /// caller that disconnects mid-await leaves no half-written state, and the
    /// user-visible result arrives on the session channel regardless of who is
    /// still listening for the reply.
    ///
    /// The task id is NOT pre-validated against `SessionState.async_tasks`. The
    /// adapter owns that table's real lifecycle (its `claimStop` already refuses
    /// unknown, terminal, and already-stopping tasks), and a local check could
    /// only ever be staler than the adapter's — it would turn a race into a
    /// silent no-op rather than the `false` the caller can report.
    pub async fn stop_async_task(&self, conn_id: &str, task_id: &str) -> Result<bool, AcpError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?
                .cmd_tx
                .clone()
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        cmd_tx
            .send(ConnectionCommand::StopAsyncTask {
                task_id: task_id.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| AcpError::ProcessExited)?;
        reply_rx
            .await
            .map_err(|_| AcpError::protocol("Async task stop reply channel closed".to_string()))?
    }

    pub async fn cancel(&self, db: &DatabaseConnection, conn_id: &str) -> Result<(), AcpError> {
        let (cmd_tx, state_arc, emitter) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (
                conn.cmd_tx.clone(),
                conn.state.clone(),
                conn.emitter.clone(),
            )
        };
        cmd_tx
            .send(ConnectionCommand::Cancel)
            .await
            .map_err(|_| AcpError::ProcessExited)?;

        // Eagerly flip the row to `Cancelled` so the sidebar/tabs leave the
        // "running" state immediately. The agent typically replies with
        // `TurnComplete{cancelled}` which the lifecycle subscriber ignores,
        // and stays connected (so `handle_terminal_event` doesn't fire either)
        // — without this write the row would strand on `InProgress`.
        // CAS-guarded so we don't overwrite a `PendingReview`/`Completed`
        // status if the turn happened to end just before the user clicked.
        let conversation_id = state_arc.read().await.conversation_id;
        if let Some(cid) = conversation_id {
            match conversation_service::update_status_if(
                db,
                cid,
                ConversationStatus::InProgress,
                ConversationStatus::Cancelled,
            )
            .await
            {
                Ok(true) => {
                    emit_with_state(
                        &state_arc,
                        &emitter,
                        AcpEvent::ConversationStatusChanged {
                            conversation_id: cid,
                            status: ConversationStatus::Cancelled,
                        },
                    )
                    .await;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(
                        "[ACP][ERROR] failed to mark conversation {cid} cancelled \
                         on user cancel (conn={conn_id}): {e}"
                    );
                }
            }
        }

        Ok(())
    }

    /// [crewchat] Phase 4.6 — record the role's permission decision on a live
    /// connection: answer its permission requests without asking the user.
    ///
    /// Set right after `spawn_agent` and BEFORE the first prompt, which is the
    /// only window where it matters: a request can only arrive once the agent
    /// has been asked to do something. Unknown connections are ignored (a
    /// teardown raced us), never an error — this is a hint, not a command.
    pub async fn set_role_auto_approve(&self, conn_id: &str, auto_approve: bool) {
        let connections = self.connections.lock().await;
        let Some(conn) = connections.get(conn_id) else {
            return;
        };
        conn.state.write().await.role_auto_approve = auto_approve;
    }

    pub async fn respond_permission(
        &self,
        conn_id: &str,
        request_id: &str,
        option_id: &str,
    ) -> Result<(), AcpError> {
        let cmd_tx = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            conn.cmd_tx.clone()
        };
        cmd_tx
            .send(ConnectionCommand::RespondPermission {
                request_id: request_id.into(),
                option_id: option_id.into(),
            })
            .await
            .map_err(|_| AcpError::ProcessExited)
    }

    /// Fork the agent's session and persist the resulting two-row layout in
    /// one backend call: the current row gets re-pointed at S2 (the forked
    /// session) with a `[Fork]` title prefix, and a freshly-created sibling
    /// row preserves the pre-fork (S1) history at `PendingReview`. Frontend
    /// no longer touches `external_id` or fork-related row creation —
    /// the wire `ForkResultInfo` carries `sibling_conversation_id` for tab/UI
    /// reconciliation.
    pub async fn fork_session(
        &self,
        db: &AppDatabase,
        conn_id: &str,
        // Caller-supplied linkage for a connection that resumed a historical
        // conversation but hasn't sent a prompt through it yet. Such a
        // connection is bound to its session via `session_id` (resume) but its
        // conversation ROW isn't linked until the first prompt fires
        // `ConversationLinked` (see `send_prompt_linked`). A fork-send forks
        // BEFORE that first prompt, so without adopting the row here the fork
        // would reject as unlinked. Ignored when the connection is already
        // linked (the common new-conversation-then-fork path), and both must be
        // `Some` to link (a `conversation_id` needs its `folder_id`, mirroring
        // `send_prompt_linked`'s Branch A contract).
        link_conversation_id: Option<i32>,
        link_folder_id: Option<i32>,
        // Fork at this rendered turn instead of at the tail ("fork from here").
        // Resolved to an agent-specific `ForkPoint` below; a turn this agent
        // cannot name simply forks at the tail, which is what fork-send has
        // always done.
        fork_from_turn_id: Option<String>,
    ) -> Result<ForkResultInfo, AcpError> {
        let (state_arc, cmd_tx, emitter) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
            (
                conn.state.clone(),
                conn.cmd_tx.clone(),
                conn.emitter.clone(),
            )
        };

        // Serialize the fork against concurrent prompts on this connection via
        // the same per-connection `prompt_lock` that `send_prompt`/
        // `send_prompt_linked` hold. A fork re-points the live session, so a
        // prompt must never start a turn underneath it. The lock is held for the
        // WHOLE operation (gate check → enqueue → protocol round-trip →
        // persistence); because the LOCK (not a flag) provides the exclusion,
        // the fork never SETS `turn_in_flight`, so there is no flag a dropped
        // future could strand and no window where a prompt's side effects (row
        // create / InProgress) commit only to lose the gate to a fork and roll
        // back to `Cancelled`.
        let prompt_lock = self.clone_prompt_lock(conn_id).await?;
        let prompt_guard = prompt_lock.lock_owned().await;

        // Link the conversation row on demand, under the prompt lock so it
        // can't race a concurrent first prompt. A conversation opened from
        // history resumes via `session_id`, but its row is bound to the
        // connection only when the first prompt fires `ConversationLinked`;
        // fork-send forks first, so adopt the caller-supplied row here (the
        // same existing-row path as `send_prompt_linked` Branch A). No-op when
        // already linked, or when the caller didn't supply both ids (the check
        // below then rejects, unchanged).
        if state_arc.read().await.conversation_id.is_none() {
            if let (Some(cid), Some(fid)) = (link_conversation_id, link_folder_id) {
                emit_with_state(
                    &state_arc,
                    &emitter,
                    AcpEvent::ConversationLinked {
                        conversation_id: cid,
                        folder_id: fid,
                        parent_conversation_id: None,
                        parent_tool_use_id: None,
                    },
                )
                .await;
            }
        }

        // Fork requires a linked conversation row — the sibling we're about
        // to create exists to preserve THIS row's pre-fork history. Without
        // a current row, fork would either orphan S1 or violate the
        // no-pre-prompt-row invariant.
        let conversation_id = state_arc.read().await.conversation_id.ok_or_else(|| {
            AcpError::protocol("fork_session requires a linked conversation row".to_string())
        })?;

        // Resolve the fork point BEFORE the cancellation shield below: this is
        // a read-only parse, so a caller that disappears here has changed
        // nothing. Failing to resolve is not an error — it degrades to the tail
        // fork rather than refusing the user's click.
        let fork_point = match fork_from_turn_id {
            None => None,
            Some(turn_id) => {
                let agent_type = state_arc.read().await.agent_type;
                match crate::commands::conversations::get_folder_conversation_core(
                    &db.conn,
                    conversation_id,
                )
                .await
                {
                    Ok((detail, _)) => {
                        let point = crate::acp::fork::resolve_fork_point(
                            &detail.turns,
                            &turn_id,
                            agent_type,
                        );
                        if point.is_none() {
                            tracing::info!(
                                connection_id = %conn_id,
                                turn_id = %turn_id,
                                agent = %agent_type,
                                "[ACP] no fork point for this turn; forking at the tail"
                            );
                        }
                        point
                    }
                    Err(e) => {
                        tracing::warn!(
                            connection_id = %conn_id,
                            turn_id = %turn_id,
                            "[ACP] could not read the conversation to resolve a fork point \
                             ({e}); forking at the tail"
                        );
                        None
                    }
                }
            }
        };

        // Reject if a turn is already in flight. `prompt_lock` is FREE between a
        // prompt's enqueue and its `TurnComplete` (it is released the moment the
        // command is queued), so the lock alone can't catch a turn the loop is
        // mid-processing — only the gate can. We CHECK the gate (bouncing with
        // `TurnInProgress` so the caller re-queues) under the prompt lock, where
        // the loop is the only writer and the value can't flip to true
        // underneath us, but we never SET it: not setting the gate is precisely
        // why a dropped fork can't wedge the connection.
        if state_arc.read().await.turn_in_flight {
            tracing::debug!(
                connection_id = %conn_id,
                "[ACP] fork rejected: a turn is already in flight"
            );
            return Err(AcpError::TurnInProgress);
        }

        // CANCELLATION SHIELD. Up to here the fork is side-effect-free: if THIS
        // future is dropped now (e.g. an HTTP client disconnecting mid-fork), the
        // `prompt_guard` drops and nothing happened. But the instant we enqueue
        // `ConnectionCommand::Fork`, the connection loop executes the agent
        // `session/fork` and re-points the live session to S2 REGARDLESS of
        // whether this caller survives — `handle_fork_or_exit` ignores a dead
        // reply channel and still attaches + emits `SessionStarted{S2}`. So the
        // DB persistence (sibling row preserving S1 + `[Fork]` title) must NOT be
        // tied to this future; otherwise a dropped caller would strand the live
        // session on S2 with the pre-fork S1 history orphaned and no sibling row.
        // We run enqueue → reply → persist → emit in a DETACHED task that OWNS
        // the `prompt_guard`: dropping this future no longer aborts the
        // persistence — it runs to completion and only then releases the lock.
        // We await the task's handle purely to hand the result back to a live
        // caller; the result is harmlessly discarded if the caller is gone.
        let db_conn = db.conn.clone();
        let conn_id_for_task = conn_id.to_string();
        let handle = tokio::spawn(async move {
            // Holding the owned guard for the whole task is what shields the
            // persistence from caller cancellation.
            let _prompt_guard = prompt_guard;
            let outcome: Result<ForkResultInfo, AcpError> = async {
                // Protocol-only round trip — no DB writes inside the loop.
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                cmd_tx
                    .send(ConnectionCommand::Fork {
                        fork_point,
                        reply: reply_tx,
                    })
                    .await
                    .map_err(|_| AcpError::ProcessExited)?;
                let protocol_result = reply_rx
                    .await
                    .map_err(|_| AcpError::protocol("Fork reply channel closed".to_string()))??;

                let forked_session_id = protocol_result.forked_session_id;
                let original_session_id = protocol_result.original_session_id;

                let sibling_id = Self::persist_fork_outcome(
                    &db_conn,
                    conversation_id,
                    forked_session_id.clone(),
                    original_session_id.clone(),
                )
                .await?;

                // Fork mutates the sidebar in two ways the rest of the system
                // never sees otherwise: the current row's title (`[Fork] …`) and
                // external_id (→ S2) changed, and a brand-new sibling row now
                // exists (external_id S1, PendingReview). Broadcast both on
                // `conversation://changed` so every other client converges in
                // real time instead of waiting for a manual refresh. Both rows
                // are roots; the helper still guards `parent_id` internally.
                crate::commands::conversations::emit_conversation_upsert(
                    &emitter,
                    &db_conn,
                    conversation_id,
                )
                .await;
                crate::commands::conversations::emit_conversation_upsert(
                    &emitter, &db_conn, sibling_id,
                )
                .await;

                Ok(ForkResultInfo {
                    forked_session_id,
                    original_session_id,
                    sibling_conversation_id: sibling_id,
                })
            }
            .await;
            // Surface failures even when the caller is gone (the detached task's
            // Result would otherwise be dropped silently).
            if let Err(ref e) = outcome {
                tracing::error!("[ACP][ERROR] fork persistence failed (conn={conn_id_for_task}): {e}");
            }
            outcome
        });

        match handle.await {
            Ok(result) => result,
            Err(join_err) => {
                tracing::error!(
                    "[ACP][ERROR] fork persistence task did not complete (conn={conn_id}): \
                     {join_err}"
                );
                Err(AcpError::protocol(format!(
                    "fork persistence task did not complete: {join_err}"
                )))
            }
        }
    }

    /// Persist the two-row fork layout: re-point the current row at S2 under a
    /// locked `[Fork]` title prefix, and INSERT a sibling row preserving the
    /// pre-fork (S1) history at `PendingReview`, which inherits the original's
    /// title lock. Returns the sibling row id.
    ///
    /// Both titles outlive the per-turn auto-title backfill by design: neither
    /// the user's own name nor codeg's `[Fork] ` marker exists in the session
    /// file the backfill re-parses, so an unlocked row would silently revert to
    /// the parsed title on its next detail load.
    ///
    /// Factored out of [`fork_session`] so the cancellation-shielded task body
    /// stays readable. Everything runs in one transaction so a mid-sequence
    /// failure can't leak: if INSERT fails we don't re-point the current row at
    /// S2 (it stays bound to S1; the lifecycle subscriber's eventual
    /// `SessionStarted{S2}` write would still occur, but the user-visible row
    /// layout stays consistent until then). If the current-row UPDATE fails we
    /// never insert a sibling — no orphan.
    ///
    /// The transaction is deliberately WRITE-FIRST. SeaORM's SQLite backend
    /// always opens a transaction with a plain (deferred) `BEGIN` — access mode
    /// isn't configurable per-transaction for SQLite — so a transaction that
    /// LED with the `SELECT` of the current row would take a read snapshot
    /// first; if any other pooled connection commits a write before this
    /// transaction's later UPDATE (routine under this app's concurrent
    /// multi-conversation load), SQLite can't promote that now-stale snapshot
    /// to a writer and fails the whole transaction with `SQLITE_BUSY_SNAPSHOT`
    /// (code 517) — surfaced to the user as "database is locked" even though
    /// nothing was actually deadlocked, and NOT retried by `busy_timeout` (that
    /// only covers ordinary lock contention). So the FIRST statement is a write
    /// (bump `updated_at`, which we want anyway and which claims the writer
    /// lock), and only THEN do we read the row. Reading under the held write
    /// lock has a second payoff: because no other writer can interpose between
    /// the read and the UPDATE/INSERT, the title/metadata we derive can't be a
    /// stale snapshot that a concurrent rename/soft-delete already superseded —
    /// the fork observes the latest committed row and never clobbers a newer
    /// title or forks from stale routing.
    ///
    /// The claim write is filtered on `deleted_at IS NULL` (the codebase-wide
    /// "live row" predicate). Forking a soft-deleted conversation would
    /// otherwise resurrect it as a fresh, visible sibling (`deleted_at = None`),
    /// so a claim that matches no LIVE row is treated as not-found and the whole
    /// fork aborts without writing anything.
    async fn persist_fork_outcome(
        db_conn: &DatabaseConnection,
        conversation_id: i32,
        forked_session_id: String,
        original_session_id: String,
    ) -> Result<i32, AcpError> {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, QueryFilter};

        db_conn
            .transaction::<_, i32, sea_orm::DbErr>(|txn| {
                Box::pin(async move {
                    let now = chrono::Utc::now();

                    // WRITE FIRST — see the fn doc. Bumping `updated_at` is the
                    // transaction's opening statement so SQLite acquires the
                    // writer lock immediately instead of taking a deferred read
                    // snapshot it would later have to (and might fail to)
                    // promote. Filtered on `deleted_at IS NULL` so a soft-deleted
                    // conversation can't be forked back into a live sibling;
                    // `rows_affected == 0` means the row is gone OR deleted.
                    let claimed = conversation::Entity::update_many()
                        .col_expr(conversation::Column::UpdatedAt, Expr::value(now))
                        .filter(conversation::Column::Id.eq(conversation_id))
                        .filter(conversation::Column::DeletedAt.is_null())
                        .exec(txn)
                        .await?;
                    if claimed.rows_affected == 0 {
                        return Err(sea_orm::DbErr::Custom(format!(
                            "conversation {conversation_id} not found or already deleted"
                        )));
                    }

                    // Read UNDER the write lock: this SELECT sees the latest
                    // committed state and no other writer can interpose before
                    // this transaction finishes, so the derived title/metadata
                    // below can't be superseded by a concurrent rename/delete.
                    // The successful live-row claim above guarantees this returns
                    // Some; the `ok_or_else` is defensive.
                    let current = conversation::Entity::find_by_id(conversation_id)
                        .one(txn)
                        .await?
                        .ok_or_else(|| {
                            sea_orm::DbErr::Custom(format!(
                                "conversation {conversation_id} not found"
                            ))
                        })?;

                    // Strip any `[Fork]` prefix tolerantly (matches the prior
                    // frontend regex `/^\[Fork]\s*/g` behaviour for both spaced
                    // and no-space variants). None title stays None.
                    let clean_title: Option<String> = current.title.as_ref().map(|t| {
                        t.strip_prefix("[Fork]")
                            .map(str::trim_start)
                            .unwrap_or(t.as_str())
                            .to_string()
                    });

                    let folder_id = current.folder_id;
                    let agent_type_str = current.agent_type.clone();
                    let git_branch = current.git_branch.clone();
                    // The lock rides along with the title it protects: the
                    // sibling holds the pre-fork history of the very row the
                    // user renamed, so a hand-picked name has to stay locked
                    // there too. Born unlocked, the sibling's first detail load
                    // re-parses S1 and lets `refresh_auto_title` adopt the
                    // session-file title — the rename silently reverting on the
                    // conversation the fork was supposed to leave untouched.
                    // Inheriting (not forcing) keeps an auto-titled sibling
                    // eligible for later backfills.
                    let title_locked = current.title_locked;
                    // The sibling keeps the original's sidebar routing (a forked
                    // chat conversation must stay in the Chat group). `Delegate`
                    // is unreachable here — children are never forked from the
                    // UI — but the invariant `delegate ⟺ parent_id set` wins
                    // over inheritance, so it degrades to `Regular`.
                    let sibling_kind = match current.kind {
                        ConversationKind::Delegate => ConversationKind::Regular,
                        ref kind => kind.clone(),
                    };

                    // UPDATE current row → S2. Writing external_id explicitly
                    // here closes the race against `refreshConversations()`
                    // after this fn returns; the lifecycle subscriber's later
                    // SessionStarted{S2} write is an idempotent no-op.
                    let mut active: conversation::ActiveModel = current.into();
                    if let Some(ref clean) = clean_title {
                        active.title = Set(Some(format!("[Fork] {clean}")));
                        // ...and lock it. The `[Fork] ` marker is codeg's own —
                        // no parser derives it from a transcript — so on an
                        // unlocked row it survives only until the next detail
                        // load, where the auto-title backfill adopts the
                        // session-file title and the two rows this fork just
                        // created end up wearing the SAME name. Forking is a
                        // deliberate user action on a conversation the user
                        // named (or accepted the name of), so treating the
                        // result as user-set is honest; the cost is that the
                        // forked row stops tracking the session file's title,
                        // exactly as a rename would. A titleless row writes no
                        // title here and stays unlocked, so the backfill can
                        // still give it its first name.
                        active.title_locked = Set(true);
                    }
                    active.external_id = Set(Some(forked_session_id));
                    active.updated_at = Set(now);
                    active.update(txn).await?;

                    // The lifecycle subscriber may have got here first. Its
                    // `SessionStarted{S2}` handler now runs the guarded
                    // `bind_external_id`, which preserves S1 on its own row
                    // when nothing else holds it — and that race is real, not
                    // theoretical: the fork reply is sent to us BEFORE
                    // `handle_fork_or_exit` emits `SessionStarted{S2}` (see
                    // `connection.rs`), so this detached persistence task and
                    // the lifecycle worker run concurrently from that point.
                    //
                    // Inserting a second S1 row would not merely duplicate it:
                    // `idx_conversation_external_agent` is UNIQUE over
                    // `(external_id, agent_type)`, so the INSERT would fail and
                    // roll back the whole fork. Adopt whatever already holds S1
                    // instead and return ITS id — the caller feeds that straight
                    // into `ForkResultInfo.sibling_conversation_id` and the
                    // sidebar upsert, both of which must name a real row.
                    if let Some(existing) = conversation::Entity::find()
                        .filter(conversation::Column::ExternalId.eq(original_session_id.clone()))
                        .filter(conversation::Column::AgentType.eq(agent_type_str.clone()))
                        .filter(conversation::Column::Id.ne(conversation_id))
                        .filter(conversation::Column::DeletedAt.is_null())
                        .one(txn)
                        .await?
                    {
                        return Ok(existing.id);
                    }

                    // INSERT sibling row preserving pre-fork (S1) history.
                    // PendingReview because no live agent is attached to S1.
                    // [crewchat] The team snapshot rides along: a fork of a
                    // group conversation is still that group's conversation.
                    let sibling_team = conversation::Entity::find_by_id(conversation_id)
                        .one(txn)
                        .await?
                        .and_then(|row| row.team);
                    let sibling = conversation::ActiveModel {
                        id: NotSet,
                        folder_id: Set(folder_id),
                        title: Set(clean_title),
                        title_locked: Set(title_locked),
                        agent_type: Set(agent_type_str),
                        status: Set(ConversationStatus::PendingReview),
                        kind: Set(sibling_kind),
                        model: Set(None),
                        git_branch: Set(git_branch),
                        external_id: Set(Some(original_session_id)),
                        parent_id: Set(None),
                        parent_tool_use_id: Set(None),
                        delegation_call_id: Set(None),
                        message_count: Set(0),
                        created_at: Set(now),
                        updated_at: Set(now),
                        deleted_at: Set(None),
                        pinned_at: Set(None),
                        origin_cwd: Set(None),
                        team: Set(sibling_team),
                    };
                    let inserted = sibling.insert(txn).await?;
                    // The sibling keeps the pre-fork history under the
                    // original's name, so it keeps the original's tags too —
                    // without them the user's tagging would appear to have
                    // fallen off the conversation they forked FROM.
                    crate::db::service::conversation_tag_service::copy_conversation_tags(
                        txn,
                        conversation_id,
                        inserted.id,
                    )
                    .await?;
                    Ok(inserted.id)
                })
            })
            .await
            .map_err(|e| AcpError::protocol(e.to_string()))
    }

    pub async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError> {
        let removed = {
            // The map lock is held ACROSS the handoff into `draining`, and
            // readers take it in the same order, so an observer can never see
            // the connection in neither place.
            let mut connections = self.connections.lock().await;
            let removed = connections.remove(conn_id);
            if let Some(conn) = &removed {
                self.park_draining(conn).await;
            }
            removed
        };
        if let Some(conn) = removed {
            tracing::info!("[ACP] disconnect connection={}", conn_id);
            let _ = conn.cmd_tx.send(ConnectionCommand::Disconnect).await;
            Ok(())
        } else {
            Err(AcpError::ConnectionNotFound(conn_id.into()))
        }
    }

    /// Remember a connection's child until it is provably finished. Call while
    /// holding the connections lock, immediately after removing the entry.
    async fn park_draining(&self, conn: &AgentConnection) {
        let mut draining = self.draining.lock().await;
        prune_reaped(&mut draining);
        draining.push(DrainingChild {
            agent: conn.agent_type,
            pid: conn.child_pid.clone(),
            parked_at: std::time::Instant::now(),
        });
    }

    /// Every agent that could still be writing to its own files: connected or
    /// prompting, plus any whose connection is gone but whose process is not.
    ///
    /// Taken under the connections lock so the `disconnect` handoff into
    /// `draining` is atomic from here — otherwise a restore could look between
    /// the two and conclude nothing is running.
    pub async fn live_or_draining_agent_names(&self) -> Vec<String> {
        let connections = self.connections.lock().await;
        let mut names: Vec<String> = connections
            .values()
            .filter(|c| {
                matches!(
                    c.status,
                    ConnectionStatus::Connecting
                        | ConnectionStatus::Connected
                        | ConnectionStatus::Prompting
                )
            })
            .map(|c| c.agent_type.to_string())
            .collect();
        let mut draining = self.draining.lock().await;
        prune_reaped(&mut draining);
        names.extend(draining.iter().map(|c| c.agent.to_string()));
        names.sort();
        names.dedup();
        names
    }

    /// Probe an agent for the modes / config_options it advertises on a fresh
    /// session, then immediately disconnect. The probe runs with
    /// `EventEmitter::Noop` so no event reaches the desktop webview, the
    /// global `WebEventBroadcaster`, or the `InternalEventBus` — the events
    /// land only in this probe connection's own (unsubscribed) per-connection
    /// stream and in its `SessionState` (which is the read source here).
    ///
    /// Used by the delegation-settings UI to enumerate the options the user
    /// can override, with the guarantee that what the UI shows is exactly
    /// what `codeg-mcp` will pass through to `session/set_config_option`
    /// when a delegation actually fires.
    ///
    /// `preferred_config_values` are applied on the probe session before the
    /// snapshot is read, exactly as a real launch would apply them: an agent
    /// that derives one option's choices from another's value (opencode
    /// re-lists `effort` per selected model) then answers for the user's
    /// selection instead of its own default. A value the agent rejects is
    /// logged and skipped — the snapshot still comes back. Each applied
    /// option itself still reports the agent's own pick as its current value
    /// (see [`report_agent_chosen_values`]).
    ///
    /// Returns `Ok(snapshot)` even when the agent advertises no options
    /// (empty `config_options`, `None` modes) — that's a valid outcome the
    /// UI can render as "this agent has nothing to configure."
    pub async fn probe_agent_options(
        &self,
        agent_type: AgentType,
        working_dir: Option<String>,
        runtime_env: BTreeMap<String, String>,
        preferred_config_values: BTreeMap<String, String>,
    ) -> Result<AgentOptionsSnapshot, AcpError> {
        // Owner window label is informational only (used for
        // disconnect_by_owner_window), but worth being explicit so a probe
        // connection that somehow leaks past the disconnect below is easy to
        // identify in logs / debug snapshots.
        let owner_window = "delegation-probe".to_string();
        // Serialize concurrent probes for the same agent_type. Rapid tab
        // switching in the settings UI would otherwise fan out one real
        // CLI process per click — each one running up to 60s. The mutex
        // bounds this to one in-flight probe per agent type; different
        // agent_types still probe in parallel.
        //
        // The outer `probe_locks` guard MUST be dropped BEFORE the
        // `.lock_owned().await` on the per-agent mutex. If we held it
        // across the await, a probe queued behind another for the SAME
        // agent_type would keep the outer map locked, blocking probes
        // for every OTHER agent_type too — silently turning the
        // per-agent serialization into a global one.
        let per_agent_lock: Arc<tokio::sync::Mutex<()>> = {
            let mut locks = self.probe_locks.lock().await;
            locks
                .entry(agent_type)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _probe_guard = per_agent_lock.lock_owned().await;
        let applied_ids: Vec<String> = preferred_config_values.keys().cloned().collect();
        let conn_id = self
            .spawn_agent(
                agent_type,
                working_dir,
                None, // brand-new session — no resume
                runtime_env,
                owner_window,
                EventEmitter::Noop,
                None,
                preferred_config_values,
            
            None,)
            .await?;

        // Hold an `Arc<RwLock<SessionState>>` alongside the manager's own
        // entry so the state survives even if the connection task cleans
        // up its map slot mid-handshake. Without this, an agent that
        // errors during Initialize would trigger cleanup before the
        // probe's poll loop sees the `AcpEvent::Error` payload, and
        // `wait_for_session_options` would surface the unhelpful
        // `ConnectionNotFound` instead of the agent's own error text.
        let state_arc = self.get_state(&conn_id).await;

        // Generous timeout because some agents (Gemini in particular) take
        // 8-10s just to answer Initialize before session/new can even start;
        // a tight cap here would consistently return an empty snapshot and
        // make the settings UI claim those agents have nothing to configure.
        // Matches the per-step Initialize timeout in `connection.rs`.
        let probe_timeout = Duration::from_secs(60);
        let raw_snapshot = self.wait_for_session_options(&conn_id, probe_timeout).await;

        // If the wait errored, prefer the agent's own captured error
        // message over the generic ProbeTimedOut / ConnectionNotFound —
        // an agent that died on Initialize already explained why.
        let snapshot = match raw_snapshot {
            Ok(mut s) => {
                if !applied_ids.is_empty() {
                    if let Some(state) = state_arc.as_ref() {
                        let agent_chosen = state.read().await.agent_chosen_config_values.clone();
                        report_agent_chosen_values(
                            &mut s.config_options,
                            &applied_ids,
                            &agent_chosen,
                        );
                    }
                }
                Ok(s)
            }
            Err(wait_err) => {
                let captured = if let Some(state) = state_arc.as_ref() {
                    state.read().await.last_error.clone()
                } else {
                    None
                };
                Err(match captured {
                    Some(err) => AcpError::protocol(err.message),
                    None => wait_err,
                })
            }
        };

        // Always disconnect — including on Err — so a failed probe doesn't
        // leak an agent process. Ignore disconnect errors (best-effort
        // cleanup; the agent will exit when its stdio is dropped anyway).
        let _ = self.disconnect(&conn_id).await;
        snapshot
    }

    /// Poll a connection's `SessionState` until the agent signals it has
    /// finished publishing its initial selectors (`SelectorsReady`), then
    /// give a small grace window for any tightly-following follow-up updates
    /// before snapshotting. Waiting on `selectors_ready` — not just
    /// `config_options.is_some()` — matters because some agents emit an
    /// empty `SessionConfigOptions` first and then push the real options
    /// in a subsequent update; returning on the first `Some(vec![])` would
    /// race ahead of those updates and report the agent as having nothing
    /// to configure.
    ///
    /// The `SessionConfigOptions` / `SelectorsReady` ACP events populate
    /// `SessionState` via `apply_event` regardless of which `EventEmitter`
    /// variant the connection uses — that's why the probe can rely on
    /// `Noop` and still observe the values here.
    ///
    /// Returns `AcpError::ProbeTimedOut` when the timeout elapses without
    /// `selectors_ready` ever flipping to `true`. Distinguishing that case
    /// from a clean "ready with no options" snapshot lets the UI tell the
    /// user "the agent never published its options — retry" instead of
    /// silently claiming the agent has nothing to configure.
    async fn wait_for_session_options(
        &self,
        conn_id: &str,
        timeout: Duration,
    ) -> Result<AgentOptionsSnapshot, AcpError> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(50);
        // Grace window between `selectors_ready` flipping true and the
        // snapshot we return. Lets a stragging `ConfigOptionUpdate` that
        // an agent emits in the same tick land before we read.
        let grace_period = Duration::from_millis(500);
        let mut selectors_ready_at: Option<std::time::Instant> = None;
        loop {
            let (
                config_options,
                modes,
                available_commands,
                prompt_capabilities,
                selectors_ready,
            ) = {
                let conns = self.connections.lock().await;
                let conn = conns
                    .get(conn_id)
                    .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.into()))?;
                let s = conn.state.read().await;
                (
                    s.config_options.clone(),
                    s.modes.clone(),
                    s.available_commands.clone(),
                    s.prompt_capabilities.clone(),
                    s.selectors_ready,
                )
            };
            if selectors_ready {
                let ready_at = *selectors_ready_at.get_or_insert_with(std::time::Instant::now);
                if ready_at.elapsed() >= grace_period {
                    // Commands ride along from the same probe session (the grace
                    // window lets a late `available_commands` land before we read).
                    return Ok(AgentOptionsSnapshot {
                        modes,
                        config_options: config_options.unwrap_or_default(),
                        available_commands,
                        prompt_capabilities,
                    });
                }
            }
            if start.elapsed() >= timeout {
                return Err(AcpError::ProbeTimedOut);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    pub async fn disconnect_by_owner_window(&self, owner_window_label: &str) -> usize {
        let cmd_txs = {
            let mut connections = self.connections.lock().await;
            let ids: Vec<String> = connections
                .iter()
                .filter_map(|(id, conn)| {
                    if conn.owner_window_label == owner_window_label {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            let mut txs = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(conn) = connections.remove(&id) {
                    // Same handoff as `disconnect`: closing a window leaves
                    // the agents exiting, not exited.
                    self.park_draining(&conn).await;
                    txs.push(conn.cmd_tx);
                }
            }
            txs
        };

        let disconnected = cmd_txs.len();
        for cmd_tx in cmd_txs {
            let _ = cmd_tx.send(ConnectionCommand::Disconnect).await;
        }
        tracing::info!(
            "[ACP] disconnect by owner window owner_window={} count={}",
            owner_window_label, disconnected
        );
        disconnected
    }

    /// End every live connection running `agent_type` and report how many there
    /// were, not returning until this agent's processes — the ones just ended
    /// AND any it already had exiting — have been put past running, as far as
    /// the OS lets that be established ([`kill_tree_and_wait`] is precise about
    /// where that stops).
    ///
    /// Added for the Antigravity sign-out, and the reason is the shape of that
    /// agent's credential store rather than anything about connections. An
    /// Antigravity process caches its OAuth credentials in memory and writes
    /// them back to the keychain (or token file) on every silent refresh,
    /// behind a lock that is per-PROCESS. So a connection still running while
    /// the credential is cleared will, at its next token expiry, helpfully
    /// restore the account the user just signed out of — or overwrite the one
    /// they signed in as afterwards.
    ///
    /// That is why this borrows [`Self::disconnect_all`]'s shape rather than
    /// [`Self::disconnect_by_owner_window`]'s, despite reading like the latter.
    /// The caller needs "those processes are gone", not "those processes were
    /// asked to go", and it differs from the shutdown backstop in the two ways
    /// that follow from exactly that:
    ///
    /// - It sweeps the `draining` list, not just the map. A connection an
    ///   earlier `disconnect` (or a closed window) already removed is invisible
    ///   in the map while its process is still exiting — and that process holds
    ///   the same in-memory credential. Only `draining` knows about it, and
    ///   nothing else would ever kill it.
    /// - It waits for each process to be past acting, escalating to `SIGKILL`;
    ///   see [`kill_tree_and_wait`] for what that does and does not establish.
    ///   `kill_tree` on its own returns as soon as `SIGTERM` is delivered, and
    ///   `SIGTERM` can be caught or ignored, so its return says nothing about
    ///   whether the agent is still there to write the credential back.
    ///
    /// Everything `disconnect_all` says about `try_send`, about sleeping the
    /// WHOLE grace window, and about loading each pid only afterwards applies
    /// verbatim. In particular the pid must not be read early: a connection
    /// still `Connecting` reads zero now and publishes a live pid moments later,
    /// and that is exactly the child that would go on to rewrite the credential.
    ///
    /// The return value counts the LIVE connections ended, not the processes
    /// swept — a drainer was disconnected by whoever parked it, and counting it
    /// again here would report a teardown that did not happen. A pid the sweep
    /// could not settle is logged, not raised. On unix that means `kill(2)`
    /// answered something other than `ESRCH` — in practice `EPERM`, i.e. that
    /// pid is not ours to signal, which points at a recycled number rather than
    /// a surviving agent, though it does not prove either. Weighed against
    /// refusing to return on it, which would strand the sign-out in exactly the
    /// dead end it exists to undo: the agent restoring a credential costs the
    /// user another press of the button, and never signing out costs them the
    /// account.
    ///
    /// Callers that need no new connection to appear behind them must hold
    /// [`Self::lock_out_new_connections`] BEFORE calling, as `disconnect_all`'s
    /// shutdown caller does by construction.
    pub async fn disconnect_by_agent_type(&self, agent_type: AgentType) -> usize {
        let cmd_txs: Vec<tokio::sync::mpsc::Sender<ConnectionCommand>> = {
            let mut connections = self.connections.lock().await;
            let ids: Vec<String> = connections
                .iter()
                .filter(|(_, conn)| conn.agent_type == agent_type)
                .map(|(id, _)| id.clone())
                .collect();

            let mut txs = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(conn) = connections.remove(&id) {
                    // Same handoff as every other teardown: the entry goes
                    // immediately, so the child is parked as draining to stay
                    // visible while it exits — and, here, to be swept below.
                    self.park_draining(&conn).await;
                    txs.push(conn.cmd_tx);
                }
            }
            txs
        };

        let disconnected = cmd_txs.len();
        for cmd_tx in &cmd_txs {
            let _ = cmd_tx.try_send(ConnectionCommand::Disconnect);
        }
        tracing::info!(
            "[ACP] disconnect by agent type agent={:?} count={}",
            agent_type, disconnected
        );

        // Every child that could still be running as this agent: the ones just
        // parked above AND the ones an earlier teardown left draining. Cells,
        // not pids — each pid is loaded at the last possible moment, inside the
        // sweep.
        let pid_cells: Vec<Arc<std::sync::atomic::AtomicU32>> = {
            let mut draining = self.draining.lock().await;
            prune_reaped(&mut draining);
            draining
                .iter()
                .filter(|c| c.agent == agent_type)
                .map(|c| c.pid.clone())
                .collect()
        };
        if pid_cells.is_empty() {
            return disconnected;
        }

        tokio::time::sleep(DISCONNECT_ALL_GRACE).await;

        match tokio::task::spawn_blocking(move || {
            pid_cells
                .iter()
                .filter(|cell| !kill_tree_and_wait(cell))
                .count()
        })
        .await
        {
            Ok(0) => {}
            Ok(unconfirmed) => tracing::warn!(
                "[ACP] disconnect by agent type agent={:?}: {} process(es) could not be confirmed gone",
                agent_type, unconfirmed
            ),
            // Only reachable if the sweep panicked or the runtime is shutting
            // down under it. Nothing to retry against — but it must not pass
            // silently for "everything was confirmed".
            Err(e) => tracing::warn!(
                "[ACP] disconnect by agent type agent={:?}: sweep did not finish: {e}",
                agent_type
            ),
        }

        disconnected
    }

    /// Disconnect every connection, then hard-kill any surviving agent process
    /// trees as a shutdown backstop.
    ///
    /// The graceful path (send `Disconnect` → the connection driver thread
    /// breaks its command loop → `run_connection` unwinds →
    /// `acp::agent_process`'s `ChildGuard::drop` runs `kill_tree`) is enough on its own
    /// *when it gets to run*. It doesn't at process exit: `run_connection` is
    /// driven on a dedicated `std::thread` (see `spawn_agent`), and when the
    /// quit handler returns the process terminates those threads
    /// mid-flight — often before the driver reaches `ChildGuard::drop` — so the
    /// agent CLI (and its own children, e.g. MCP servers / a forked `node`) is
    /// reparented and lingers until it independently notices its stdin EOF
    /// (~30s for Claude Code / node).
    ///
    /// So: fire every `Disconnect`, give the graceful path a short grace window,
    /// then synchronously `kill_tree` whatever is still running.
    ///
    /// What the window actually buys is NOT a gentler death for the agent — the
    /// graceful path ends in the same `kill_tree` — it is time for the driver
    /// thread's own post-loop cleanup (delegation-token revoke, `cancel_by_parent`
    /// for delegations / questions / plan approvals), which at quit previously
    /// got no time at all.
    ///
    /// Each pid is read from its live cell as late as possible — after the
    /// window, inside the kill loop — never from an up-front snapshot. That is
    /// load-bearing in both directions:
    /// - A connection whose process was reaped during the window has already
    ///   had its cell zeroed by the `on_exit` callback, so the backstop skips it
    ///   instead of `kill_tree`-ing a pid the OS may have recycled onto an
    ///   unrelated process tree. A connection that merely *ended* keeps its pid
    ///   and still gets swept — `ChildGuard::drop` signals without waiting, so
    ///   "the driver returned" is not "the agent is gone".
    /// - A connection whose child spawned *after* quit began (still `Connecting`
    ///   when the map was drained) publishes its pid into the same cell, so the
    ///   backstop still reaches it — an up-front snapshot would read `0` and
    ///   leak exactly the orphan this exists to kill.
    ///
    /// Only pids this manager spawned (and their descendants) are touched;
    /// `child_pid == 0` (never spawned, already finished, or a test/viewer entry
    /// that owns no process) is skipped.
    pub async fn disconnect_all(&self) -> usize {
        let handles: Vec<(
            tokio::sync::mpsc::Sender<ConnectionCommand>,
            Arc<std::sync::atomic::AtomicU32>,
        )> = {
            let mut connections = self.connections.lock().await;
            connections
                .drain()
                .map(|(_, conn)| (conn.cmd_tx, conn.child_pid))
                .collect()
        };
        let disconnected = handles.len();
        // `try_send`, not `send().await`: this is the shutdown path, and the
        // backstop below is only reachable if every send returns. A connection
        // whose command queue is full (32 deep) would otherwise park the whole
        // quit here — precisely the wedged connection whose tree most needs
        // killing. A dropped `Disconnect` just means that connection skips the
        // graceful path and gets hard-killed instead.
        for (cmd_tx, _) in &handles {
            let _ = cmd_tx.try_send(ConnectionCommand::Disconnect);
        }
        tracing::info!("[ACP] disconnect_all count={}", disconnected);

        if disconnected == 0 {
            return 0;
        }

        // Grace window: let the drivers unwind and run their own cleanup.
        tokio::time::sleep(DISCONNECT_ALL_GRACE).await;

        // Backstop: hard-kill whatever is still running. Runs on a blocking
        // thread so the synchronous `kill_tree` doesn't stall the async runtime
        // while a `block_on(disconnect_all())` shutdown caller waits on it.
        let pid_cells: Vec<Arc<std::sync::atomic::AtomicU32>> =
            handles.into_iter().map(|(_, pid)| pid).collect();
        let _ = tokio::task::spawn_blocking(move || {
            for cell in pid_cells {
                // Load here, not before the window — see the doc comment.
                let pid = cell.load(std::sync::atomic::Ordering::SeqCst);
                if pid == 0 {
                    continue;
                }
                match kill_tree::blocking::kill_tree(pid) {
                    Ok(_) => {
                        tracing::info!(
                            "[ACP] disconnect_all backstop killed process tree pid={pid}"
                        );
                    }
                    Err(e) => {
                        // The process can still exit between the load and the
                        // kill; that error is expected, not a failure.
                        tracing::debug!("[ACP] disconnect_all backstop kill_tree pid={pid}: {e}");
                    }
                }
            }
        })
        .await;

        disconnected
    }

    /// Block new connections from being established until the returned guard
    /// is dropped. The caller must enumerate live connections only AFTER
    /// holding this, never before.
    pub async fn lock_out_new_connections(
        &self,
    ) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.external_restore_lock.clone().write_owned().await
    }

    pub async fn list_connections(&self) -> Vec<ConnectionInfo> {
        let connections = self.connections.lock().await;
        connections.values().map(|c| c.info()).collect()
    }

    /// Raw per-connection rows for the pet panel's active-session list.
    /// "Active" = the connection is currently `Prompting`, awaiting a
    /// permission, or in an `Error` state — the sessions a user would want to
    /// see or act on from the floating pet. Idle `Connected` sessions are
    /// excluded to keep the list focused (mirrors the Codex pet "signal"
    /// model).
    ///
    /// `title` is left empty here: this layer has no DB handle. The command
    /// layer (`pet_list_active_sessions_core`) fills it from the conversation
    /// row. Connections without both a bound `conversation_id` and `folder_id`
    /// are skipped — the panel needs both to render a row and to navigate to
    /// it. Lock discipline mirrors `find_connection_by_conversation_id`: hold
    /// the connections mutex while taking each per-session read lock (the
    /// reads are microseconds and released each iteration).
    pub async fn list_active_sessions(&self) -> Vec<crate::models::pet::PetSessionEntry> {
        let connections = self.connections.lock().await;
        let mut out = Vec::new();
        for (id, conn) in connections.iter() {
            let state = conn.state.read().await;
            let (Some(conversation_id), Some(folder_id)) =
                (state.conversation_id, state.folder_id)
            else {
                continue;
            };
            let pending = state
                .pending_permission
                .as_ref()
                .map(crate::models::pet::PetPermissionSummary::from);
            let is_active = pending.is_some()
                || matches!(
                    state.status,
                    ConnectionStatus::Prompting | ConnectionStatus::Error
                );
            if !is_active {
                continue;
            }
            out.push(crate::models::pet::PetSessionEntry {
                connection_id: id.clone(),
                conversation_id,
                folder_id,
                agent_type: state.agent_type,
                title: String::new(),
                status: state.status.clone(),
                pending,
                // Same reason as `title`: resolving a delegation child's parent
                // needs the DB. Filled by `pet_list_active_sessions_core`.
                parent: None,
            });
        }
        out
    }

    /// Clone the `Arc<RwLock<SessionState>>` for a given connection id so the
    /// caller can read/write state without holding the connections mutex.
    /// Returns `None` if no such connection is registered.
    pub async fn get_state(
        &self,
        conn_id: &str,
    ) -> Option<std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>> {
        let connections = self.connections.lock().await;
        connections.get(conn_id).map(|conn| conn.state.clone())
    }

    /// Like `get_state`, but also clones the connection's `EventEmitter`.
    /// Used by the lifecycle subscriber when it needs to both update the
    /// per-session state and re-broadcast a derived event (e.g. emitting
    /// `ConversationStatusChanged` after writing the row's status).
    /// One short lock on the connections map; both pieces are cheap to clone.
    pub async fn get_state_and_emitter(
        &self,
        conn_id: &str,
    ) -> Option<(
        std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
        EventEmitter,
    )> {
        let connections = self.connections.lock().await;
        connections
            .get(conn_id)
            .map(|conn| (conn.state.clone(), conn.emitter.clone()))
    }

    /// Wait (bounded) for the connected agent to say what a prompt may carry.
    ///
    /// `spawn_agent` returns as soon as the process is up and registered: it
    /// only holds for `SessionStarted` when deduplicating a RESUME, so a fresh
    /// session's `initialize` — which is what publishes these capabilities
    /// (`emit_prompt_capabilities`) — is still in flight when it returns. A
    /// caller that must know the encoding before its first prompt therefore has
    /// to ask, rather than read the state and find `None`.
    ///
    /// `None` means the wait ran out or the connection went away; callers treat
    /// that as "no information" and send what they already had, never as an
    /// error — the prompt itself is about to surface any real problem.
    pub async fn wait_for_prompt_capabilities(
        &self,
        conn_id: &str,
        timeout: Duration,
    ) -> Option<PromptCapabilitiesInfo> {
        let state = {
            let connections = self.connections.lock().await;
            connections.get(conn_id)?.state.clone()
        };
        let start = std::time::Instant::now();
        loop {
            {
                let s = state.read().await;
                if let Some(caps) = s.prompt_capabilities.clone() {
                    return Some(caps);
                }
                // A connection that already died will never advertise anything.
                if matches!(
                    s.status,
                    ConnectionStatus::Disconnected | ConnectionStatus::Error
                ) {
                    return None;
                }
            }
            if start.elapsed() >= timeout {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Append a live-feedback note to a connection's session and broadcast it.
    ///
    /// Validation: the text is trimmed and rejected when empty
    /// ([`AcpError::InvalidFeedback`]) or longer than [`MAX_FEEDBACK_CHARS`] —
    /// the full text rides in the broadcast event, the snapshot, and the MCP
    /// response, so a sanity bound keeps one pathological note from bloating
    /// them. (There is deliberately no per-turn COUNT cap: the set is cleared
    /// every turn, so its size scales with human typing, not unboundedly.)
    ///
    /// Rejected with [`AcpError::NoActiveTurn`] unless a turn is in flight —
    /// feedback is mid-turn steering, pulled by the agent via the
    /// `check_user_feedback` MCP tool; with no active turn there is nothing to
    /// steer and the note would strand (the frontend falls back to an ordinary
    /// prompt). The append rides `emit_with_state` so `SessionState.feedback`,
    /// the ring buffer, and every attached client stay in lockstep.
    /// `blocks`, when present, is the full prompt-block draft (text plus
    /// image attachments) to deliver on the native wire instead of the bare
    /// `text` — `text` then serves as the recorded note. Only the native
    /// channel can carry blocks; see the pull-path gate below.
    pub async fn submit_feedback(
        &self,
        conn_id: &str,
        text: String,
        blocks: Option<Vec<PromptInputBlock>>,
    ) -> Result<FeedbackItem, AcpError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(AcpError::InvalidFeedback("empty note".into()));
        }
        if trimmed.chars().count() > MAX_FEEDBACK_CHARS {
            return Err(AcpError::InvalidFeedback(format!(
                "note exceeds {MAX_FEEDBACK_CHARS} characters"
            )));
        }
        let text = trimmed.to_string();
        let blocks = blocks.filter(|b| !b.is_empty());
        let (state, cmd_tx, emitter) = {
            let connections = self.connections.lock().await;
            let conn = connections
                .get(conn_id)
                .ok_or_else(|| AcpError::ConnectionNotFound(conn_id.to_string()))?;
            (
                conn.state.clone(),
                conn.cmd_tx.clone(),
                conn.emitter.clone(),
            )
        };
        // Channel-aware eligibility: a note is deliverable natively (the
        // `_session/steering` push, synthesized at initialize) or through the
        // `check_user_feedback` pull tool. Reject only when NEITHER channel
        // exists — e.g. a session launched before the feature was enabled (no
        // tool) on an adapter without proven native steering.
        // `native_steering_available` is re-read on every call so a
        // `startedNewTurn` downgrade mid-session silently reroutes later
        // notes to the pull path.
        let (native, tool_available) = {
            let s = state.read().await;
            (s.native_steering_available, s.feedback_tool_available)
        };
        if !native && !tool_available {
            return Err(AcpError::FeedbackDisabled);
        }

        if native {
            return Self::submit_feedback_native(conn_id, state, cmd_tx, emitter, text, blocks)
                .await;
        }

        // The pull tool delivers plain text (`PendingFeedback`), so a draft
        // carrying attachment blocks cannot ride it without silently dropping
        // them. Two ways to get here: an ordinary pull session (the composer
        // offers its mid-turn send on every session with a delivery channel,
        // so a codex/grok/gemini draft with an image lands right here), or a
        // native session that downgraded between the frontend's channel read
        // and this call (startedNewTurn latch). `NoActiveTurn` is the
        // rejection the caller already maps to its queue fallback, which
        // re-routes the WHOLE draft — attachments included — as the next
        // turn's prompt.
        if blocks.is_some() {
            return Err(AcpError::NoActiveTurn);
        }

        let item = FeedbackItem::new_pending(
            uuid::Uuid::new_v4().to_string(),
            text,
            chrono::Utc::now(),
        );
        // Gate on `turn_in_flight` and append in ONE critical section (via the
        // gated emit): a `TurnComplete` (flips the flag) or `UserMessage`
        // (clears `feedback`) can't slip between the gate and the append+seq, so
        // a note is never stranded on a finished turn nor re-added to a new one.
        let applied = emit_with_state_gated(
            &state,
            &emitter,
            AcpEvent::FeedbackSubmitted { item: item.clone() },
            |s| s.turn_in_flight,
        )
        .await;
        if !applied {
            return Err(AcpError::NoActiveTurn);
        }
        Ok(item)
    }

    /// Native `_session/steering` delivery — the push half of
    /// [`Self::submit_feedback`], cancellation-shielded.
    ///
    /// SHIELD (fork_session's pattern): once `ConnectionCommand::Steer` is
    /// enqueued, the connection loop performs the injection REGARDLESS of
    /// whether this caller survives. An HTTP caller disconnecting mid-await
    /// must not leave "injected but unrecorded" state — no `FeedbackItem`, no
    /// broadcast, absent from reconnect snapshots — so enqueue → outcome →
    /// record → emit runs in a DETACHED task and the caller merely awaits the
    /// JoinHandle to relay the result to a still-live client.
    ///
    /// Invariants (asserted by tests):
    /// * NO DOUBLE DELIVERY. A natively-injected note is `Delivered` from
    ///   birth, and `read_pending_feedback` only returns `Pending` — so even
    ///   with the `check_user_feedback` tool still injected (it is, by
    ///   design), a pull can never hand the agent the same text again.
    /// * UNGATED EMIT. The pull path's `emit_with_state_gated(turn_in_flight)`
    ///   is exactly wrong here: the adapter's `injected` reply is the
    ///   authoritative fact the content reached the turn. Gating on a
    ///   just-settled turn would misreport `NoActiveTurn`, and the caller
    ///   would resubmit the same text as a prompt → duplicate. A delivered
    ///   note recorded right after `TurnComplete` is harmless — the notes
    ///   list renders only while prompting, and the next turn's `UserMessage`
    ///   clears `feedback`.
    /// * `created_at` PRECEDES THE INJECTION. It is taken before the `Steer`
    ///   command is enqueued, so it is earlier than any transcript entry the
    ///   injection can cause. The frontend relies on that ordering to tell the
    ///   agent's own copy of the message from the same words sent in an
    ///   earlier round (`suppressPersistedSteeredPrompts`).
    async fn submit_feedback_native(
        conn_id: &str,
        state: Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
        cmd_tx: tokio::sync::mpsc::Sender<ConnectionCommand>,
        emitter: EventEmitter,
        text: String,
        blocks: Option<Vec<PromptInputBlock>>,
    ) -> Result<FeedbackItem, AcpError> {
        // Cheap pre-flight, NOT the authoritative check (that's the loop's
        // idle arm replying `NoActiveTurn`): skip the round-trip when no turn
        // is in flight at all. The counter read alongside it identifies WHICH
        // turn this steer was admitted against — see the re-check below.
        let admitted_turns_completed = {
            let s = state.read().await;
            if !s.turn_in_flight {
                return Err(AcpError::NoActiveTurn);
            }
            s.turns_completed
        };
        // The wire payload: the caller's full draft when it carried blocks
        // (attachments included), else the recorded text as a single block —
        // byte-identical to the historical text-only steer. Uploaded-image
        // markers (web / remote mode) are re-hydrated exactly like a prompt's,
        // AFTER the admission checks above so a rejected steer never triggers
        // file reads, and BEFORE the shield below so a failure aborts with no
        // side effects.
        // Whether the caller sent a real draft rather than bare text. Read
        // BEFORE `blocks` is consumed below, and the sole gate on recording a
        // block list at all: a text-only steer must keep producing exactly the
        // note it always produced.
        let had_blocks = blocks.is_some();
        let wire_blocks = match blocks {
            Some(mut blocks) => {
                crate::acp::prompt_hydration::hydrate_prompt_blocks(
                    &mut blocks,
                    &crate::paths::codeg_uploads_root(),
                )
                .await?;
                // Hydration is the ONLY await this path puts between admission
                // and the enqueue, and it runs for as long as reading the
                // uploads takes. The loop's idle arm already covers "the turn
                // ended" (it replies `NoActiveTurn`), but it cannot cover "the
                // NEXT turn started in the meantime": the loop would then be
                // in its active arm and inject the note into a turn the user
                // never aimed at, recorded `Delivered` while the composer
                // clears. Re-check the admitted turn's identity so that case
                // takes the caller's queue fallback instead — which re-routes
                // the whole draft, attachment included.
                let changed = {
                    let s = state.read().await;
                    steered_turn_changed(
                        admitted_turns_completed,
                        s.turn_in_flight,
                        s.turns_completed,
                    )
                };
                if changed {
                    return Err(AcpError::NoActiveTurn);
                }
                blocks
            }
            None => vec![PromptInputBlock::Text { text: text.clone() }],
        };
        // Project what the user sent for the broadcast, AFTER hydration and
        // from the same bytes the agent gets — the contract an ordinary prompt
        // already follows (`user_blocks_from_prompt` at the `UserMessage`
        // emit). Without it the note reaches the live transcript as `text`
        // alone, which is the composer's DISPLAY form: a steered image would
        // render as words about an image until a reload replaced it with the
        // agent's own copy.
        //
        // `None` for a text-only steer, so that path's note is unchanged and
        // the frontend keeps rendering it from `text`.
        //
        // Filtered on the ATTACHMENT bytes rather than on `blocks.is_some()`: a
        // draft that projects down to text alone says nothing `text` does not
        // already say, and recording a list for it would put one on a note the
        // field's contract calls text-only.
        //
        // No budget check here on purpose — the per-turn attachment bound lives
        // at the single authorized writer (`SessionState::apply_event`'s
        // `FeedbackSubmitted` arm), where the check and the append share one
        // critical section. Enforcing it here would be a read, an agent
        // round-trip, then a write: concurrent steers would both read the same
        // total and both pass it.
        let record_blocks = had_blocks
            .then(|| crate::acp::user_blocks_from_prompt(&wire_blocks))
            .filter(|projected| crate::acp::feedback::attachment_bytes(projected) > 0);
        let conn_id_for_task = conn_id.to_string();
        let handle = tokio::spawn(async move {
            let outcome: Result<FeedbackItem, AcpError> = async {
                // Stamped BEFORE the command goes out, so the note's instant is
                // causally earlier than anything the injection can cause. The
                // adapter pushes the text to the agent before it answers, and
                // the agent may write its own transcript copy of the message
                // while this task is still awaiting that answer — a note
                // stamped on the way back would then look NEWER than the copy
                // it produced, and the frontend (which folds a persisted copy
                // away only when it postdates the injection — see
                // `suppressPersistedSteeredPrompts`) would show the message
                // twice. Same clock, same host: `created_at` is when the note
                // was created, which is also what the pull path records.
                let created_at = chrono::Utc::now();
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                cmd_tx
                    .send(ConnectionCommand::Steer {
                        blocks: wire_blocks,
                        reply: reply_tx,
                    })
                    .await
                    .map_err(|_| AcpError::ProcessExited)?;
                let steer = reply_rx.await.map_err(|_| {
                    AcpError::protocol("Steer reply channel closed".to_string())
                })??;
                match steer {
                    // Honored opt-in: the content was NOT consumed and is
                    // still host-owned. Surface the frontend's existing
                    // turn-end fallback (resubmit as a normal prompt).
                    SteerOutcome::PromptRequired => return Err(AcpError::NoActiveTurn),
                    SteerOutcome::Injected => {}
                    SteerOutcome::StartedNewTurn => {
                        // The adapter ignored the opt-in and detached a turn
                        // (stale binary lying about its version?). The content
                        // IS consumed — record it delivered, NEVER resend —
                        // but this adapter can't be trusted with the idle race
                        // again: downgrade to the MCP pull channel for the
                        // rest of the session.
                        tracing::warn!(
                            "[ACP][feedback] _session/steering returned startedNewTurn \
                             (conn={conn_id_for_task}); downgrading native steering for \
                             this session"
                        );
                        state.write().await.native_steering_available = false;
                    }
                }
                let item = FeedbackItem::new_delivered(
                    uuid::Uuid::new_v4().to_string(),
                    text,
                    created_at,
                    record_blocks,
                );
                // Ungated on purpose — see the invariant on this fn's doc.
                emit_with_state(
                    &state,
                    &emitter,
                    AcpEvent::FeedbackSubmitted { item: item.clone() },
                )
                .await;
                Ok(item)
            }
            .await;
            // Surface real failures even when the caller is gone (the
            // detached task's Result would otherwise drop silently).
            // `NoActiveTurn` is an expected reroute, not a failure.
            if let Err(ref e) = outcome {
                if !matches!(e, AcpError::NoActiveTurn) {
                    tracing::error!(
                        "[ACP][ERROR] native feedback submit failed (conn={conn_id_for_task}): {e}"
                    );
                }
            }
            outcome
        });
        match handle.await {
            Ok(result) => result,
            Err(join_err) => Err(AcpError::protocol(format!(
                "steer task did not complete: {join_err}"
            ))),
        }
    }

    /// Read the pending feedback for a connection WITHOUT marking it delivered.
    /// Returns an immediate snapshot. Read-only — backs the READ half of the
    /// `check_user_feedback` round-trip so the listener can commit delivery only
    /// after the response is actually written (a dropped / failed write leaves
    /// the notes pending for the agent's next check).
    pub async fn read_pending_feedback(&self, conn_id: &str) -> Vec<PendingFeedback> {
        let Some(state) = self.get_state(conn_id).await else {
            return Vec::new();
        };
        let pending: Vec<PendingFeedback> = {
            let s = state.read().await;
            s.feedback
                .iter()
                .filter(|f| f.status == FeedbackStatus::Pending)
                .map(|f| PendingFeedback {
                    id: f.id.clone(),
                    text: f.text.clone(),
                    created_at: f.created_at,
                })
                .collect()
        };
        bounded_feedback_batch(pending, MAX_FEEDBACK_RESPONSE_BYTES)
    }

    /// Mark the named notes `Delivered` and broadcast the consumption. Called by
    /// the listener ONLY after the `check_user_feedback` response was written to
    /// the companion, so a dropped / failed write leaves the notes pending and
    /// the agent's next check re-delivers them (at-least-once).
    ///
    /// Delivery boundary: "delivered" means the response reached the agent's MCP
    /// companion over the UDS. The one remaining hop (companion → agent stdout)
    /// can only fail when the agent process is gone/closing — i.e. the turn is
    /// being torn down, at which point the note is moot (the agent won't act on
    /// it). A mid-wait cancel is already handled upstream by the listener's
    /// peer-close race (no commit), and a cancel after the round-trip completes
    /// cannot suppress the response (the companion's inflight entry is already
    /// consumed). So this is the right boundary for a best-effort steering
    /// side-channel; an end-to-end ack would only cover the moot teardown tail.
    ///
    /// The mark happens under a single write lock; only notes still `Pending`
    /// flip (idempotent — a repeated commit, or a note already consumed by a
    /// racing call, is skipped) and only the ids actually flipped are emitted,
    /// so a double-commit can't double-broadcast.
    pub async fn commit_feedback_delivered(&self, conn_id: &str, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let Some((state, emitter)) = self.get_state_and_emitter(conn_id).await else {
            return;
        };
        let id_set: std::collections::HashSet<&String> = ids.iter().collect();
        let delivered_at = chrono::Utc::now();
        let marked: Vec<String> = {
            let mut s = state.write().await;
            let mut marked = Vec::new();
            for f in s.feedback.iter_mut() {
                if f.status == FeedbackStatus::Pending && id_set.contains(&f.id) {
                    f.status = FeedbackStatus::Delivered;
                    f.delivered_at = Some(delivered_at);
                    marked.push(f.id.clone());
                }
            }
            marked
        };
        if !marked.is_empty() {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::FeedbackConsumed {
                    ids: marked,
                    delivered_at,
                },
            )
            .await;
        }
    }

    /// Register a blocking `ask_user_question` on a connection: park a one-shot
    /// in `pending_questions` keyed by a fresh `question_id`, broadcast the
    /// `QuestionRequest` (so every attached client renders the interactive card
    /// and a mid-turn attach recovers it from the snapshot), and hand the
    /// receiver back to the listener to await. `None` when the connection is
    /// gone (nothing to ask) OR when this connection already has a pending ask
    /// — see below.
    ///
    /// One pending ask per connection: `SessionState.pending_question` and the
    /// frontend card are single slots, so a second concurrent ask would
    /// overwrite the first's card/snapshot and orphan the first (still-parked)
    /// tool call with no way to answer it. A single agent is blocked in its
    /// `ask_user_question` call and cannot issue a second, so this only guards a
    /// parallel / misbehaving MCP client; the refused second call resolves as
    /// `declined` (the listener's None path) so its agent proceeds with its own
    /// judgment instead of hanging. The check + insert are atomic under the
    /// registry lock.
    pub async fn register_question(
        &self,
        conn_id: &str,
        questions: Vec<QuestionSpec>,
    ) -> Option<RegisteredQuestion> {
        // Defense-in-depth: the companion validates, but the broker socket is
        // only token-gated, so refuse to broadcast malformed/oversized specs
        // (None → the listener declines the ask, as for any other None path).
        if crate::acp::question::validate_specs(&questions).is_err() {
            return None;
        }
        let (state, emitter) = self.get_state_and_emitter(conn_id).await?;
        let question_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut reg = self.pending_questions.lock().await;
            if reg.values().any(|e| e.parent_connection_id == conn_id) {
                return None;
            }
            reg.insert(
                question_id.clone(),
                PendingQuestionEntry {
                    parent_connection_id: conn_id.to_string(),
                    questions: questions.clone(),
                    sender: tx,
                },
            );
        }
        // Ungated emit: the agent is blocked in the tool call, so the card must
        // show regardless of any turn-flag timing.
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::QuestionRequest {
                question_id: question_id.clone(),
                questions,
            },
        )
        .await;
        // Teardown event-ordering race: `cancel_questions_by_parent` may have
        // drained this entry between the insert above and the emit just now. The
        // QuestionRequest we broadcast would then have no waiter, and the sweep's
        // QuestionResolved may have raced ahead of it — leaving a card up with no
        // live backend waiter. Emit a compensating QuestionResolved (ordered after
        // our QuestionRequest) and decline. (The listener's post-register token
        // re-check covers the complementary case: a register that lands entirely
        // after the sweep, which this presence check would not catch.)
        if self
            .compensate_if_question_drained(&question_id, &state, &emitter)
            .await
        {
            return None;
        }
        Some(RegisteredQuestion {
            question_id,
            answer_rx: rx,
        })
    }

    /// Returns `true` — after emitting a clearing `QuestionResolved` — when
    /// `question_id` is no longer pending, i.e. a teardown sweep drained it in the
    /// window after its `QuestionRequest` was broadcast. The compensating event is
    /// ordered after the request so no client keeps a card with no live backend
    /// waiter. Returns `false` (no emit) while the entry is still parked.
    async fn compensate_if_question_drained(
        &self,
        question_id: &str,
        state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
        emitter: &EventEmitter,
    ) -> bool {
        if self.pending_questions.lock().await.contains_key(question_id) {
            return false;
        }
        emit_with_state(
            state,
            emitter,
            AcpEvent::QuestionResolved {
                question_id: question_id.to_string(),
            },
        )
        .await;
        true
    }

    /// Resolve a pending `ask_user_question` with the user's submission (from any
    /// client). Removes the one-shot atomically (first answer wins; a duplicate /
    /// already-resolved id is an idempotent no-op), sends the self-describing
    /// outcome to the blocked listener, and broadcasts `QuestionResolved` so the
    /// card clears on every client. Routing uses the entry's stored parent
    /// connection (the `question_id` is the authoritative key), so a stale
    /// `conn_id` from the caller can't misroute.
    pub async fn answer_question(
        &self,
        conn_id: &str,
        question_id: &str,
        answer: QuestionAnswer,
    ) -> Result<(), AcpError> {
        let _ = conn_id;
        let entry = self.pending_questions.lock().await.remove(question_id);
        let Some(entry) = entry else {
            // Already answered / canceled / gone elsewhere — idempotent success.
            return Ok(());
        };
        let outcome = build_outcome(&entry.questions, &answer);
        // Ignore a dropped receiver: the listener may have abandoned the wait
        // (peer-close) at the same instant; the resolved-event below still clears
        // the card.
        let _ = entry.sender.send(outcome);
        if let Some((state, emitter)) = self.get_state_and_emitter(&entry.parent_connection_id).await
        {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::QuestionResolved {
                    question_id: question_id.to_string(),
                },
            )
            .await;
        }
        Ok(())
    }

    /// Cancel a pending `ask_user_question` — the companion's tool call was
    /// canceled (peer-close) or the connection is tearing down. Removes the
    /// one-shot (dropping the sender unblocks the listener with a declined
    /// outcome) and broadcasts `QuestionResolved` so the card clears. No-op if
    /// the question was already answered / gone.
    pub async fn cancel_question(&self, conn_id: &str, question_id: &str) {
        let _ = conn_id;
        let removed = self.pending_questions.lock().await.remove(question_id);
        let Some(entry) = removed else {
            return;
        };
        if let Some((state, emitter)) = self.get_state_and_emitter(&entry.parent_connection_id).await
        {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::QuestionResolved {
                    question_id: question_id.to_string(),
                },
            )
            .await;
        }
    }

    /// Cancel every pending `ask_user_question` parked on a connection that is
    /// tearing down. The `run_connection` cleanup guard calls this (alongside
    /// the delegation `DelegationBroker::cancel_by_parent` cascade) so question
    /// entries — and the listener tasks parked on them — are reclaimed
    /// synchronously on disconnect, instead of lingering until the companion's
    /// ask socket happens to close. Dropping each entry's sender unblocks its
    /// listener with a declined outcome; the `QuestionResolved` broadcast clears
    /// the card on every client. No-op when nothing is pending for this parent.
    pub async fn cancel_questions_by_parent(&self, conn_id: &str) {
        // Remove every entry for this parent under the lock (dropping their
        // senders unblocks the parked listeners), then emit outside the lock —
        // the registry mutex is never held across an await.
        let drained: Vec<String> = {
            let mut reg = self.pending_questions.lock().await;
            let ids: Vec<String> = reg
                .iter()
                .filter(|(_, e)| e.parent_connection_id == conn_id)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                reg.remove(id);
            }
            ids
        };
        if drained.is_empty() {
            return;
        }
        // Best-effort card clear: depending on the teardown path the connection
        // may already be out of the map (`disconnect` removes it before the
        // run_connection cleanup guard fires this sweep), so tolerate `None` — the
        // core removal above already ran and the frontend clears on disconnect.
        if let Some((state, emitter)) = self.get_state_and_emitter(conn_id).await {
            for question_id in drained {
                emit_with_state(&state, &emitter, AcpEvent::QuestionResolved { question_id }).await;
            }
        }
    }

    /// Register a pending Grok `exit_plan_mode` approval on `conn_id`, broadcast
    /// `PlanApprovalRequest` (so every attached client renders the card and a
    /// mid-turn attach recovers it from the snapshot), and return the receiver
    /// the connection's ext handler awaits. `None` when the connection is gone
    /// (nothing to approve) OR when one is already pending on it (the agent is
    /// blocked in a single `exit_plan_mode` call; a second would orphan the
    /// first). Mirrors [`Self::register_question`].
    pub async fn register_plan_approval(
        &self,
        conn_id: &str,
        tool_call_id: String,
        plan_markdown: String,
    ) -> Option<RegisteredPlanApproval> {
        let (state, emitter) = self.get_state_and_emitter(conn_id).await?;
        let approval_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut reg = self.pending_plan_approvals.lock().await;
            if reg.values().any(|e| e.parent_connection_id == conn_id) {
                return None;
            }
            reg.insert(
                approval_id.clone(),
                PendingPlanApprovalEntry {
                    parent_connection_id: conn_id.to_string(),
                    sender: tx,
                },
            );
        }
        // Ungated emit: the agent is blocked in the tool call, so the card must
        // show regardless of any turn-flag timing.
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::PlanApprovalRequest {
                approval_id: approval_id.clone(),
                tool_call_id,
                plan_markdown,
            },
        )
        .await;
        // Teardown event-ordering race (mirrors `register_question`): the
        // cleanup guard's `cancel_plan_approvals_by_parent` may have drained this
        // entry between the insert above and the emit just now — its
        // `PlanApprovalResolved` could then have raced ahead of our
        // `PlanApprovalRequest`, leaving a card up with no live backend waiter.
        // Emit a compensating `PlanApprovalResolved` (ordered after our request)
        // and decline.
        if self
            .compensate_if_plan_approval_drained(&approval_id, &state, &emitter)
            .await
        {
            return None;
        }
        Some(RegisteredPlanApproval {
            approval_id,
            answer_rx: rx,
        })
    }

    /// Returns `true` — after emitting a clearing `PlanApprovalResolved` — when
    /// `approval_id` is no longer pending, i.e. a teardown sweep drained it in the
    /// window after its `PlanApprovalRequest` was broadcast. Mirrors
    /// [`Self::compensate_if_question_drained`].
    async fn compensate_if_plan_approval_drained(
        &self,
        approval_id: &str,
        state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
        emitter: &EventEmitter,
    ) -> bool {
        if self.pending_plan_approvals.lock().await.contains_key(approval_id) {
            return false;
        }
        emit_with_state(
            state,
            emitter,
            AcpEvent::PlanApprovalResolved {
                approval_id: approval_id.to_string(),
            },
        )
        .await;
        true
    }

    /// Resolve a pending plan approval with the user's decision (from any
    /// client). Removes the one-shot atomically (first answer wins; a duplicate /
    /// already-resolved id is an idempotent no-op), sends the decision to the
    /// blocked ext handler, and broadcasts `PlanApprovalResolved` so the card
    /// clears on every client. Routing uses the entry's stored parent connection
    /// (the `approval_id` is the authoritative key), so a stale `conn_id` from the
    /// caller can't misroute. Mirrors [`Self::answer_question`].
    pub async fn answer_plan_approval(
        &self,
        conn_id: &str,
        approval_id: &str,
        answer: PlanApprovalAnswer,
    ) -> Result<(), AcpError> {
        let _ = conn_id;
        let entry = self.pending_plan_approvals.lock().await.remove(approval_id);
        let Some(entry) = entry else {
            // Already answered / canceled / gone elsewhere — idempotent success.
            return Ok(());
        };
        // Ignore a dropped receiver: the handler may have abandoned the wait
        // (teardown) at the same instant; the resolved event below still clears
        // the card.
        let _ = entry.sender.send(answer);
        if let Some((state, emitter)) =
            self.get_state_and_emitter(&entry.parent_connection_id).await
        {
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::PlanApprovalResolved {
                    approval_id: approval_id.to_string(),
                },
            )
            .await;
        }
        Ok(())
    }

    /// Cancel every pending plan approval parked on a connection that is tearing
    /// down (the `run_connection` cleanup guard calls this). Dropping each
    /// entry's sender resolves the handler's await as a disconnect (Grok keeps
    /// plan mode active, re-surfaces on reconnect); the `PlanApprovalResolved`
    /// broadcast clears the card. Mirrors [`Self::cancel_questions_by_parent`].
    pub async fn cancel_plan_approvals_by_parent(&self, conn_id: &str) {
        let drained: Vec<String> = {
            let mut reg = self.pending_plan_approvals.lock().await;
            let ids: Vec<String> = reg
                .iter()
                .filter(|(_, e)| e.parent_connection_id == conn_id)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                reg.remove(id);
            }
            ids
        };
        if drained.is_empty() {
            return;
        }
        // Best-effort card clear: the connection may already be out of the map
        // (disconnect removes it before this sweep), so tolerate `None`.
        if let Some((state, emitter)) = self.get_state_and_emitter(conn_id).await {
            for approval_id in drained {
                emit_with_state(&state, &emitter, AcpEvent::PlanApprovalResolved { approval_id })
                    .await;
            }
        }
    }

    /// Resolve a conversation_id to its currently-active connection id, if any.
    /// Used by the by-conversation snapshot endpoint and the LifecycleSubscriber.
    /// Per-session state is acquired via `read().await` to avoid the
    /// `try_read`-skip false negative that would intermittently return None
    /// while `emit_with_state` is mid-update — the wait is microseconds.
    pub async fn find_connection_by_conversation_id(&self, conversation_id: i32) -> Option<String> {
        let connections = self.connections.lock().await;
        for (id, conn) in connections.iter() {
            let state = conn.state.read().await;
            if state.conversation_id == Some(conversation_id) {
                return Some(id.clone());
            }
        }
        None
    }

    /// The in-flight user prompt for `conversation_id` and the instant its turn
    /// started, if a turn is currently running on its live connection. `Some`
    /// exactly between `UserMessage` and `TurnComplete` (see
    /// `SessionState.pending_user_message` / `pending_user_message_started_at`);
    /// `None` when no connection is bound to the conversation or no turn is in
    /// flight.
    ///
    /// Used by the detail endpoint to stamp the persisted in-flight user turn
    /// with the broadcast `message_id`, so a cross-client viewer's synthesized
    /// turn (keyed by that same id) dedups against it instead of rendering a
    /// second copy. The start instant lets the matcher tell the in-flight prompt
    /// apart from a prior identical one. One lock pass over the connections map,
    /// mirroring `find_connection_by_conversation_id`.
    pub async fn pending_user_message_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Option<(
        crate::acp::session_state::PendingUserMessage,
        Option<chrono::DateTime<chrono::Utc>>,
    )> {
        let connections = self.connections.lock().await;
        for conn in connections.values() {
            let state = conn.state.read().await;
            if state.conversation_id == Some(conversation_id) {
                return state
                    .pending_user_message
                    .clone()
                    .map(|pending| (pending, state.pending_user_message_started_at));
            }
        }
        None
    }

    /// Resolve an `(external_id, agent_type)` (agent session) to its
    /// currently-active connection id, if any. Sibling to
    /// `find_connection_by_conversation_id`, used as the discovery fallback for
    /// the cross-client viewer attach: a connection binds its `conversation_id`
    /// only on the first prompt, but its `external_id` is set as soon as the
    /// session starts — so for a historical conversation opened by a second
    /// client *before* anyone has sent a prompt, the by-conversation lookup
    /// misses while this one still finds the live owner, letting the second
    /// client attach as a viewer instead of reusing the connection as a
    /// (mis-tagged) owner and later tearing it down.
    ///
    /// `agent_type` is part of the match because `external_id` is unique only
    /// per agent (`UNIQUE(external_id, agent_type)`), not globally — without it,
    /// a session id shared across two agents could attach a viewer to the wrong
    /// agent's connection.
    pub async fn find_connection_by_external_id(
        &self,
        external_id: &str,
        agent_type: AgentType,
    ) -> Option<String> {
        let connections = self.connections.lock().await;
        for (id, conn) in connections.iter() {
            if conn.agent_type != agent_type {
                continue;
            }
            let state = conn.state.read().await;
            if state.external_id.as_deref() == Some(external_id) {
                return Some(id.clone());
            }
        }
        None
    }
}

/// Production impl of `ConnectionSpawner` used by `DelegationBroker`.
///
/// Bundles `Arc<ConnectionManager>` with `Arc<AppDatabase>` because
/// `cancel` writes the cancelled status onto the conversation row, which
/// happens inside `ConnectionManager::cancel`. The wrapper exists so the
/// broker can depend on a small `dyn`-able interface instead of pulling
/// in the full `AppState` graph.
///
/// `data_dir` is required so `spawn` can build a runtime env that
/// includes the git credential helper — without it, delegated subagents
/// fail any git command that depends on the codeg-injected helper.
#[derive(Clone)]
pub struct ConnectionManagerSpawner {
    pub manager: Arc<ConnectionManager>,
    pub db: Arc<AppDatabase>,
    pub data_dir: Arc<PathBuf>,
}

#[async_trait::async_trait]
impl crate::acp::delegation::spawner::ConnectionSpawner for ConnectionManagerSpawner {
    async fn spawn(
        &self,
        parent_connection_id: &str,
        agent_type: AgentType,
        working_dir: Option<String>,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
        mut role: Option<crate::acp::delegation::spawner::RoleSpawnContext>,
    ) -> Result<String, crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        // Resolve the parent connection so we can inherit its emitter and
        // owner_window. Falling back is not safe: a child whose emitter is
        // wired to a different broadcaster would emit events the frontend
        // never sees.
        let (emitter, owner_window, parent_working_dir) = {
            let conns = self.manager.connections.lock().await;
            let parent = conns.get(parent_connection_id).ok_or_else(|| {
                SpawnerError::Spawn(format!(
                    "parent connection {parent_connection_id} not found"
                ))
            })?;
            let pwd = {
                let s = parent.state.read().await;
                s.working_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
            };
            (
                parent.emitter.clone(),
                parent.owner_window_label.clone(),
                pwd,
            )
        };
        let effective_working_dir = working_dir.or(parent_working_dir);

        // Build the same runtime env `acp_connect` would build for a
        // user-initiated session — disabled check, settings overrides,
        // model provider creds, git helper. Without this, delegated
        // subagents would skip the user's configuration entirely.
        let mut runtime_env = crate::commands::acp::build_session_runtime_env(
            &self.db,
            agent_type,
            None,
            self.data_dir.as_path(),
        )
        .await
        .map_err(|e| SpawnerError::Spawn(e.to_string()))?;

        // [crewchat] The role's brain model, pinned the way THIS body can be
        // pinned: an overlay home the CLI reads its model from at startup
        // (claude: `CLAUDE_CONFIG_DIR`; codex: `CODEX_HOME`). Every body that
        // cannot be pinned says so instead of quietly running the CLI's own
        // model — a roster pick that is not in effect must be visible in the
        // logs, not assumed.
        if let Some(context) = role.as_mut() {
            let outcome = crate::agent_host::prepare_role_overlay(
                &self.data_dir.join("role-sessions"),
                agent_type,
                &context.role_id,
                &context.model_provider,
                &context.model,
            )
            .await;
            match outcome {
                crate::agent_host::OverlayOutcome::Applied(overlay) => {
                    runtime_env.extend(overlay.env);
                    context.model_applied = true;
                    tracing::info!(
                        "[crewchat][role] member session runs on the role model {} \
                         (home {})",
                        overlay.model_label,
                        overlay.home.display()
                    );
                }
                crate::agent_host::OverlayOutcome::Unsupported(reason) => {
                    tracing::warn!(
                        "[crewchat][role] role {} cannot be pinned on this body ({reason}); \
                         the session runs on the CLI's own model",
                        context.role_id
                    );
                }
                crate::agent_host::OverlayOutcome::Unavailable(reason) => {
                    tracing::warn!(
                        "[crewchat][role] role {} model not applied ({reason}); \
                         the session runs on the CLI's own model",
                        context.role_id
                    );
                }
            }
        }

        // [crewchat] Phase 4.6 — the member's Agent page decides its permission
        // preset and whether it may be asked at all. Read straight from the
        // roster row rather than carried on `RoleSpawnContext`: that keeps the
        // permission fact in ONE place (the Agent) and needs no new plumbing
        // through the delegation types. The role wins over whatever the broker
        // suggested, because that IS the user's decision; an empty role mode
        // leaves the broker's value (today: none) in place.
        let role_permissions = match role.as_ref() {
            Some(context) => crate::db::service::agent_role_service::get_role(
                &self.db.conn,
                &context.role_id,
            )
            .await
            .ok()
            .flatten()
            .map(|row| (row.permission_mode.trim().to_string(), row.auto_approve)),
            None => None,
        };
        let preferred_mode_id = role_permissions
            .as_ref()
            .map(|(mode, _)| mode.as_str())
            .filter(|mode| !mode.is_empty())
            .map(str::to_string)
            .or(preferred_mode_id);
        let role_auto_approve = role_permissions
            .as_ref()
            .is_some_and(|(_, auto_approve)| *auto_approve);

        let connection_id = self
            .manager
            .spawn_agent(
                agent_type,
                effective_working_dir,
                None,
                runtime_env,
                owner_window,
                emitter,
                preferred_mode_id,
                preferred_config_values,
                role,
            )
            .await
            .map_err(|e| SpawnerError::Spawn(e.to_string()))?;
        // Record the decision on the fresh connection: it must land before the
        // prompt this caller sends next, which is the whole point — the member
        // must never ask the user.
        self.manager
            .set_role_auto_approve(&connection_id, role_auto_approve)
            .await;
        Ok(connection_id)
    }

    async fn send_prompt_linked_for_delegation(
        &self,
        conn_id: &str,
        task: String,
        link: crate::acp::delegation::spawner::DelegationLink,
    ) -> Result<i32, crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        // The child has no caller-supplied conversation_id (it's brand new).
        // folder_id must be None too — the manager's create-new-row branch
        // requires folder_id, which we resolve from the child's working_dir
        // via folder_service. Do that lookup here so the trait stays small.
        let working_dir_pathbuf = {
            let conns = self.manager.connections.lock().await;
            let conn = conns
                .get(conn_id)
                .ok_or_else(|| SpawnerError::Send(format!("child {conn_id} not found")))?;
            let s = conn.state.read().await;
            s.working_dir.clone()
        };
        let folder_path = working_dir_pathbuf
            .ok_or_else(|| {
                SpawnerError::Send(
                    "child connection has no working_dir; cannot derive folder_id".into(),
                )
            })?
            .to_string_lossy()
            .to_string();
        // `ensure_folder_for_path`, NOT `add_folder`: the row exists to carry the
        // child's `folder_id` and to resolve its cwd on resume, and neither of
        // those reads `is_open`. Opening it would turn whatever `working_dir` the
        // agent picked — a PR checkout under /tmp, a throwaway worktree — into a
        // top-level project in the user's sidebar, and would silently reopen a
        // folder the user had closed.
        let folder =
            crate::db::service::folder_service::ensure_folder_for_path(&self.db.conn, &folder_path)
                .await
                .map_err(|e| SpawnerError::Send(format!("ensure_folder_for_path: {e}")))?;

        let result = self
            .manager
            .send_prompt_linked(
                &self.db,
                conn_id,
                vec![PromptInputBlock::Text { text: task }],
                Some(folder.id),
                None,
                Some(link),
            )
            .await
            .map_err(|e| SpawnerError::Send(e.to_string()))?;
        result.ok_or_else(|| {
            SpawnerError::Send(
                "send_prompt_linked succeeded but no conversation_id was bound".into(),
            )
        })
    }

    async fn spawn_for_resume(
        &self,
        parent_connection_id: &str,
        agent_type: AgentType,
        working_dir: Option<String>,
        external_session_id: &str,
        preferred_mode_id: Option<String>,
        preferred_config_values: BTreeMap<String, String>,
    ) -> Result<crate::acp::delegation::spawner::ResumedSpawn, crate::acp::delegation::spawner::SpawnerError>
    {
        use crate::acp::delegation::spawner::{ResumedSpawn, SpawnerError};
        // Same parent inheritance as `spawn` — a resumed child whose emitter is
        // wired to a different broadcaster would stream to nobody.
        let (emitter, owner_window, parent_working_dir) = {
            let conns = self.manager.connections.lock().await;
            let parent = conns.get(parent_connection_id).ok_or_else(|| {
                SpawnerError::Spawn(format!(
                    "parent connection {parent_connection_id} not found"
                ))
            })?;
            let pwd = {
                let s = parent.state.read().await;
                s.working_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
            };
            (
                parent.emitter.clone(),
                parent.owner_window_label.clone(),
                pwd,
            )
        };
        let effective_working_dir = working_dir.or(parent_working_dir);

        let runtime_env = crate::commands::acp::build_session_runtime_env(
            &self.db,
            agent_type,
            None,
            self.data_dir.as_path(),
        )
        .await
        .map_err(|e| SpawnerError::Spawn(e.to_string()))?;

        // Detect dedup reuse BEFORE spawning, with the SAME lookup
        // `spawn_agent` runs at its own entry: a live connection for this
        // (agent, working_dir, session_id) — e.g. the user has the canceled
        // child session open in a tab — makes `spawn_agent` return that
        // connection instead of creating one. The broker must know, because
        // its failure teardown may only disconnect a connection this call
        // actually created. A connection appearing in the pre-check→spawn
        // window is missed, but that window is milliseconds and a misfire
        // additionally requires the send itself to fail.
        let working_dir_path = effective_working_dir.as_ref().map(std::path::PathBuf::from);
        let pre_existing = self
            .manager
            .find_connection_for_reuse(
                agent_type,
                working_dir_path.as_ref(),
                Some(external_session_id),
            )
            .await;

        // `session_id = Some(..)` is the whole difference vs `spawn`: the
        // connection loads the child's prior agent session (and its context)
        // instead of minting a fresh one.
        let connection_id = self
            .manager
            .spawn_agent(
                agent_type,
                effective_working_dir,
                Some(external_session_id.to_string()),
                runtime_env,
                owner_window,
                emitter,
                preferred_mode_id,
                preferred_config_values,
            
            None,)
            .await
            .map_err(|e| SpawnerError::Spawn(e.to_string()))?;
        let reused = pre_existing.as_deref() == Some(connection_id.as_str());
        Ok(ResumedSpawn {
            connection_id,
            reused,
        })
    }

    async fn send_resume_prompt(
        &self,
        conn_id: &str,
        prompt: String,
        folder_id: i32,
        child_conversation_id: i32,
    ) -> Result<(), crate::acp::delegation::spawner::SpawnerError> {
        use crate::acp::delegation::spawner::SpawnerError;
        // Adopt the child's EXISTING row (caller-supplied path) — no delegation
        // link: the row already carries parent_id / parent_tool_use_id /
        // delegation_call_id from the original delegation, and
        // `send_prompt_linked` rejects a link combined with an explicit
        // conversation_id precisely because adopted rows own their linkage.
        self.manager
            .send_prompt_linked(
                &self.db,
                conn_id,
                vec![PromptInputBlock::Text { text: prompt }],
                Some(folder_id),
                Some(child_conversation_id),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| SpawnerError::Send(e.to_string()))
    }

    async fn has_live_connection_for_conversation(&self, conversation_id: i32) -> bool {
        self.manager
            .find_connection_by_conversation_id(conversation_id)
            .await
            .is_some()
    }

    async fn cancel(
        &self,
        conn_id: &str,
    ) -> Result<(), crate::acp::delegation::spawner::SpawnerError> {
        self.manager
            .cancel(&self.db.conn, conn_id)
            .await
            .map_err(|e| crate::acp::delegation::spawner::SpawnerError::Cancel(e.to_string()))
    }

    async fn disconnect(
        &self,
        conn_id: &str,
    ) -> Result<(), crate::acp::delegation::spawner::SpawnerError> {
        self.manager
            .disconnect(conn_id)
            .await
            .map_err(|e| crate::acp::delegation::spawner::SpawnerError::Disconnect(e.to_string()))
    }
}

/// Production impl of `ParentSessionLookup` for the delegation listener.
/// Resolves the parent's current `conversation_id` by reading its
/// `SessionState`. Bundled with `ConnectionManagerSpawner` here so the
/// concrete wiring lives next to the manager it depends on.
#[derive(Clone)]
pub struct ConnectionManagerParentLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl crate::acp::delegation::listener::ParentSessionLookup for ConnectionManagerParentLookup {
    async fn current_conversation_id(&self, parent_connection_id: &str) -> Option<i32> {
        let state = self.manager.get_state(parent_connection_id).await?;
        let snapshot = state.read().await;
        snapshot.conversation_id
    }
}

/// Production impl of `SessionFeedbackAccess` for the delegation listener's
/// `check_user_feedback` arm. Resolves the parent connection's pending feedback
/// by delegating to `ConnectionManager::read_pending_feedback` /
/// `commit_feedback_delivered`. Mirrors
/// `ConnectionManagerParentLookup` so the listener stays unit-testable with an
/// in-memory stub.
#[derive(Clone)]
pub struct ConnectionManagerFeedbackLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl SessionFeedbackAccess for ConnectionManagerFeedbackLookup {
    async fn read_pending_feedback(
        &self,
        parent_connection_id: &str,
    ) -> Vec<PendingFeedback> {
        self.manager
            .read_pending_feedback(parent_connection_id)
            .await
    }

    async fn commit_feedback_delivered(&self, parent_connection_id: &str, ids: Vec<String>) {
        self.manager
            .commit_feedback_delivered(parent_connection_id, ids)
            .await
    }
}

/// Production impl of `SessionQuestionAccess` for the delegation listener's
/// `ask_user_question` arm. Registers / cancels the parent connection's pending
/// question by delegating to `ConnectionManager`. Mirrors
/// `ConnectionManagerFeedbackLookup` so the listener stays unit-testable with an
/// in-memory stub.
#[derive(Clone)]
pub struct ConnectionManagerQuestionLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl SessionQuestionAccess for ConnectionManagerQuestionLookup {
    async fn register_question(
        &self,
        parent_connection_id: &str,
        questions: Vec<QuestionSpec>,
    ) -> Option<RegisteredQuestion> {
        self.manager
            .register_question(parent_connection_id, questions)
            .await
    }

    async fn cancel_question(&self, parent_connection_id: &str, question_id: &str) {
        self.manager
            .cancel_question(parent_connection_id, question_id)
            .await
    }

    async fn cancel_questions_by_parent(&self, parent_connection_id: &str) {
        self.manager
            .cancel_questions_by_parent(parent_connection_id)
            .await
    }
}

/// Production impl of [`SessionPlanApprovalAccess`] for the Grok `exit_plan_mode`
/// ext bridge. Registers / cancels the parent connection's pending plan approval
/// by delegating to `ConnectionManager`. Mirrors `ConnectionManagerQuestionLookup`
/// so the connection handler stays unit-testable with an in-memory stub.
#[derive(Clone)]
pub struct ConnectionManagerPlanApprovalLookup {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait::async_trait]
impl SessionPlanApprovalAccess for ConnectionManagerPlanApprovalLookup {
    async fn register_plan_approval(
        &self,
        parent_connection_id: &str,
        tool_call_id: String,
        plan_markdown: String,
    ) -> Option<RegisteredPlanApproval> {
        self.manager
            .register_plan_approval(parent_connection_id, tool_call_id, plan_markdown)
            .await
    }

    async fn cancel_plan_approvals_by_parent(&self, parent_connection_id: &str) {
        self.manager
            .cancel_plan_approvals_by_parent(parent_connection_id)
            .await
    }
}

/// Make every option the probe applied a caller's selection to report the value
/// the agent picked on its own, instead of that selection.
///
/// Every consumer of the probe's snapshot reads an option's current value as
/// what it runs when the caller leaves it unset: the delegation settings panel
/// names its "Default" choice after it, and the task and automation editors
/// show and pin it for an option nobody touched. For an applied option that is
/// the agent's own pick, which the session no longer holds once the selection
/// is in — reporting the selection would label the user's model as the agent's
/// default. The options the agent derived from the selection (opencode's
/// `effort`, re-listed per model) keep the value it answered, because that is
/// what they run when unset given the selection.
///
/// `agent_chosen` is the establishment's own answer
/// (`SessionState::agent_chosen_config_values`). An applied id the agent never
/// answered with, or the snapshot does not carry, is left alone.
fn report_agent_chosen_values(
    options: &mut [crate::acp::types::SessionConfigOptionInfo],
    applied_ids: &[String],
    agent_chosen: &BTreeMap<String, String>,
) {
    use crate::acp::types::SessionConfigKindInfo;
    for option in options
        .iter_mut()
        .filter(|option| applied_ids.contains(&option.id))
    {
        let Some(chosen) = agent_chosen.get(&option.id) else {
            continue;
        };
        match &mut option.kind {
            SessionConfigKindInfo::Select(select) => select.current_value = chosen.clone(),
            SessionConfigKindInfo::Boolean(boolean) => boolean.current_value = chosen == "true",
        }
    }
}

#[cfg(test)]
mod tests;
