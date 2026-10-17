//! Transcript-tail watcher: surfaces Claude Code's OUT-OF-TURN activity.
//!
//! Claude Code produces activity outside any codeg-driven prompt turn:
//! `<task-notification>` completions of async sub-agents (`Agent` launched in
//! the background) and background shell tasks, the agent's continued work
//! after such a notification (which can run for many minutes), and cron//loop
//! autonomous turns. None of that has a reliable ACP wire representation —
//! out-of-turn `session/update`s are forwarded but carry no turn settlement,
//! and cron turns produce NO wire events at all (claude-agent-acp #270). The
//! session's own JSONL transcript is the only complete source, so this module
//! tails it:
//!
//! * **Accounting** — launch acks are detected from the record-level
//!   `toolUseResult` (`status:"async_launched"` → `agentId`, or
//!   `backgroundTaskId` for background shells; these fields exist ONLY on
//!   disk, never on the wire), settled by any of: a `<task-notification>`
//!   record (matching `<task-id>`), a `TaskOutput` result whose structured
//!   `task.status` reached a terminal state, or a `TaskStop`/`KillShell`
//!   call. Background shells almost never emit a `<task-notification>` (they
//!   are collected inline via `TaskOutput` or just left running), so those
//!   two extra signals are what keep the count from stranding. Entries are
//!   re-armed when the main agent resumes a settled sub-agent via
//!   `SendMessage`, and expired past
//!   [`background_keepalive_max_age`]. The outstanding count is mirrored into
//!   `SessionState` (via `apply_event`) to exempt the connection from both
//!   idle sweeps — disconnecting kills the agent CLI, and the background work
//!   dies with it.
//!
//! * **Rendering** — new transcript records that do NOT belong to a
//!   codeg-sent prompt turn are assembled into turns with the SAME Stage-A/
//!   Stage-B code the detail parser uses ([`ClaudeRecordAccumulator`] +
//!   [`group_into_turns`]) and emitted as `AcpEvent::BackgroundActivity`
//!   upserts for the frontend's overlay slice. Foreground turns are excluded
//!   by the **prompt ledger**: every prompt codeg sends is fingerprinted, and
//!   a transcript turn whose initiating user record matches an unconsumed
//!   fingerprint is the wire-rendered foreground turn (each fingerprint is
//!   consumed exactly once, so a cron//loop re-fire of the SAME text later
//!   correctly classifies as out-of-turn).
//!
//! * **Session title** — Claude Code's generated name arrives as a dedicated
//!   `ai-title` transcript record whenever the background summarizer finishes,
//!   routinely AFTER the turn that triggered it ended. The ACP adapter only
//!   pulls the name at turn-end (`maybeUpdateSessionTitle`, claude-agent-acp
//!   0.69.0), so on a short session there is nothing to read yet and no wire
//!   event ever follows. These bytes are already being tailed, so the records
//!   are folded here and handed to [`publish_native_title`] — the same path a
//!   live ACP title takes. Not activity: it rides alongside the activity event
//!   rather than inside it (see `run_watch`). This watch is also the only
//!   live writer of a Claude title: the adapter's own `session_info_update`
//!   title is held for it, and it drops that title while it reads the
//!   transcript (`session_title::release_held_wire_title`; the module docs
//!   of `acp::session_title` say why).
//!
//! The watcher is connection-scoped on purpose: background work cannot outlive
//! the agent CLI process, whose lifetime IS the connection's. Poll ticks are
//! mtime-gated (an unchanged file costs one `stat`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use crate::acp::session_state::{background_keepalive_max_age, SessionState};
use crate::acp::session_title::{publish_native_title, release_held_wire_title};
use crate::acp::types::{AcpEvent, BackgroundSettledInfo, ConnectionStatus};
use crate::models::agent::AgentType;
use crate::models::message::MessageTurn;
use crate::parsers::claude::{
    capture_tag, capture_title_record, find_clear_rollover_successor, find_session_file,
    group_into_turns, is_meta_message, queued_human_prompt, queued_task_notification,
    slash_command_display, task_notification_result_regex, task_notification_status_regex,
    task_notification_summary_regex, task_notification_task_id_regex,
    task_notification_tool_use_id_regex, ClaudeRecordAccumulator, BACKGROUND_RESULT_MAX_CHARS,
    CONTEXT_CONTINUATION_PREFIX,
};
use crate::parsers::truncate_str;
use crate::web::event_bridge::{emit_with_state, EventEmitter};

/// Poll cadence while background work is outstanding or the transcript moved
/// recently — tight enough that a completed task surfaces within a beat.
const POLL_ACTIVE: Duration = Duration::from_secs(1);
/// Cadence when nothing is pending: cron//loop turns can still land at any
/// time, so the watch never stops — an unchanged file costs one `stat`.
const POLL_IDLE: Duration = Duration::from_secs(3);
/// How long after the last transcript growth the tight cadence is kept.
/// Sized to cover the agent's reaction to a settled task: after the
/// task-notification lands (last growth) the model may take 10–20s to write
/// its first reply block for a heavy synthesis (observed ~16s for a
/// two-agent digest) — dropping to the idle cadence inside that window adds
/// up to POLL_IDLE of avoidable surfacing latency. An unchanged file costs
/// one `stat` per tick, so the wider window is effectively free.
const RECENT_ACTIVITY_WINDOW: Duration = Duration::from_secs(30);
/// Prompt fingerprints older than this are dropped unconsumed — a rejected /
/// never-persisted prompt must not linger and swallow a later cron re-fire of
/// the same text.
const LEDGER_TTL: Duration = Duration::from_secs(600);
/// Max fingerprints kept (oldest evicted first). Far above any realistic
/// number of prompts in flight between transcript flushes.
const LEDGER_CAP: usize = 32;
/// Rotate the episode accumulator at the next out-of-turn boundary once it
/// holds this many messages, bounding per-tick regroup cost during very long
/// autonomous stretches. Already-emitted turns stay valid in the frontend
/// overlay; rotation only re-bases the id namespace for what follows.
const MAX_EPISODE_MESSAGES: usize = 512;
/// Absolute episode bound: a SINGLE autonomous turn can exceed
/// `MAX_EPISODE_MESSAGES` without ever hitting a boundary (a heavy /loop
/// iteration runs hundreds of tool calls in one turn), and every tick
/// clones + regroups + re-hashes the whole episode — unbounded, that's
/// O(n²) work over the turn. Past this valve the episode is force-rotated
/// mid-turn: the in-progress turn renders split across two overlay cards (a
/// visible seam, corrected by the next detail refetch) in exchange for a
/// hard cap on per-tick work. Double the boundary threshold so normal
/// boundary rotation always wins for multi-turn episodes.
const FORCE_ROTATE_MESSAGES: usize = MAX_EPISODE_MESSAGES * 2;

/// How a transcript record supplied its turn-initiating text. Verbatim text
/// can use the ledger's ordinary prefix match; a slash command reconstructed
/// from tags needs the narrower command-separator normalization below.
#[derive(Debug, PartialEq, Eq)]
enum TurnInitiatorText {
    Verbatim(String),
    ReconstructedSlashCommand(String),
}

impl TurnInitiatorText {
    fn as_str(&self) -> &str {
        match self {
            Self::Verbatim(text) | Self::ReconstructedSlashCommand(text) => text,
        }
    }
}

/// Reproduce the one lossy transformation made by [`slash_command_display`]:
/// whitespace separating the command name from its arguments becomes one
/// space. Whitespace *inside* the arguments remains byte-for-byte significant.
fn reconstructed_slash_command_fingerprint(text: &str) -> Option<String> {
    let text = text.trim();
    let name_end = text.find(char::is_whitespace).unwrap_or(text.len());
    let name = &text[..name_end];
    if !name.starts_with('/') {
        return None;
    }
    let args = text[name_end..].trim();
    if args.is_empty() {
        Some(name.to_string())
    } else {
        Some(format!("{name} {args}"))
    }
}

/// The first text claude-agent-acp writes into the transcript's user record
/// for a prompt, trimmed (`promptToClaude`, as of 0.81.1): each block in
/// order — a text block as it is; a text resource or a resource link as the
/// link [`claude_uri_link`] renders, with a resource's body moved to a
/// `<context>` block at the END of the record; images and blob resources as
/// no text at all.
fn first_transcript_text(blocks: &[crate::acp::types::PromptInputBlock]) -> Option<String> {
    use crate::acp::types::PromptInputBlock;
    blocks.iter().find_map(|block| {
        let text = match block {
            PromptInputBlock::Text { text } => text.trim().to_string(),
            PromptInputBlock::Resource {
                uri,
                text: Some(_),
                ..
            }
            | PromptInputBlock::ResourceLink { uri, .. } => claude_uri_link(uri),
            PromptInputBlock::Resource { .. } | PromptInputBlock::Image { .. } => {
                return None
            }
        };
        (!text.is_empty()).then_some(text)
    })
}

/// claude-agent-acp's `formatUriAsLink`: `[@name](uri)` for a `file://` or
/// `zed://` uri, named after its last path segment, and the bare uri for
/// anything else (a web page, a pasted file's `clipboard://` uri).
fn claude_uri_link(uri: &str) -> String {
    let named = |rest: &str| {
        let name = rest.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(rest);
        format!("[@{name}]({uri})")
    };
    if let Some(path) = uri.strip_prefix("file://") {
        named(path)
    } else if uri.starts_with("zed://") {
        named(uri)
    } else {
        uri.to_string()
    }
}

/// Fingerprints of prompts codeg itself sent on this connection, so the
/// watcher can tell wire-rendered foreground turns apart from out-of-turn
/// activity. Shared between the connection loop (writer, on every
/// `ConnectionCommand::Prompt`) and the watcher tick (consumer). A std mutex
/// is deliberate: both sides take it for microseconds and the watcher locks it
/// from a blocking context.
pub(crate) struct PromptLedger {
    entries: Mutex<VecDeque<LedgerEntry>>,
    /// When this connection last sent `/clear` — the one thing that makes a
    /// transcript rollover attributable to THIS session rather than to any of
    /// the other transcripts sharing the project directory. See
    /// [`Self::clear_rollover_expected`].
    ///
    /// Stamped twice over: the `Instant` ages the expectation out, the
    /// `SystemTime` is what a re-arm compares against the session's own
    /// change instant to tell a `/clear` sent for the session being left from
    /// one sent for the session being entered (a connection outlives a fork,
    /// and the watcher learns of the switch up to a poll late).
    clear_sent_at: Mutex<Option<(Instant, std::time::SystemTime)>>,
}

struct LedgerEntry {
    fingerprint: String,
    recorded_at: Instant,
}

/// How long after sending `/clear` the watcher keeps looking for the
/// successor transcript. Deliberately the same span the detector itself
/// allows between the two files (`CLEAR_ROLLOVER_MAX_GAP_SECS`): a gate that
/// outlived the detector's window would leave a stretch where the watcher
/// still hunts but can no longer accept the answer.
const CLEAR_ROLLOVER_EXPECT_WINDOW: Duration =
    Duration::from_secs(crate::parsers::claude::CLEAR_ROLLOVER_MAX_GAP_SECS as u64);

/// How far after a session change a `/clear` may still be one that was typed
/// into the NEW session. Bounds a comparison that crosses the wall clock: the
/// real gap is the watcher's poll lag (at most `POLL_IDLE` plus scheduling),
/// so this is an order of magnitude of headroom and no more.
const REARM_CLEAR_GRACE: Duration = Duration::from_secs(30);

impl PromptLedger {
    pub(crate) fn shared() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(VecDeque::new()),
            clear_sent_at: Mutex::new(None),
        })
    }

    /// True while a `/clear` sent on this connection could still be followed
    /// by its successor transcript appearing on disk.
    ///
    /// Nothing on disk links a rollover back to the session it came from (the
    /// successor's first records carry a fresh uuid, `parentUuid: null`, and
    /// no reference to the file it replaced), so without this the only
    /// available evidence — "a sibling with a `/clear` head appeared right
    /// about when our file went quiet" — is equally true of every OTHER
    /// conversation open on the same folder. codeg is a multi-agent
    /// workbench; two Claude sessions in one project directory is the normal
    /// case, not the exotic one. Knowing that WE asked for the clear is what
    /// keeps this session from adopting a stranger's transcript.
    fn clear_rollover_expected(&self) -> bool {
        let at = self.clear_sent_at.lock().unwrap_or_else(|p| p.into_inner());
        at.is_some_and(|(t, _)| t.elapsed() < CLEAR_ROLLOVER_EXPECT_WINDOW)
    }

    /// Consume the expectation once its successor has been adopted, so a
    /// later sibling rollover inside the same window is not also taken.
    fn consume_clear_request(&self) {
        *self.clear_sent_at.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Drop an expectation that belongs to a session this connection has
    /// LEFT. Called on a fork/re-resume re-arm: an unconsumed `/clear` from
    /// the outgoing session would otherwise authorize a rollover hunt against
    /// the incoming one's transcript, which never cleared.
    ///
    /// `changed_at` is when the session id actually changed. A `/clear`
    /// recorded after it was typed into the NEW session — the watcher simply
    /// had not polled yet — and must survive. Without a change instant to
    /// compare against there is nothing to tell the two apart, so the
    /// expectation is dropped: a missed adoption still recovers on reopen,
    /// where adopting a stranger's transcript would not.
    ///
    /// The window is bounded rather than open-ended because the comparison
    /// crosses the wall clock, which can step backwards under it. The gap
    /// this admits is only ever the watcher's own poll lag (`POLL_IDLE` plus
    /// scheduling), so anything further out is not a late-polled `/clear` —
    /// it is a clock that moved, and it resolves to the safe answer.
    fn expire_clear_request_before(&self, changed_at: Option<std::time::SystemTime>) {
        let mut slot = self.clear_sent_at.lock().unwrap_or_else(|p| p.into_inner());
        let belongs_to_new_session =
            slot.zip(changed_at)
                .is_some_and(|((_, sent_at), changed_at)| {
                    sent_at
                        .duration_since(changed_at)
                        .is_ok_and(|since_change| since_change <= REARM_CLEAR_GRACE)
                });
        if !belongs_to_new_session {
            *slot = None;
        }
    }

    #[cfg(test)]
    fn note_clear_for_test(&self) {
        *self.clear_sent_at.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((Instant::now(), std::time::SystemTime::now()));
    }

    /// Record the fingerprint of a prompt codeg is about to send: the first
    /// text the Claude adapter writes into the transcript's user record for
    /// it, trimmed — which is what that record's initiating text starts with.
    ///
    /// Almost always the prompt's first text block: the prose lands verbatim.
    /// A prompt that opens with an attachment instead has nothing typed before
    /// it — a page the built-in browser handed over, sent on its own — and
    /// there the adapter writes the attachment's link first. Leaving such a
    /// prompt unrecorded classified its own turn as out-of-turn, so the overlay
    /// rendered the prompt and its reply a second time, under the live copy.
    pub(crate) fn record_prompt_blocks(&self, blocks: &[crate::acp::types::PromptInputBlock]) {
        let Some(fingerprint) = first_transcript_text(blocks) else {
            // Nothing the adapter writes as text (an image-only prompt): its
            // record carries no initiating text, so it cannot start a turn of
            // its own here either.
            tracing::debug!("[bg-watch] prompt without text — no fingerprint recorded");
            return;
        };
        // `/clear` is a CLI-local command: the adapter forwards it like any
        // other prompt and Claude answers it by starting a new session on a
        // new transcript file. This is the only notice codeg gets.
        if fingerprint == "/clear" || fingerprint.starts_with("/clear ") {
            *self.clear_sent_at.lock().unwrap_or_else(|p| p.into_inner()) =
                Some((Instant::now(), std::time::SystemTime::now()));
        }
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.push_back(LedgerEntry {
            fingerprint,
            recorded_at: Instant::now(),
        });
        while entries.len() > LEDGER_CAP {
            entries.pop_front();
        }
    }

    /// Match `initiator_text` (the transcript turn's initiating user text)
    /// against the unconsumed fingerprints; on match the entry is consumed —
    /// exactly once per sent prompt, so a later same-text autonomous re-fire
    /// finds no entry and classifies as out-of-turn. A verbatim record may
    /// carry appended wrapper content after the sent text, hence its prefix
    /// matching fallback.
    ///
    /// A slash command's initiator text is RECONSTRUCTED rather than read back:
    /// the CLI persists the invocation as command tags, and
    /// [`slash_command_display`] rebuilds it as `"/name" + ' ' + trimmed args`.
    /// For that record type only, reproduce the same separator normalization on
    /// the fingerprint. Normalizing every whitespace run would conflate
    /// semantically different ordinary prompts and command arguments, risking
    /// suppression of a genuine out-of-turn turn.
    fn consume_matching(&self, initiator: &TurnInitiatorText) -> bool {
        let text = initiator.as_str().trim();
        if text.is_empty() {
            return false;
        }
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.retain(|e| e.recorded_at.elapsed() < LEDGER_TTL);
        if let Some(pos) = entries.iter().position(|e| match initiator {
            TurnInitiatorText::Verbatim(_) => {
                text == e.fingerprint || text.starts_with(e.fingerprint.as_str())
            }
            TurnInitiatorText::ReconstructedSlashCommand(_) => {
                text == e.fingerprint
                    || reconstructed_slash_command_fingerprint(&e.fingerprint).as_deref()
                        == Some(text)
            }
        }) {
            entries.remove(pos);
            return true;
        }
        false
    }

    /// Fingerprint a bare string — test convenience over
    /// [`Self::record_prompt_blocks`]. (The `_session/steering` arm in
    /// `connection.rs` used to be the production caller; it now records the
    /// steered blocks directly, since a steered draft can carry attachments.)
    #[cfg(test)]
    pub(crate) fn record_text(&self, text: &str) {
        self.record_prompt_blocks(&[crate::acp::types::PromptInputBlock::Text {
            text: text.to_string(),
        }]);
    }
}

/// Aborts the watcher task when the owning conversation loop exits
/// (disconnect or fork restart — the restarted loop arms a fresh watcher).
pub(crate) struct BackgroundWatchGuard(tokio::task::JoinHandle<()>);

impl Drop for BackgroundWatchGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Arm the transcript watcher for a Claude connection; other agents have no
/// transcript-notification mechanism and get no watcher (returns `None`).
pub(crate) fn spawn_if_claude(
    conn_id: &str,
    agent_type: AgentType,
    state: Arc<RwLock<SessionState>>,
    emitter: EventEmitter,
    cwd: String,
    ledger: Arc<PromptLedger>,
) -> Option<BackgroundWatchGuard> {
    if agent_type != AgentType::ClaudeCode {
        return None;
    }
    let conn_id = conn_id.to_string();
    let handle = tokio::spawn(async move {
        run_watch(conn_id, state, emitter, cwd, ledger).await;
    });
    Some(BackgroundWatchGuard(handle))
}

/// Wait out `delay` before the next poll, or less if `wake` is notified.
async fn wait_for_poll(delay: Duration, wake: &tokio::sync::Notify) {
    tokio::select! {
        _ = tokio::time::sleep(delay) => {}
        _ = wake.notified() => {}
    }
}

/// Publish what a tick learned about the session's title.
///
/// A title the transcript just named the session goes out exactly like a live
/// ACP one (same skip-cache, same lifecycle write). It is held rather than
/// dropped while the conversation row is still unbound: unlike a live ACP
/// title there is no resend to COUNT on. These bytes are read exactly once,
/// and while the CLI does re-emit the record on its own metadata flushes,
/// nothing guarantees another one lands after the row binds — a session that
/// ends right there would keep its first-prompt name.
///
/// Then the title the adapter published, which the notification loop held for
/// this watch: dropped while the transcript titles the session, published only
/// while there is no transcript to read. Settled after the tick, so a
/// transcript that exists has had its lookup this poll.
///
/// The read guard is released before `publish_native_title` asks for the
/// write lock, and both halves short-circuit so a settled session takes no
/// write lock at all.
async fn publish_tick_titles(
    ws: &mut WatchState,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
) {
    let title_is_publishable =
        ws.pending_title.is_some() && state.read().await.conversation_id.is_some();
    if title_is_publishable {
        if let Some(title) = ws.pending_title.take() {
            publish_native_title(state, emitter, title).await;
        }
    }
    release_held_wire_title(state, emitter, ws.reads_transcript()).await;
}

async fn run_watch(
    conn_id: String,
    state: Arc<RwLock<SessionState>>,
    emitter: EventEmitter,
    cwd: String,
    ledger: Arc<PromptLedger>,
) {
    let mut ws = WatchState::new();
    // Wall-clock boundary between pre-existing transcript history (renders
    // via the detail fetch) and records that belong to THIS watch's lifetime.
    // Captured BEFORE the session is established: a NEW session's file is
    // created strictly after this instant, so every record in it — including
    // a first prompt or launch ack written before the watcher learns the
    // session id (SessionStarted can lag file creation by seconds) — is ours
    // to account, classify, and ledger-consume. A RESUMED session's history
    // predates this instant and is skipped. Baselining blindly at EOF on
    // first discovery used to drop that pre-discovery window: the ack never
    // registered (no keep-alive) and the first prompt's ledger entry
    // lingered, able to swallow a later same-text cron refire.
    let spawn_epoch = std::time::SystemTime::now();
    let mut first_arm_done = false;
    // A wire title the notification loop holds for this watch wakes it, so the
    // title is settled within a tick of arriving (`publish_tick_titles`).
    let wire_title_wake = Arc::clone(&state.read().await.wire_title_wake);
    loop {
        wait_for_poll(ws.poll_delay(), &wire_title_wake).await;

        let (session_id, session_changed_at, is_prompting, turn_ended_abnormally) = {
            let s = state.read().await;
            (
                s.external_id.clone(),
                s.external_id_changed_at,
                s.status == ConnectionStatus::Prompting,
                s.last_turn_ended_abnormally,
            )
        };
        let Some(session_id) = session_id else {
            continue; // session not established yet
        };
        if ws.session_id.as_deref() != Some(session_id.as_str()) {
            // New/resumed/forked session: re-locate and re-baseline. History
            // up to now renders via the normal detail fetch, not the overlay.
            // The first arm keeps the spawn epoch (see above). A LATER re-arm
            // (fork / re-resume) baselines at the instant the session id
            // actually CHANGED — not at this tick: the frontend fires its
            // follow-up prompt the moment the fork resolves, so records can
            // land in the forked transcript seconds before this poll notices;
            // a tick-time epoch would misclassify them as copied history (the
            // copy carries ORIGINAL, pre-fork timestamps — the changed-at
            // instant cleanly separates the two).
            let epoch = if first_arm_done {
                session_changed_at.unwrap_or_else(std::time::SystemTime::now)
            } else {
                spawn_epoch
            };
            if first_arm_done {
                // An unconsumed `/clear` from the session being LEFT must not
                // authorize a rollover hunt against the one being entered,
                // which never cleared. One typed into the new session between
                // the switch and this poll survives — same `session_changed_at`
                // the epoch above is taken from.
                ledger.expire_clear_request_before(session_changed_at);
            }
            first_arm_done = true;
            ws.rearm(session_id.clone(), epoch);
        }

        // File I/O + JSON parsing + turn grouping are blocking work; a large
        // tail after a long foreground turn must not stall the runtime.
        let ledger_ref = Arc::clone(&ledger);
        let cwd_for_tick = cwd.clone();
        let conn_for_tick = conn_id.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut ws = ws;
            let event = ws.tick(
                &ledger_ref,
                &cwd_for_tick,
                &conn_for_tick,
                is_prompting,
                turn_ended_abnormally,
            );
            (ws, event)
        })
        .await;
        let event = match joined {
            Ok((returned, event)) => {
                ws = returned;
                event
            }
            Err(e) => {
                // Tick panicked (never expected — it is written to skip bad
                // input). Start over with a fresh baseline rather than killing
                // the watch for the rest of the connection's life.
                tracing::warn!("[bg-watch] tick panicked, re-arming: {e}");
                ws = WatchState::new();
                continue;
            }
        };

        // Deliberately OUTSIDE the activity emit below: a tick whose tail is
        // nothing but an `ai-title` record produces no turns, no settlements
        // and no accounting change, so `tick` correctly returns `None` — which
        // is the common case for a title generated after the last turn ended.
        publish_tick_titles(&mut ws, &state, &emitter).await;

        if let Some(new_id) = ws.pending_transcript_id.take() {
            tracing::info!(
                "[bg-watch] transcript rollover connection={} to={}",
                conn_id,
                new_id
            );
            emit_with_state(
                &state,
                &emitter,
                AcpEvent::TranscriptRolledOver {
                    transcript_id: new_id,
                },
            )
            .await;
        }

        if let Some(event) = event {
            if let AcpEvent::BackgroundActivity {
                turns,
                settled,
                outstanding,
                watermark,
                ..
            } = &event
            {
                tracing::info!(
                    "[bg-watch] surfacing connection={} turns={} settled={} outstanding={} watermark={}",
                    conn_id,
                    turns.len(),
                    settled.len(),
                    outstanding,
                    watermark
                );
            }
            emit_with_state(&state, &emitter, event).await;
        }
    }
}

/// One launched-but-unresolved background task.
struct TaskEntry {
    kind: &'static str,
    started_at: Instant,
}

/// The current out-of-turn episode: a contiguous run of transcript records
/// not belonging to any codeg-sent prompt turn, assembled into turns via the
/// detail parser's own Stage A/B.
struct Episode {
    /// Byte offset of the episode's initiating record — the stable base of
    /// this episode's overlay turn ids (`bg-<start_offset>-<idx>`).
    start_offset: u64,
    acc: ClaudeRecordAccumulator,
    /// turn id → content hash at last emission, for changed-turn upserts.
    emitted_hashes: HashMap<String, u64>,
    /// The task id of the `<task-notification>` that initiated this episode,
    /// or `None` for any other out-of-turn initiator (a cron prompt, other
    /// injected text). Carried across a force-rotation (same continuous
    /// out-of-turn stretch, just re-based) — see `classify_and_feed`. Used by
    /// `collect_changed_turns` to tag each collected turn for the held-turn
    /// suppression filter in `tick()`.
    origin_task_id: Option<String>,
}

enum Mode {
    /// Records belong to a codeg-sent prompt turn — the wire renders them.
    Foreground,
    /// Records are out-of-turn — the overlay renders them.
    Background,
}

pub(crate) struct WatchState {
    session_id: Option<String>,
    file: Option<PathBuf>,
    /// Bytes consumed through the last complete line — the emitted watermark.
    committed: u64,
    /// Bytes after `committed`: a trailing partial line awaiting its newline.
    carry: Vec<u8>,
    /// Last observed (mtime, len): the cheap "did anything change" gate.
    last_stat: Option<(Option<std::time::SystemTime>, u64)>,
    mode: Mode,
    episode: Option<Episode>,
    tasks: HashMap<String, TaskEntry>,
    /// Task ids that have settled at least once — a later `SendMessage` to
    /// such an id re-arms it (the resumed sub-agent will notify again).
    settled_ids: HashSet<String>,
    /// Task ids launched (an `async_launched`/`backgroundTaskId` ack seen)
    /// while the connection's CURRENTLY (or most recently) active turn was
    /// `Prompting`. An `async_launched` (sub-agent) id is inserted here;
    /// `backgroundTaskId` (shell) ids deliberately are NOT (see `account()`).
    /// Cleared on every Connected→Prompting rising edge (each turn starts
    /// with an empty set) AND, early, the instant a turn is observed to have
    /// ended abnormally (see `last_turn_ended_abnormally` below) — otherwise
    /// it persists UNCHANGED across a normal Prompting→Connected falling
    /// edge, with no time limit. Used to detect an out-of-turn
    /// `<task-notification>` follow-up that belongs to a turn #870
    /// (claude-agent-acp v0.59.0) is holding open for its own spawned
    /// sub-agents: that follow-up's content is already rendering on the wire,
    /// so the OVERLAY turn for it must be suppressed to avoid double-rendering
    /// it (`tick()`'s `changed_turns` filter). The `settled` notification for
    /// the same task is NOT suppressed — the frontend needs it to flip the
    /// launch card, and it patches that card in-memory rather than re-parsing
    /// the transcript, so it can't double-render (see `tick()`). No time window
    /// is needed: the set's own lifetime — cleared only at the next rising
    /// edge — already covers the case where the turn's tail content is read by
    /// a tick strictly AFTER the falling edge (the watcher polls on its own
    /// cadence, independent of exactly when the turn settles), for however long
    /// that takes.
    current_turn_launched_ids: HashSet<String>,
    /// `Prompting` state observed at the previous tick — the edge detector for
    /// `current_turn_launched_ids` above.
    was_prompting: bool,
    /// A codeg-sent prompt has been matched in the transcript and the model has
    /// not answered it yet. Within that window the CLI writes the rest of the
    /// SUBMISSION — a slash command's `<local-command-stdout>`, the `isMeta`
    /// instruction `/goal` injects for the model, image metadata — and
    /// `turn_initiator_text` reads those as fresh initiators (they are user
    /// records with text, and the ledger has nothing left to match them
    /// against). Flipping to `Background` there re-renders the wire's own reply
    /// as an overlay: the observed `/goal` bug, where the whole answer appeared
    /// twice.
    ///
    /// Four things bound it, because a swallowed initiator is worse than a
    /// duplicate (a duplicate self-heals on the next detail refetch; a turn that
    /// never reaches the overlay is missing from the LIVE view until then):
    /// only records carrying the matched prompt's own `promptId` are affected
    /// (`foreground_submission_id` — a submission id is per-submission, so an
    /// autonomous prompt always has a different one, or none); the model's first
    /// record closes the window; any non-prompting tick closes it (so a turn
    /// that ends without ever answering cannot strand it); and a
    /// `<task-notification>` is exempt outright — it settles on its own schedule
    /// and its follow-up has no other live place to render.
    foreground_awaiting_reply: bool,
    /// `promptId` of the ledger-matched record that opened the window above.
    /// `None` when that record carried none, which disables the window entirely
    /// — without an id there is no way to tell a submission's own records from
    /// an unrelated initiator, and the safe failure is the pre-fix duplicate,
    /// never a hidden turn.
    foreground_submission_id: Option<String>,
    /// `Prompting` state for the tick currently being processed. Set once at
    /// `tick()` entry from the caller-supplied snapshot so `account()` (called
    /// per transcript line within the same tick) can read it without an extra
    /// parameter threaded through every call site.
    currently_prompting: bool,
    /// `MessageTurn.id` → the out-of-turn episode's origin task id (`None` if
    /// the episode wasn't initiated by a `<task-notification>`, e.g. a cron
    /// prompt), for turns collected THIS tick by `collect_changed_turns`.
    /// Drained by the suppression filter at the end of `tick()` — entries
    /// never outlive the tick that created them.
    turn_origin_task_ids: HashMap<String, Option<String>>,
    last_disk_activity: Option<Instant>,
    last_emitted_outstanding: Option<u32>,
    armed_logged: bool,
    /// Base of the most recently created episode's id namespace. Episode
    /// bases must be STRICTLY increasing: two episodes created while
    /// processing one tick's batch would otherwise share `committed` (it
    /// advances per batch, not per record) and collide their `bg-<base>-…`
    /// ids — the frontend upserts by id, so a collision conflates turns.
    last_episode_base: u64,
    /// Wall-clock boundary for the arm baseline: records at/after this
    /// instant belong to this watch's lifetime and are processed even when
    /// they were written before the transcript file was first discovered;
    /// records before it are pre-existing history. Set by `rearm`.
    epoch: Option<std::time::SystemTime>,
    /// Newest non-empty `custom-title` / `ai-title` value this watch has read
    /// off the transcript, in the parser's own two slots (`parsers::claude::
    /// capture_title_record`). Kept separate rather than folded into one
    /// string so the user's `/rename` keeps winning over a title Claude Code
    /// generates afterwards, exactly as `parse_conversation_detail` resolves
    /// the pair over the whole file.
    ///
    /// Seeded from the skipped pre-baseline history at arm time
    /// (`seed_titles_from_history`) precisely because that resolution IS a
    /// whole-file rule — the two records are appended independently, so the
    /// tail alone is not enough to resolve them.
    custom_title: Option<String>,
    ai_title: Option<String>,
    /// A resolved title this watch read but has not published yet. Set only
    /// when a title RECORD actually changed the resolution, so a session with
    /// a settled name costs nothing per tick.
    pending_title: Option<String>,
    /// Transcript uuid of a Claude `/clear` rollover this watch just adopted.
    /// The ACP session id is unchanged, so this is the id `conversation.external_id`
    /// must be re-pointed at. Consumed by `run_watch` after the tick.
    pending_transcript_id: Option<String>,
    /// The last read of `file` failed (it exists but cannot be read). Cleared
    /// by the next read that succeeds and by adopting a file.
    read_failed: bool,
    /// Transcript uuid this watch rolled over ONTO, kept for as long as the
    /// watch lives. `find_session_file` resolves the ACP session id, which
    /// after a `/clear` names the abandoned file — so a re-locate (the stat
    /// error path below nulls `file`) has to ask for this id instead, or the
    /// watch would silently fall back onto the dead transcript. Cleared by
    /// `rearm`: a fork/resume is a different session, not this chain.
    rolled_over_id: Option<String>,
}

impl WatchState {
    pub(crate) fn new() -> Self {
        Self {
            session_id: None,
            file: None,
            committed: 0,
            carry: Vec::new(),
            last_stat: None,
            mode: Mode::Foreground,
            episode: None,
            tasks: HashMap::new(),
            settled_ids: HashSet::new(),
            current_turn_launched_ids: HashSet::new(),
            was_prompting: false,
            foreground_awaiting_reply: false,
            foreground_submission_id: None,
            currently_prompting: false,
            turn_origin_task_ids: HashMap::new(),
            last_disk_activity: None,
            // Some(0), not None: consumers assume zero until told otherwise,
            // so the first tick must not emit an accounting-only event for a
            // connection with no background work.
            last_emitted_outstanding: Some(0),
            armed_logged: false,
            last_episode_base: 0,
            epoch: None,
            custom_title: None,
            ai_title: None,
            pending_title: None,
            pending_transcript_id: None,
            read_failed: false,
            rolled_over_id: None,
        }
    }

    /// The session's name as this watch currently understands it: the user's
    /// own `/rename` first, then Claude Code's generated summary — the same
    /// precedence `parsers::claude` applies when it resolves the whole file.
    fn resolved_title(&self) -> Option<String> {
        self.custom_title.clone().or_else(|| self.ai_title.clone())
    }

    /// Whether this watch reads the session's transcript, the file whose title
    /// records the history parser resolves too: it has the file and its last
    /// read of it did not fail. Until it does (it retries the lookup every
    /// tick), or for good when the agent writes where codeg does not look, or
    /// while the file cannot be read, the adapter's wire title is the only one
    /// codeg has (`session_title::release_held_wire_title`).
    pub(crate) fn reads_transcript(&self) -> bool {
        self.file.is_some() && !self.read_failed
    }

    /// Fold one transcript record into the title slots, queueing the result
    /// for publication when it changed the resolved name.
    ///
    /// Claude Code writes its generated title as a dedicated `ai-title`
    /// record, and it lands whenever the background summarizer finishes —
    /// routinely AFTER the turn that triggered it has already ended. The ACP
    /// adapter only reads the name back at turn-end, so on a short session
    /// that title is never published and the conversation keeps its
    /// first-prompt fallback name. The watcher is already tailing these exact
    /// bytes, so surfacing the record here is what makes the name appear
    /// while the session is still live instead of on its next detail load.
    ///
    /// Only records that carry a title are inspected — everything else costs
    /// one string compare.
    fn capture_title(&mut self, value: &serde_json::Value) {
        let record_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if !matches!(record_type, "custom-title" | "ai-title") {
            return;
        }
        let before = self.resolved_title();
        capture_title_record(value, record_type, &mut self.custom_title, &mut self.ai_title);
        let after = self.resolved_title();
        // `capture_title_record` ignores blank values, so `after` is `None`
        // only when nothing has ever been captured — never a clear.
        if after.is_some() && after != before {
            self.pending_title = after;
        }
    }

    /// Allocate the id-namespace base for a new episode: the current byte
    /// offset, tie-broken upward so consecutive episodes never collide even
    /// when created within a single tick's batch.
    fn next_episode_base(&mut self) -> u64 {
        let base = self.committed.max(self.last_episode_base + 1);
        self.last_episode_base = base;
        base
    }

    fn rearm(&mut self, session_id: String, epoch: std::time::SystemTime) {
        let tasks = std::mem::take(&mut self.tasks);
        let settled_ids = std::mem::take(&mut self.settled_ids);
        *self = Self::new();
        // Keep the accounting across a fork/resume of the same CLI process —
        // the background work is still running in it; the max-age valve and
        // future notifications (same task-id) still resolve these entries.
        // `settled_ids` must survive too: a post-fork `SendMessage(to: <id>)`
        // resumes a sub-agent that settled BEFORE the fork, and without the
        // set the re-arm is missed — outstanding stays 0 and closing the tab
        // could kill the resumed work.
        self.tasks = tasks;
        self.settled_ids = settled_ids;
        self.session_id = Some(session_id);
        self.epoch = Some(epoch);
    }

    /// Switch onto a `/clear` successor transcript without changing the ACP
    /// session id (the adapter keeps that id). The new file is a fresh
    /// conversation, so the episode/title/offset state resets; overlay events
    /// still carry the original session id so the frontend can map them.
    ///
    /// Task accounting crosses over for the same reason it crosses a fork
    /// (see `rearm`): `/clear` replaces Claude's context, it does not stop the
    /// delegations and background shells already running — dropping them would
    /// zero `outstanding`, release the frontend's sweep exemption, and let
    /// closing the tab kill work that is still in flight.
    fn adopt_rollover(&mut self, new_id: String, f: PathBuf) {
        let session_id = self.session_id.take();
        let tasks = std::mem::take(&mut self.tasks);
        let settled_ids = std::mem::take(&mut self.settled_ids);
        *self = Self::new();
        self.session_id = session_id;
        self.tasks = tasks;
        self.settled_ids = settled_ids;
        self.pending_transcript_id = Some(new_id.clone());
        self.rolled_over_id = Some(new_id);
        self.epoch = Some(std::time::UNIX_EPOCH);
        self.adopt_file(f);
    }

    fn poll_delay(&self) -> Duration {
        let recently_active = self
            .last_disk_activity
            .is_some_and(|at| at.elapsed() < RECENT_ACTIVITY_WINDOW);
        if !self.tasks.is_empty() || recently_active {
            POLL_ACTIVE
        } else {
            POLL_IDLE
        }
    }

    /// One poll tick: stat-gate, tail-read complete lines, account + classify
    /// each record, regroup the episode, and decide what (if anything) to
    /// emit. Never panics on malformed input — bad lines are skipped.
    ///
    /// `is_prompting` is a snapshot of the connection's `Prompting` status
    /// taken by the async caller right before this (blocking) tick runs — see
    /// `current_turn_launched_ids`'s doc comment for why the watcher needs it.
    /// `turn_ended_abnormally` is a snapshot of `SessionState::
    /// last_turn_ended_abnormally` taken at the same instant — meaningful only
    /// on the tick that observes the falling edge (see below).
    pub(crate) fn tick(
        &mut self,
        ledger: &PromptLedger,
        cwd: &str,
        conn_id: &str,
        is_prompting: bool,
        turn_ended_abnormally: bool,
    ) -> Option<AcpEvent> {
        let session_id = self.session_id.clone()?;

        // Rising edge (a fresh turn started prompting): ids a PAST turn
        // launched must not suppress an out-of-turn follow-up that has
        // nowhere else to render. Falling edge: if the turn ended abnormally
        // (cancelled/refused/etc — its content never reached the wire),
        // release its launched ids NOW rather than waiting for the next
        // rising edge — there is no live view left for a late notification to
        // duplicate, so the overlay is correctly the only place left for it
        // to render. A NORMAL falling edge leaves the set untouched: it stays
        // suppression-eligible, with no time limit, until the next rising
        // edge (see `current_turn_launched_ids`'s doc comment).
        // `account()` reads `currently_prompting` per-line below without its
        // own parameter.
        if is_prompting && !self.was_prompting {
            self.current_turn_launched_ids.clear();
        }
        if !is_prompting && self.was_prompting && turn_ended_abnormally {
            self.current_turn_launched_ids.clear();
        }
        // Not prompting ⇒ no submission is in flight, so the window is closed.
        // Deliberately a LEVEL check, not the falling edge the launched-ids use:
        // an edge can be missed entirely (a whole turn can start and finish
        // between two polls, and this tick's own lines can re-open the window
        // AFTER the edge was handled), which would strand the flag set with no
        // later edge to clear it — and a stranded flag swallows the next
        // genuinely autonomous initiator. The level check cannot strand.
        // Re-arming within the same tick is still correct: the flag is set by
        // the LEDGER MATCH in the line loop below, not by this snapshot, so a
        // turn read entirely after it ended still classifies exactly as it did
        // live. And since the first `!is_prompting` observation is the falling
        // edge, this adds no new exposure to the snapshot's race with the wire.
        if !is_prompting {
            self.foreground_awaiting_reply = false;
        }
        self.was_prompting = is_prompting;
        self.currently_prompting = is_prompting;

        // Expire tasks past the keep-alive max age so a lost completion can't
        // pin the connection alive forever; the emitted outstanding drop also
        // releases the frontend's sweep exemption mirror.
        let max_age = background_keepalive_max_age()
            .to_std()
            .unwrap_or(Duration::from_secs(3600));
        let before = self.tasks.len();
        self.tasks.retain(|id, t| {
            let keep = t.started_at.elapsed() < max_age;
            if !keep {
                tracing::info!(
                    "[bg-watch] expiring {} task={id} after max-age (completion never observed)",
                    t.kind
                );
            }
            keep
        });
        let expired_any = self.tasks.len() != before;

        // Locate the transcript (it may not exist yet for a brand-new
        // session; retry every tick until it does). After a `/clear` the ACP
        // session id names the ABANDONED file, so the lookup asks for the id
        // this watch rolled onto instead.
        if self.file.is_none() {
            let lookup_id = self.rolled_over_id.clone().unwrap_or_else(|| session_id.clone());
            if let Some(f) = find_session_file(&lookup_id) {
                self.adopt_file(f);
            }
            if let Some(f) = &self.file {
                if !self.armed_logged {
                    self.armed_logged = true;
                    tracing::info!(
                        "[bg-watch] armed connection={} session={} baseline={} file={}",
                        conn_id,
                        session_id,
                        self.committed,
                        f.display()
                    );
                }
            }
        }

        // `/clear` leaves the original `{session_id}.jsonl` in place and
        // writes a sibling uuid file. Look for that successor ONLY while a
        // `/clear` this connection sent is still outstanding: the on-disk
        // evidence alone cannot tell our own rollover from the rollover of
        // any other conversation open on the same folder, and adopting a
        // stranger's transcript would re-point this row's `external_id` at a
        // session it does not own (see `PromptLedger::clear_rollover_expected`).
        if ledger.clear_rollover_expected() {
            if let Some(current) = self.file.clone() {
                if let Some((new_id, new_path)) =
                    find_clear_rollover_successor(&current, &session_id)
                {
                    if self.file.as_ref() != Some(&new_path) {
                        tracing::info!(
                            "[bg-watch] /clear rollover connection={} from={} to={} file={}",
                            conn_id,
                            session_id,
                            new_id,
                            new_path.display()
                        );
                        ledger.consume_clear_request();
                        self.adopt_rollover(new_id, new_path);
                    }
                }
            }
        }
        let path = self.file.clone()?;

        // Cheap gate: unchanged (mtime, len) and no pending partial line means
        // nothing to read this tick.
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("[bg-watch] stat failed for {}: {e}", path.display());
                self.file = None; // session file may have moved; re-locate
                return None;
            }
        };
        let stat = (meta.modified().ok(), meta.len());
        let unchanged = self.last_stat.as_ref() == Some(&stat);
        self.last_stat = Some(stat);

        let mut changed_turns: Vec<MessageTurn> = Vec::new();
        let mut settled: Vec<BackgroundSettledInfo> = Vec::new();

        if meta.len() < self.committed {
            // Truncated/rewritten out from under us: re-baseline at EOF. The
            // frontend overlay reconciles on its next detail refetch.
            tracing::warn!(
                "[bg-watch] transcript shrank ({} -> {}), re-baselining",
                self.committed,
                meta.len()
            );
            self.committed = meta.len();
            self.carry.clear();
            self.episode = None;
            self.mode = Mode::Foreground;
        } else if !unchanged {
            match self.read_new_lines(&path) {
                Ok(lines) => {
                    self.read_failed = false;
                    if !lines.is_empty() {
                        self.last_disk_activity = Some(Instant::now());
                    }
                    for line in &lines {
                        let value: serde_json::Value = match serde_json::from_str(line) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        self.account(&value, &mut settled);
                        self.capture_title(&value);
                        self.classify_and_feed(&value, ledger, cwd, &mut changed_turns);
                    }
                    if !lines.is_empty() {
                        // Regroup once per tick (not per record): collect the
                        // episode's turns whose content changed since the last
                        // emission.
                        self.collect_changed_turns(cwd, &mut changed_turns);
                    }
                }
                Err(e) => {
                    // Retried on every tick (`last_stat` forgotten), so a file
                    // that becomes readable again without changing length or
                    // mtime (a permission fix) is read on the next tick. Logged
                    // once per run of failures.
                    if !self.read_failed {
                        tracing::warn!("[bg-watch] read failed for {}: {e}", path.display());
                    }
                    self.read_failed = true;
                    self.last_stat = None;
                    return None;
                }
            }
        }

        // Held-turn OVERLAY suppression: a turn #870 (claude-agent-acp v0.59.0)
        // is holding open for its own spawned sub-agents renders their follow-up
        // content on the wire
        // already, so the OVERLAY copy of that content (a `changed_turns` entry)
        // must NOT also render — that's the double-render this drop prevents. No
        // time window is needed: an id's membership in `current_turn_launched_ids`
        // alone closes the TOCTOU race a naive "is_prompting right now" check
        // would miss (the turn's own tail content can be read by THIS tick
        // strictly after the falling edge, even though it was genuinely
        // wire-rendered a beat earlier while still `Prompting`) — the set simply
        // isn't cleared until the next rising edge (or immediately, for an
        // abnormal ending — see `tick()`'s entry). Every other out-of-turn turn
        // (cron//loop autonomous turns have no originating task id at all; a
        // notification for a task some OTHER, already-superseded turn launched
        // isn't in THIS turn's set; background shells are never inserted into the
        // set at all — see `account()`) passes through unaffected.
        //
        // `settled` is deliberately NOT filtered the same way. It carries the
        // task's terminal state + `<result>` + launching `tool_use_id`, which the
        // frontend needs to flip the launch CARD (`AgentToolCallPart`) from
        // "running in background" to its completed/result form — the ONLY trigger
        // for that flip. Filtering it (as an earlier iteration did) left the card
        // frozen forever, because `settled.push` fires exactly once per
        // notification record and the bytes are never re-read. Un-filtering it
        // does NOT re-introduce a double-render: the frontend patches the
        // existing card in-memory from this payload (`resolveBackgroundTask`)
        // rather than issuing the `refetchDetail` it used to — see §3.2.
        // `outstanding`/`watermark` are computed independently and untouched by
        // this filter, so the sweep-exemption accounting stays accurate.
        changed_turns.retain(|t| {
            let origin = self.turn_origin_task_ids.remove(&t.id).flatten();
            !matches!(origin, Some(task_id) if self.current_turn_launched_ids.contains(&task_id))
        });

        let outstanding = self.tasks.len() as u32;
        let accounting_changed =
            expired_any || self.last_emitted_outstanding != Some(outstanding);
        if changed_turns.is_empty() && settled.is_empty() && !accounting_changed {
            return None;
        }
        self.last_emitted_outstanding = Some(outstanding);
        Some(AcpEvent::BackgroundActivity {
            session_id,
            turns: changed_turns,
            outstanding,
            settled,
            watermark: self.committed,
        })
    }

    /// Adopt a just-discovered transcript file, choosing the arm baseline.
    /// History (records before `epoch`) renders via the detail fetch and is
    /// skipped; records at/after `epoch` — a first prompt or launch ack
    /// written between session creation and this discovery — are processed
    /// like any other appended record. Without an epoch (never armed via
    /// `rearm`), falls back to EOF, the pure-history behavior.
    fn adopt_file(&mut self, f: PathBuf) {
        if let Ok(meta) = std::fs::metadata(&f) {
            // `baseline_offset_since` never lands inside a trailing partial
            // line; the EOF fallback covers only an unreadable file (where
            // the tail reader will re-locate anyway).
            self.committed = self
                .epoch
                .and_then(|e| baseline_offset_since(&f, e))
                .unwrap_or(meta.len());
            // Deliberately do NOT pre-seed `last_stat`: the stat gate below
            // must see this tick as changed so a baseline that landed BEFORE
            // EOF (pre-discovery records to process) is read immediately, not
            // on the next unrelated append.
            self.seed_titles_from_history(&f, self.committed);
        }
        self.read_failed = false;
        self.file = Some(f);
    }

    /// Fold the title records in the SKIPPED history into the title slots,
    /// without queueing any of them for publication.
    ///
    /// `customTitle ?? aiTitle` is a WHOLE-FILE rule, and Claude Code appends
    /// the two records INDEPENDENTLY: `/rename` writes a lone `custom-title`,
    /// the background summarizer writes a lone `ai-title` (verified in the
    /// 2.1.185 CLI — two separate one-record writers, plus a metadata flush
    /// that re-emits whichever are set). A session renamed before this watch
    /// armed therefore keeps its `custom-title` entirely in the history the
    /// baseline skips, and resolving over the tail alone would let the next
    /// `ai-title` publish over the user's own name — which the CLI re-emits
    /// constantly (228 identical copies in one observed transcript), so the
    /// exposure is not theoretical. Seeding costs one bounded read of the
    /// prefix, once per arm, on top of the whole-file read
    /// `baseline_offset_since` just did.
    ///
    /// `pending_title` is deliberately untouched: history renders through the
    /// ordinary detail fetch, which already resolved this same pair over these
    /// same bytes, so re-publishing it would rename on every reconnect.
    fn seed_titles_from_history(&mut self, path: &PathBuf, upto: u64) {
        if upto == 0 {
            return;
        }
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        let mut reader = std::io::BufReader::new(file.take(upto));
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            // Mirror the tail reader: a non-UTF-8 (or unparsable) line is
            // skipped, never fatal to the rest of the scan.
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim_end()) else {
                continue;
            };
            let Some(record_type) = value.get("type").and_then(|t| t.as_str()) else {
                continue;
            };
            capture_title_record(
                &value,
                record_type,
                &mut self.custom_title,
                &mut self.ai_title,
            );
        }
    }

    /// Read bytes appended since `committed`, returning COMPLETE lines only;
    /// a trailing partial line stays in `carry` until its newline arrives.
    ///
    /// Linear in the bytes read: each line is copied out of `carry` once, then
    /// the unread tail moves to a buffer of its own. One append can be large —
    /// a resume replays the surviving history into the same file — and
    /// splitting `carry` line by line instead gave every line a buffer as big
    /// as everything still unread, about bytes × lines / 2 in all (#885).
    fn read_new_lines(&mut self, path: &PathBuf) -> std::io::Result<Vec<String>> {
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(self.committed + self.carry.len() as u64))?;
        // Straight into `carry`. Bytes read before an error stay there, so the
        // next call returns their complete lines even if nothing new arrives.
        f.read_to_end(&mut self.carry)?;

        let mut lines = Vec::new();
        let mut consumed = 0;
        for line in self.carry.split_inclusive(|b| *b == b'\n') {
            let Some(body) = line.strip_suffix(b"\n") else {
                break; // the partial tail
            };
            consumed += line.len();
            // Mirror the detail parser: a non-UTF-8 line is skipped, but its
            // bytes still count toward the watermark.
            if let Ok(text) = std::str::from_utf8(body) {
                lines.push(text.to_owned());
            }
        }
        self.committed += consumed as u64;
        // Not `drain`: that would keep the whole batch's allocation alive
        // behind a partial line for as long as the session is watched.
        self.carry = self.carry[consumed..].to_vec();
        Ok(lines)
    }

    /// Task accounting for one record: launch acks; settlements via a
    /// `<task-notification>` record or one folded into a running turn, a
    /// `TaskOutput` result reaching a terminal `task.status`, or a
    /// `TaskStop`/`KillShell` call; and `SendMessage` re-arms of settled
    /// sub-agents.
    fn account(&mut self, value: &serde_json::Value, settled: &mut Vec<BackgroundSettledInfo>) {
        let record_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match record_type {
            // A task that settled while a turn ran: its notification is folded
            // into that turn (see `queued_task_notification`), and this is the
            // usual way a background task ends. Missed, the task kept the
            // connection exempt from the idle sweeps until its keep-alive
            // expired. Like a `TaskOutput` collection, it only leaves the
            // count, with no `settled` push: the agent takes the result inline,
            // so there is nothing to announce out of turn, and the launch card
            // gets its final state from the detail parse.
            "attachment" => {
                if let Some(id) = queued_task_notification(value)
                    .and_then(|raw| capture_tag(task_notification_task_id_regex(), raw))
                {
                    if self.tasks.remove(&id).is_some() {
                        tracing::info!("[bg-watch] settled task={id} folded into a turn");
                    }
                    self.settled_ids.insert(id);
                }
            }
            "user" => {
                if let Some(tur) = value.get("toolUseResult") {
                    if tur.get("status").and_then(|s| s.as_str()) == Some("async_launched") {
                        if let Some(id) = tur
                            .get("agentId")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                        {
                            // `entry().or_insert_with()`, not a blind `insert`:
                            // a re-observed id (e.g. a resumed sub-agent's ack
                            // repeating) must not reset `started_at` to now —
                            // that would restart the max-age clock from
                            // whatever tick last saw it instead of counting
                            // from first launch, silently extending how long a
                            // truly-abandoned task can pin the connection
                            // alive. The log fires only on first registration.
                            self.tasks.entry(id.to_string()).or_insert_with(|| {
                                tracing::info!("[bg-watch] registered async agent task={id}");
                                TaskEntry {
                                    kind: "agent",
                                    started_at: Instant::now(),
                                }
                            });
                            // This turn is still `Prompting` at launch time — a
                            // later out-of-turn follow-up for this SAME id is
                            // therefore held-open content already rendering on
                            // the wire (see `current_turn_launched_ids`'s doc
                            // comment); mark it so `tick()`'s suppression
                            // filter can catch it.
                            if self.currently_prompting {
                                self.current_turn_launched_ids.insert(id.to_string());
                            }
                        }
                    } else if let Some(id) = tur
                        .get("backgroundTaskId")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        // Same first-seen rationale as the agent branch above —
                        // doubly important here since a still-running shell is
                        // typically observed via REPEATED `BashOutput`-style
                        // reads of this identical shape.
                        self.tasks.entry(id.to_string()).or_insert_with(|| {
                            tracing::info!("[bg-watch] registered background shell task={id}");
                            TaskEntry {
                                kind: "shell",
                                started_at: Instant::now(),
                            }
                        });
                        // Deliberately NOT inserted into `current_turn_launched_ids`:
                        // #870 never holds a turn open for a shell (this
                        // module's own top-of-file doc comment — "a hold must
                        // NEVER wait on a shell"), so a shell's owning turn
                        // always ends via an ordinary `end_turn` while the
                        // shell keeps running. If shells were suppression-
                        // eligible, the unbounded (until-next-turn) lifetime
                        // of that set would silently swallow a shell's
                        // eventual completion for its entire realistic
                        // runtime — content that was never on the wire in the
                        // first place, with nothing to fall back on.
                    }

                    // Settle a task the agent collected via `TaskOutput`: its
                    // structured `task.status` reaching a terminal state means
                    // the task finished (exit recorded), even though no
                    // `<task-notification>` was ever written — the agent
                    // awaited it inline. This is the DOMINANT settle path for
                    // background shells, which almost never notify. No `settled`
                    // push on purpose: an inline-awaited collection must not
                    // raise an out-of-turn OS notification (the agent is
                    // mid-turn and already holds the result); only the
                    // outstanding count drops. `settled_ids` still records it so
                    // a later `SendMessage` resume can re-arm.
                    if let Some(task) = tur.get("task") {
                        if let Some(id) = task
                            .get("task_id")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                        {
                            let status =
                                task.get("status").and_then(|s| s.as_str()).unwrap_or("");
                            if is_terminal_task_status(status) && self.tasks.remove(id).is_some() {
                                self.settled_ids.insert(id.to_string());
                                tracing::info!(
                                    "[bg-watch] settled task={id} via TaskOutput status={status}"
                                );
                            }
                        }
                    }
                }
                if let Some(text) = user_record_text(value) {
                    let trimmed = text.trim_start();
                    if trimmed.starts_with("<task-notification>") {
                        let task_id = capture_tag(task_notification_task_id_regex(), trimmed);
                        let status = capture_tag(task_notification_status_regex(), trimmed)
                            .unwrap_or_else(|| "completed".into());
                        let summary = capture_tag(task_notification_summary_regex(), trimmed);
                        // The notification is self-contained: its `<tool-use-id>`
                        // is the launching tool call's id and `<result>` is the
                        // sub-agent's report. Carrying both lets the frontend flip
                        // the launch card in-memory (rewriting its marker) with no
                        // `refetchDetail` — see `BackgroundSettledInfo`'s doc. A
                        // background shell's notification names its `Bash` call
                        // as well (with no `<result>`); the frontend leaves that
                        // card alone.
                        let tool_use_id =
                            capture_tag(task_notification_tool_use_id_regex(), trimmed);
                        // Same cap the cold-parse fold applies, so the live card
                        // matches and a pathological report can't blow the
                        // event-stream size budget.
                        let result = capture_tag(task_notification_result_regex(), trimmed)
                            .map(|r| truncate_str(&r, BACKGROUND_RESULT_MAX_CHARS));
                        if let Some(id) = task_id {
                            let known = self.tasks.remove(&id).is_some();
                            self.settled_ids.insert(id.clone());
                            tracing::info!(
                                "[bg-watch] settled task={id} status={status} known={known}"
                            );
                            settled.push(BackgroundSettledInfo {
                                task_id: id,
                                status,
                                summary,
                                tool_use_id,
                                result,
                            });
                        }
                    }
                }
            }
            "assistant" => {
                let Some(blocks) = value
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                else {
                    return;
                };
                for block in blocks {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                        continue;
                    }
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let input = block.get("input");
                    match name {
                        // A settled sub-agent can be resumed: `SendMessage(to:
                        // <id>)` re-arms its accounting entry (it will notify
                        // again — the notification's own <note> documents
                        // multi-notify).
                        "SendMessage" => {
                            let Some(to) =
                                input.and_then(|i| i.get("to")).and_then(|t| t.as_str())
                            else {
                                continue;
                            };
                            if self.settled_ids.remove(to) {
                                tracing::info!("[bg-watch] re-armed resumed task={to}");
                                self.tasks.insert(
                                    to.to_string(),
                                    TaskEntry {
                                        kind: "agent",
                                        started_at: Instant::now(),
                                    },
                                );
                                // Mirrors the launch-time insert in the
                                // `async_launched` branch above: if THIS turn
                                // (the one issuing the resume) is itself held
                                // open by #870 for the resumed sub-agent, its
                                // second notification must be suppression-
                                // eligible the same way a freshly-launched
                                // one is — otherwise a resume-then-hold
                                // reproduces the same double-render.
                                if self.currently_prompting {
                                    self.current_turn_launched_ids.insert(to.to_string());
                                }
                            }
                        }
                        // Explicit kill: the background task's process is gone,
                        // so it must leave the outstanding count now — no
                        // completion notification will follow. `TaskStop` names
                        // it via `task_id`, `KillShell` via `shell_id`.
                        "TaskStop" | "KillShell" => {
                            if let Some(id) = input
                                .and_then(|i| i.get("task_id").or_else(|| i.get("shell_id")))
                                .and_then(|t| t.as_str())
                                .filter(|s| !s.is_empty())
                            {
                                if self.tasks.remove(id).is_some() {
                                    self.settled_ids.insert(id.to_string());
                                    tracing::info!("[bg-watch] settled task={id} via {name}");
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Classify a record against the prompt ledger and feed out-of-turn ones
    /// into the current episode.
    fn classify_and_feed(
        &mut self,
        value: &serde_json::Value,
        ledger: &PromptLedger,
        cwd: &str,
        changed_turns: &mut Vec<MessageTurn>,
    ) {
        // The model has started answering the matched prompt: the rest of the
        // transcript is fair game for out-of-turn classification again.
        if value.get("type").and_then(|t| t.as_str()) == Some("assistant") {
            self.foreground_awaiting_reply = false;
        }

        // A message folded into the running turn continues that turn and never
        // starts one. Matched against the ledger — a prompt codeg sent that
        // Claude Code folded into a turn it started on its own, or a steer — it
        // hands the rest of the turn to the wire, so an open episode closes as
        // for any codeg-sent prompt. Unmatched, it stays with whatever turn is
        // running, the overlay's or the wire's. The steer arm records its
        // fingerprint only once the adapter answers `injected`, so the CLI can
        // write the attachment before the ledger has it; taken for an
        // out-of-turn start, it would draw the rest of a wire-rendered turn in
        // the overlay as well.
        if let Some(text) = folded_prompt_text(value) {
            if ledger.consume_matching(&TurnInitiatorText::Verbatim(text)) {
                tracing::debug!("[bg-watch] folded prompt matched ledger");
                self.collect_changed_turns(cwd, changed_turns);
                self.episode = None;
                self.mode = Mode::Foreground;
                self.foreground_awaiting_reply = true;
                self.foreground_submission_id = None;
                return;
            }
        }

        if let Some(initiator) = turn_initiator_text(value) {
            let initiator_text = initiator.as_str();
            if ledger.consume_matching(&initiator) {
                // A codeg-sent prompt: the wire renders this turn. Close any
                // open episode first (flush its final state) and go silent.
                tracing::debug!("[bg-watch] foreground turn matched ledger");
                self.collect_changed_turns(cwd, changed_turns);
                self.episode = None;
                self.mode = Mode::Foreground;
                self.foreground_awaiting_reply = true;
                self.foreground_submission_id = record_submission_id(value);
                return;
            }
            // A `<task-notification>` is never part of a submission: it is
            // background work settling on its own schedule, and the episode it
            // opens is the only place its follow-up can render (the wire never
            // carries it). It must classify out-of-turn even inside the window
            // — the one initiator the window may not swallow.
            if self.foreground_awaiting_reply
                && self.foreground_submission_id.is_some()
                && record_submission_id(value) == self.foreground_submission_id
                && task_notification_origin_id(initiator_text).is_none()
            {
                // Still inside the matched prompt's own submission — command
                // output, the instruction `/goal` injects, image metadata. None
                // of it starts a turn; the wire is rendering the one it belongs
                // to. See `foreground_awaiting_reply`.
                tracing::debug!(
                    "[bg-watch] submission record before the reply, staying foreground: {:?}",
                    initiator_text.chars().take(60).collect::<String>()
                );
                return;
            }
            tracing::debug!(
                "[bg-watch] out-of-turn initiator: {:?}",
                initiator_text.chars().take(60).collect::<String>()
            );
            let rotate = self
                .episode
                .as_ref()
                .is_some_and(|e| e.acc.messages.len() >= MAX_EPISODE_MESSAGES);
            if matches!(self.mode, Mode::Foreground) || self.episode.is_none() || rotate {
                if rotate {
                    self.collect_changed_turns(cwd, changed_turns);
                }
                // Any stable, strictly-increasing base works — turn ids only
                // need to be unique and stable within the watch.
                self.episode = Some(Episode {
                    start_offset: self.next_episode_base(),
                    acc: ClaudeRecordAccumulator::new(
                        self.file.clone().unwrap_or_else(|| PathBuf::from("")),
                    ),
                    emitted_hashes: HashMap::new(),
                    origin_task_id: task_notification_origin_id(initiator_text),
                });
            }
            self.mode = Mode::Background;
        }

        if matches!(self.mode, Mode::Background) {
            // Mid-turn safety valve: one giant turn with no boundary would
            // otherwise grow the episode — and the per-tick regroup over it —
            // without bound. Flush and re-base; the seam is cosmetic and the
            // next detail refetch renders the turn whole.
            let force_rotate = self
                .episode
                .as_ref()
                .is_some_and(|e| e.acc.messages.len() >= FORCE_ROTATE_MESSAGES);
            if force_rotate {
                tracing::warn!(
                    "[bg-watch] episode reached {FORCE_ROTATE_MESSAGES} messages without a turn \
                     boundary — force-rotating (the in-progress turn renders split until the \
                     next detail refetch)"
                );
                self.collect_changed_turns(cwd, changed_turns);
                // Cosmetic re-basing of the SAME continuous out-of-turn
                // stretch (not a new initiator record) — the origin carries
                // over unchanged.
                let inherited_origin =
                    self.episode.as_ref().and_then(|e| e.origin_task_id.clone());
                self.episode = Some(Episode {
                    start_offset: self.next_episode_base(),
                    acc: ClaudeRecordAccumulator::new(
                        self.file.clone().unwrap_or_else(|| PathBuf::from("")),
                    ),
                    emitted_hashes: HashMap::new(),
                    origin_task_id: inherited_origin,
                });
            }
            if let Some(episode) = self.episode.as_mut() {
                episode.acc.feed_value(value.clone());
            }
        }
    }

    /// Regroup the open episode with the detail parser's Stage B + post-
    /// processing and append turns whose content changed since last emission.
    fn collect_changed_turns(&mut self, cwd: &str, out: &mut Vec<MessageTurn>) {
        let Some(episode) = self.episode.as_mut() else {
            return;
        };
        if episode.acc.messages.is_empty() {
            return;
        }
        let origin_task_id = episode.origin_task_id.clone();
        let mut messages = episode.acc.messages.clone();
        // An autonomous turn can itself launch background work; fold any
        // ack+notification pairs seen within this episode, same as the
        // detail parse does.
        episode.acc.apply_background_lifecycle(&mut messages);
        let mut turns = group_into_turns(messages);
        crate::parsers::relocate_orphaned_tool_results(&mut turns);
        crate::parsers::structurize_read_tool_output(&mut turns);
        crate::parsers::resolve_patch_line_numbers(&mut turns, Some(cwd));
        // Same last step as the detail parse, so an overlay turn carries the
        // same elapsed time the reply will show once it settles. An episode
        // that opens mid-reply simply has no boundary for its first turn.
        crate::parsers::backfill_turn_durations(&mut turns, &[]);
        for (idx, mut turn) in turns.into_iter().enumerate() {
            turn.id = format!("bg-{}-{}", episode.start_offset, idx);
            let hash = hash_turn(&turn);
            if episode.emitted_hashes.get(&turn.id) == Some(&hash) {
                continue;
            }
            episode.emitted_hashes.insert(turn.id.clone(), hash);
            // Recorded for `tick()`'s held-turn suppression filter, drained
            // there the same tick it's populated — never outlives one tick.
            self.turn_origin_task_ids
                .insert(turn.id.clone(), origin_task_id.clone());
            out.push(turn);
        }
    }

    #[cfg(test)]
    fn with_file_for_test(session_id: &str, file: PathBuf) -> Self {
        let mut ws = Self::new();
        ws.session_id = Some(session_id.to_string());
        ws.file = Some(file);
        ws
    }
}

/// Extract the text of a user record's content: bare string form, or the
/// concatenated text blocks of the array form. `None` for non-user records.
fn user_record_text(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    content_text(value.get("message")?.get("content")?)
}

/// The text of a user `content` value — a record's `message.content`, or the
/// prompt of a message folded into a running turn: the bare string form, or
/// the concatenated text blocks of the array form.
fn content_text(content: &serde_json::Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let arr = content.as_array()?;
    let texts: Vec<&str> = arr
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect();
    if texts.is_empty() {
        return None;
    }
    Some(texts.join("\n"))
}

/// The text of a record that STARTS a new logical turn, or `None` for records
/// that continue the current one. Ground rules (from real transcripts):
///
/// * tool-result-only user records continue the in-progress turn;
/// * `isMeta` ARRAY records are slash-command expansions — part of the
///   foreground turn that issued the command (a cron prompt is `isMeta` too,
///   but always STRING content, so it still initiates);
/// * auto-compaction continuation summaries land MID-turn while the wire is
///   still rendering it — never a boundary;
/// * everything else user-typed/injected (real prompts, `<task-notification>`
///   records, cron prompts) initiates.
///
/// A message folded into the running turn is an attachment, not a `user`
/// record, and never initiates: see [`folded_prompt_text`].
fn turn_initiator_text(value: &serde_json::Value) -> Option<TurnInitiatorText> {
    if value.get("type").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    let content = value.get("message")?.get("content")?;

    if let Some(s) = content.as_str() {
        if s.starts_with(CONTEXT_CONTINUATION_PREFIX) {
            return None;
        }
        // A slash command persists as command tags; codeg sent the display
        // form ("/name args"), so match the ledger against that.
        if let Some(display) = slash_command_display(s) {
            return Some(TurnInitiatorText::ReconstructedSlashCommand(display));
        }
        return Some(TurnInitiatorText::Verbatim(s.to_string()));
    }

    let arr = content.as_array()?;
    if !arr.is_empty()
        && arr
            .iter()
            .all(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
    {
        return None;
    }
    if is_meta_message(value) {
        return None;
    }
    let text = user_record_text(value)?;
    if text.starts_with(CONTEXT_CONTINUATION_PREFIX) {
        return None;
    }
    Some(TurnInitiatorText::Verbatim(text))
}

/// The text of a message the user sent mid-turn, which Claude Code folded into
/// the running turn and recorded as an attachment ([`queued_human_prompt`]).
/// `None` for any other record, and for one with no text (an image on its
/// own): there is nothing to match, as an image-only prompt leaves no
/// fingerprint either.
fn folded_prompt_text(value: &serde_json::Value) -> Option<String> {
    content_text(queued_human_prompt(value)?).filter(|text| !text.trim().is_empty())
}

/// The submission a record belongs to. Claude Code stamps every user record it
/// writes while composing and running ONE prompt with that prompt's `promptId`
/// (verified across real transcripts: 13 prompts in a session carried 10
/// distinct ids, repeating only across a prompt's own interrupt/retry), so it is
/// the only reliable way to tell a submission's side records — command output,
/// the instruction `/goal` injects — from an unrelated initiator.
fn record_submission_id(value: &serde_json::Value) -> Option<String> {
    value
        .get("promptId")
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// If `text` (an out-of-turn initiator from `turn_initiator_text`) is a
/// `<task-notification>` record, its `<task-id>` — mirroring `account()`'s
/// exact gate so the two never diverge on what counts as a task-notification.
/// `None` for any other out-of-turn initiator (a cron prompt, other injected
/// text), which has no originating task to attribute an episode to.
fn task_notification_origin_id(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("<task-notification>") {
        return None;
    }
    capture_tag(task_notification_task_id_regex(), trimmed)
}

/// The arm baseline separating pre-existing history from records written
/// during this watch's lifetime: the byte offset of the first COMPLETE
/// CONVERSATION line (a `user`/`assistant` record) whose timestamp is at or
/// after `epoch`, or — when no such record exists yet — the end of the last
/// COMPLETE line. The fallback deliberately excludes a trailing partial line:
/// a fragment present at arm time is a record being flushed RIGHT NOW
/// (post-epoch by definition), and baselining past it (EOF) would leave only
/// its unparseable suffix for the tail reader, silently dropping the very
/// record the epoch baseline exists to preserve.
///
/// Only `user`/`assistant` records delimit the boundary; every other record
/// type (and any line without a parseable timestamp) is skipped. This is
/// load-bearing for FORK: a fork copies the parent transcript into the new
/// session file preserving each record's ORIGINAL, pre-fork timestamp, then
/// writes fresh metadata records (`queue-operation`, `mode`, …) at the FILE
/// HEAD stamped at fork time. Those head records are AHEAD of the copied
/// history by byte offset but AFTER it by timestamp, so keying the boundary on
/// "first record at/after epoch" of ANY type would return the head metadata's
/// offset and drag the entire copied history (which renders via the detail
/// fetch) into the out-of-turn overlay — duplicating it. Restricting the
/// boundary to conversation records lands it on the first genuinely-new turn,
/// with the copied history (older timestamps) correctly on the history side.
/// The skipped metadata carries no turn or accounting the watcher consumes.
/// `None` only when the file can't be read. One-shot cost at arm time (runs
/// inside the tick's `spawn_blocking`).
fn baseline_offset_since(path: &PathBuf, epoch: std::time::SystemTime) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let epoch: chrono::DateTime<chrono::Utc> = epoch.into();
    let mut offset = 0u64;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if line.last() != Some(&b'\n') {
            break;
        }
        let start = offset;
        offset += line.len() as u64;
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        // Only real conversation records anchor the boundary; fork-time head
        // metadata (see the doc comment) must never be the boundary.
        let record_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if record_type != "user" && record_type != "assistant" {
            continue;
        }
        let Some(ts) = value.get("timestamp").and_then(|t| t.as_str()) else {
            continue;
        };
        let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
            continue;
        };
        if parsed.with_timezone(&chrono::Utc) >= epoch {
            return Some(start);
        }
    }
    Some(offset)
}

/// Whether a `TaskOutput` `task.status` is terminal — the task has stopped and
/// must leave the outstanding count. `"running"` (a non-blocking poll of a
/// still-live task) is deliberately excluded so a status check doesn't clear a
/// task that is genuinely still working.
fn is_terminal_task_status(status: &str) -> bool {
    matches!(
        status,
        "completed"
            | "failed"
            | "canceled"
            | "cancelled"
            | "killed"
            | "stopped"
            | "timeout"
            | "timed_out"
            | "error"
    )
}

fn hash_turn(turn: &MessageTurn) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match serde_json::to_string(turn) {
        Ok(s) => s.hash(&mut hasher),
        Err(_) => turn.blocks.len().hash(&mut hasher),
    }
    hasher.finish()
}

#[cfg(test)]
mod tests;

