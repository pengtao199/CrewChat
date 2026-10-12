// broker submodule: status assembly / block probing (split from mod.rs)
use super::*;


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


/// What [`DelegationBroker::claim_new_blocks`] decided for one status pass.
#[derive(Default)]
pub(crate) struct ClaimedBlocks {
    /// At least one blocking prompt is reportable now — the wait should return.
    pub(crate) any_claimed: bool,
    /// Earliest moment a currently-suppressed prompt becomes reportable again.
    /// Bounds how long an otherwise-unbounded park may sleep, so the
    /// resurface valve actually fires instead of waiting on a notify that will
    /// never come (a blocked child emits nothing further on its own).
    pub(crate) retry_at: Option<Instant>,
}


/// Per-id classification captured under the pending lock during a (possibly
/// batched) status query. The async resolution that can't run under the lock —
/// `attach_live_reply` (a different lock) for a running task, `status_from_db`
/// (a DB round-trip) for one not in memory — is deferred to `assemble_reports`
/// AFTER the lock is released, so a status query never nests the pending lock
/// inside another await. This is the same lock-ordering the single-task path
/// has always used; batching just captures it per id.
pub(crate) enum StatusClass {
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
pub(crate) fn classify_locked(inner: &PendingInner, parent_connection_id: &str, task_id: &str) -> StatusClass {
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

impl DelegationBroker {

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
    pub(crate) async fn probe_blocked(&self, classes: &[StatusClass]) -> Vec<Option<BlockedOn>> {
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
    pub(crate) async fn claim_new_blocks(
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
    pub(crate) async fn assemble_reports(
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
    pub(crate) async fn attach_live_reply(
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


    /// DB status fallback for a task evicted from / never in the in-memory maps.
    /// Scopes to the caller's conversation: a child whose `parent_id` doesn't
    /// match (or when the caller has no active conversation) reports `Unknown`.
    pub(crate) async fn status_from_db(
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
}
