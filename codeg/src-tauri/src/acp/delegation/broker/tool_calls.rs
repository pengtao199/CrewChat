// broker submodule: pending tool-call tracking (split from mod.rs)
use super::*;


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
pub(crate) struct PreCanceledHandles {
    pub(crate) inner: Mutex<PreCanceledState>,
}


#[derive(Default)]
pub(crate) struct PreCanceledState {
    pub(crate) set: HashSet<String>,
    pub(crate) order: VecDeque<String>,
}


pub(crate) const PRE_CANCELED_CAP: usize = 256;


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
pub(crate) struct ToolCallTracker {
    pub(crate) inner: Mutex<HashMap<String, ToolCallTrackerBucket>>,
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
pub(crate) struct PendingToolCall {
    pub(crate) tool_call_id: String,
    /// The `(agent_type, task, working_dir)` correlation key parsed from the ACP
    /// tool_call's `raw_input`. Matched against the MCP round-trip's own
    /// key so parallel `delegate_to_agent` calls each bind to their own
    /// `tool_call_id` regardless of arrival order — pure arrival-order FIFO
    /// can mis-assign them (or, when one MCP round-trip out-races the
    /// matching ACP event, orphan to a synthetic id). `None` when the host
    /// shipped no parseable `raw_input` at ToolCall time; such entries are
    /// claimable ONLY via the post-budget FIFO fallback
    /// (`take_pending_tool_call`), never the in-loop key-match path.
    pub(crate) match_key: Option<DelegationMatchKey>,
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
    pub(crate) identityless: bool,
    pub(crate) registered_at: Instant,
}


#[derive(Default)]
pub(crate) struct ToolCallTrackerBucket {
    pub(crate) pending: VecDeque<PendingToolCall>,
    pub(crate) consumed: VecDeque<(String, Instant)>,
    /// Sticky "this parent has EVER announced an identity-less MCP call" flag.
    /// Gates [`DelegationBroker::rewrite_identityless_tool_call`]'s poll: on
    /// hosts that ship real identities (Claude/Codex/…) no identity-less
    /// entry ever registers, the flag stays `false`, and every status/cancel
    /// round-trip skips the rename at zero cost instead of burning the poll
    /// budget. Never cleared on claim — only with the bucket on teardown.
    pub(crate) identityless_seen: bool,
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
pub(crate) const PENDING_TOOL_CALL_TTL: Duration = Duration::from_secs(60);


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
pub(crate) const CLAIM_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) const CLAIM_POLL_ATTEMPTS: usize = 200;


/// Poll budget for the status/cancel call-time rename
/// ([`DelegationBroker::rewrite_identityless_tool_call`]): 30 × 10 ms = 300 ms.
/// Much shorter than the delegate claim budget because the rename is
/// best-effort (a miss falls back to the completion-time result sniff) and the
/// poll delays the status snapshot the LLM is waiting on. It only has to
/// absorb the in-process ordering race between the ACP announcement landing in
/// the lifecycle dispatcher and the companion's UDS round-trip reaching the
/// listener — sub-5 ms in practice; the sticky `identityless_seen` gate keeps
/// hosts that never announce identity-less calls at zero cost.
pub(crate) const IDENTITYLESS_RENAME_POLL_ATTEMPTS: usize = 30;

impl DelegationBroker {

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
    pub(crate) async fn register_pending_tool_call_with_key_at(
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
    pub(crate) async fn take_pending_tool_call_at(
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
    pub(crate) async fn take_identityless_tool_call(&self, parent_connection_id: &str) -> Option<String> {
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
    pub(crate) async fn take_unkeyed_tool_call_at(
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
    pub(crate) async fn has_seen_identityless_tool_calls(&self, parent_connection_id: &str) -> bool {
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
    pub(crate) async fn take_matching_tool_call_at(
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
    pub(crate) async fn consume_explicit_tool_call(&self, parent_connection_id: &str, tool_call_id: &str) {
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
    pub(crate) async fn claim_pending_tool_call_with_brief_wait(
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
    pub(crate) async fn take_pre_canceled_handle(&self, handle: &str) -> bool {
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
    pub(crate) async fn buffer_pre_canceled_handle(&self, handle: String) {
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
    pub(crate) async fn drop_tool_calls_for_parent(&self, parent_connection_id: &str, keep_consumed: bool) {
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
}
