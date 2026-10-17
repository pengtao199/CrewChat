use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：pump
impl TaskEngine {
    /// Advance the folder's merge slot: the user's merge queue first, then the
    /// auto-merge train. Called wherever that slot can have freed (a settled
    /// merge, crash recovery, a fresh review, the reconcile tick) — the two
    /// sources of landings share one slot, so they share one pump.
    pub(super) async fn merge_pump(self: &Arc<Self>, folder_id: i32) {
        if self.drain_merge_queue(folder_id).await {
            return;
        }
        self.auto_merge_sweep(folder_id).await;
    }

    /// Dispatch the oldest merge the user queued on this folder. Returns
    /// whether the merge slot is spoken for — the auto sweep must not chase a
    /// user's landing into the same slot, and a queued column drains one
    /// landing per pump (every settle pumps again).
    ///
    /// A queue that changed under the scan (the user withdrew or edited a merge
    /// while this was working) is re-scanned rather than worked from the stale
    /// picture: continuing would dispatch out of order, and reporting an empty
    /// queue would hand the slot to the auto sweep — which has neither the
    /// user's commit message nor their worktree choice. Bounded, because each
    /// retry needs a fresh concurrent change to happen at all; still churning
    /// after that leaves the slot to the next pump.
    async fn drain_merge_queue(self: &Arc<Self>, folder_id: i32) -> bool {
        for _ in 0..MERGE_QUEUE_DRAIN_ATTEMPTS {
            match self.drain_merge_queue_once(folder_id).await {
                DrainOutcome::Taken => return true,
                DrainOutcome::Empty => return false,
                DrainOutcome::Stale => continue,
            }
        }
        true
    }

    /// One pass over the folder's queue. See [`Self::drain_merge_queue`].
    ///
    /// Refusals are handled like the sweep's: a task that left the queue on its
    /// own moves on to the next one, and a real refusal (wrong base branch,
    /// staged changes, a worktree that vanished) banners the row and drops its
    /// queue entry, so a hopeless intent is attempted once rather than looped
    /// over by the reconcile tick.
    async fn drain_merge_queue_once(self: &Arc<Self>, folder_id: i32) -> DrainOutcome {
        // Fast path only — merge_task re-checks under the folder lock.
        let merging = work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Merging])
            .await
            .unwrap_or_default();
        if merging.iter().any(|t| t.folder_id == folder_id) {
            return DrainOutcome::Taken;
        }
        let mut queued: Vec<_> =
            work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Review])
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|t| t.folder_id == folder_id)
                .filter_map(|t| {
                    // The raw column value travels with the parsed intent: it is
                    // the token every write below CASes on (see
                    // `QueuedMergeClaim`).
                    let raw = t.pending_merge.clone()?;
                    let intent = work_task_service::queued_merge(Some(raw.as_str()))?;
                    let claim = QueuedMergeClaim {
                        raw,
                        queued_at: intent.queued_at,
                    };
                    Some((intent, claim, t))
                })
                .collect();
        queued.sort_by_key(|(intent, _, task)| queue_order(intent, task));
        for (intent, claim, task) in queued {
            if self.live_worktree(&task).await.is_none() {
                // Only the intent we scanned can be refused; anything else on
                // the row now is a change we have to re-read.
                if !self
                    .refuse_queued_merge(&task, &claim, "the task worktree no longer exists on disk")
                    .await
                {
                    return DrainOutcome::Stale;
                }
                continue;
            }
            match self
                .merge_task_inner(
                    task.id,
                    intent.message.clone(),
                    intent.delete_worktree,
                    intent.instructions.clone(),
                    false,
                    Some(&claim),
                )
                .await
            {
                // Queued again: the slot was taken between the scan and the
                // folder lock. The row keeps its place in line untouched.
                Ok(_) => return DrainOutcome::Taken,
                // The user withdrew or edited THIS merge while we worked on it:
                // the whole snapshot is stale (their edit may now sort first,
                // and it must not be left to the auto sweep's defaults).
                Err(e) if is_queued_merge_superseded(&e) => return DrainOutcome::Stale,
                // The task left review on its own — it is out of the queue for
                // good, so the rest of the snapshot still holds.
                Err(e) if is_benign_merge_race(&e) => continue,
                Err(e) => {
                    tracing::warn!("[work_task] queued merge of task {} refused: {e}", task.id);
                    if !self.refuse_queued_merge(&task, &claim, &e).await {
                        return DrainOutcome::Stale;
                    }
                    return DrainOutcome::Taken;
                }
            }
        }
        DrainOutcome::Empty
    }

    /// Drop a queued merge that cannot run and leave the reason on the card —
    /// the queue must not hold an intent the user has no way to see failing.
    ///
    /// Both writes are CAS'd on the refused intent: while the dispatch was
    /// failing the user may have withdrawn it or queued different options, and
    /// neither a silent delete of that newer request nor a banner about an
    /// older one would be true.
    /// `false` = the row moved on, so nothing was written: its current state is
    /// not this refusal's to judge, and the caller must re-read the queue.
    async fn refuse_queued_merge(
        &self,
        task: &crate::db::entities::work_task::Model,
        claim: &QueuedMergeClaim,
        error: &str,
    ) -> bool {
        let dropped = work_task_service::clear_queued_merge(&self.db.conn, task.id, &claim.raw)
            .await
            .unwrap_or(false);
        if !dropped {
            return false;
        }
        let _ = work_task_service::set_review_error(
            &self.db.conn,
            task.id,
            task.run_seq,
            &format!("queued merge failed: {error}"),
        )
        .await;
        self.emit_upsert(task.id);
        true
    }

    // ── auto-merge (unattended landing) ─────────────────────────────────────

    /// Fire-and-forget [`Self::merge_pump`] — a dispatch takes the folder's git
    /// lock to validate and commit its CAS, and waits on the agent coming up
    /// after that, neither of which may stall the engine's event loop.
    pub(super) fn spawn_merge_pump(self: &Arc<Self>, folder_id: i32) {
        let engine = self.clone();
        tokio::spawn(async move {
            engine.merge_pump(folder_id).await;
        });
    }

    /// Pump one folder — or, with `None`, every folder that currently holds a
    /// reviewed task. `None` serves the reconcile tick and a change of the
    /// global settings row, which can switch auto-merge on for any folder that
    /// follows it. Each folder's pump runs spawned, so one folder's dispatch
    /// cannot delay another's.
    pub async fn sweep_merge_backlog(self: &Arc<Self>, folder_id: Option<i32>) {
        match folder_id {
            Some(folder_id) => self.spawn_merge_pump(folder_id),
            None => {
                let review = work_task_service::list_by_status(
                    &self.db.conn,
                    &[WorkTaskStatus::Review],
                )
                .await
                .unwrap_or_default();
                let mut swept: HashSet<i32> = HashSet::new();
                for task in review {
                    if swept.insert(task.folder_id) {
                        self.spawn_merge_pump(task.folder_id);
                    }
                }
            }
        }
    }

    /// When the folder's settings ask for it, dispatch the same merge the
    /// button would — agent-written commit message, worktree per the folder's
    /// default — for the oldest reviewed task that is actually mergeable. One
    /// dispatch per sweep: merges are serial per folder anyway, and every
    /// settle re-sweeps, so a column of reviewed tasks drains one landing at a
    /// time (the merge train).
    ///
    /// Concurrent sweeps, clicks and settles are all safe: `merge_task`
    /// serializes dispatches on the folder git lock and CAS-guards
    /// review→merging, so the worst case is a benign "someone got there first"
    /// refusal, which the sweep swallows. A dispatch refused for a real reason
    /// (wrong base branch, staged changes, …) leaves that reason on the row as
    /// the card's error banner — and a row with an error is no longer
    /// eligible, so a hopeless dispatch is attempted once, not looped.
    async fn auto_merge_sweep(self: &Arc<Self>, folder_id: i32) {
        let settings = work_task_service::settings_get_effective(&self.db.conn, folder_id)
            .await
            .unwrap_or_default();
        if !settings.auto_merge {
            return;
        }
        // Fast path only — merge_task re-checks under the folder lock.
        let merging = work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Merging])
            .await
            .unwrap_or_default();
        if merging.iter().any(|t| t.folder_id == folder_id) {
            return;
        }
        let mut candidates: Vec<_> =
            work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Review])
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|t| t.folder_id == folder_id && auto_merge_candidate(t, &settings))
                .collect();
        candidates.sort_by_key(|t| (t.settled_at, t.id));
        for task in candidates {
            // A gone worktree cannot serve a merge generation; that task's
            // acceptance is the "complete" button — a user decision.
            if self.live_worktree(&task).await.is_none() {
                continue;
            }
            match self
                // No instructions: nobody was at the keyboard to write any.
                .merge_task(task.id, None, settings.delete_worktree_default, None, true)
                .await
            {
                // An unattended dispatch never queues (see `merge_task`), so
                // this is always the live generation.
                Ok(_) => {}
                Err(e) if is_benign_merge_race(&e) => {}
                Err(e) => {
                    tracing::warn!(
                        "[work_task] auto-merge of task {} refused: {e}",
                        task.id
                    );
                    if work_task_service::set_review_error(
                        &self.db.conn,
                        task.id,
                        task.run_seq,
                        &format!("auto-merge failed: {e}"),
                    )
                    .await
                    .unwrap_or(false)
                    {
                        self.emit_upsert(task.id);
                    }
                }
            }
            // One dispatch (or one refusal) per sweep. Cascading on after a
            // refusal would stamp one environmental problem onto every card
            // in the column.
            return;
        }
    }

    // ── merging crash recovery (git truth) ──────────────────────────────────

    /// Recover a task stuck in `merging` (crash / lost process) from git
    /// truth in the project folder. A merge generation with a live agent
    /// connection is not stuck — its TurnComplete settles it.
    pub async fn recover_merging(self: &Arc<Self>, task_id: i32) {
        if self.merging.lock().await.contains_key(&task_id) {
            return; // merge dispatch / delivery in flight in this process
        }
        let Ok(task) = work_task_service::get_model(&self.db.conn, task_id).await else {
            return;
        };
        if task.status != WorkTaskStatus::Merging {
            return;
        }
        if let Some(conn_id) = task.connection_id.as_deref() {
            if self.manager.get_state_and_emitter(conn_id).await.is_some() {
                return; // live merge generation — on_turn_complete owns the settle
            }
            // Dead: its TurnComplete is never coming, so this pass inherits the
            // teardown that settle owed. Retire it BEFORE deciding anything —
            // this very pass calls `remove_worktree_locked` when the merge
            // landed, and a leftover entry reads there as "a live agent is
            // still working in that worktree" and refuses the cleanup the user
            // asked for. Nothing below re-reads the connection.
            self.retire_connection(conn_id, task_id).await;
        }
        let Some(state) = task
            .merge_state
            .as_deref()
            .and_then(|s| serde_json::from_str::<WorkTaskMergeState>(s).ok())
        else {
            self.back_to_review(
                task_id,
                "merge state lost — please merge again".to_string(),
                None,
            )
            .await;
            return;
        };
        // A `merging` row is one of two very different things. Read the op
        // before touching anything operation-specific: a delivery never went
        // near the project folder, and its truth lives at the forge.
        if state.op == WorkTaskMergeOp::DeliverPr {
            self.recover_delivery(&task, &state).await;
            return;
        }
        let Ok(root) = get_folder_core(&self.db, task.folder_id).await else {
            return;
        };

        let lock = self.folder_lock(task.folder_id).await;
        let _guard = lock.lock().await;
        // Re-read under the lock; an in-flight settle may have finished it.
        let Ok(current) = work_task_service::get_model(&self.db.conn, task_id).await else {
            return;
        };
        if current.status != WorkTaskStatus::Merging || current.run_seq != task.run_seq {
            return;
        }

        match self.merge_landed_commit(&current, &state, &root.path).await {
            Ok(Some(commit)) => {
                let landed = work_task_service::merge_landed(&self.db.conn, task_id, &commit)
                    .await
                    .unwrap_or(false);
                if landed {
                    self.emit_upsert(task_id);
                    self.spawn_forge_writeback(task_id, WritebackOutcome::Merged(commit));
                    if state.delete_worktree {
                        self.remove_worktree_locked(task_id, None).await;
                    }
                }
            }
            Ok(None) => {
                self.clean_merge_residue(&root.path).await;
                self.back_to_review(
                    task_id,
                    "the merge was interrupted before landing — merge again".to_string(),
                    None,
                )
                .await;
            }
            Err(e) => {
                self.back_to_review(task_id, format!("could not verify the merge: {e}"), None)
                    .await;
            }
        }
    }

    // ── worktree cleanup ────────────────────────────────────────────────────
}
