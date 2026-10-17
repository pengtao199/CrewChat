use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：merge
impl TaskEngine {
    /// Land a reviewed task on its base branch. Runs the full stage 0/A/B
    /// pipeline; on any failure the task returns to review with a readable
    /// error. Optionally removes the worktree after landing.
    /// Accept a reviewed task: dispatch a MERGE GENERATION — the agent lands
    /// the task onto the base branch itself in its session, resolving any
    /// conflicts in the same turn. Validation runs before the review→merging
    /// CAS, so a refused merge leaves the task untouched; after dispatch the
    /// settle comes from git truth (`settle_merge_generation` / recovery),
    /// never from the agent's word. `auto` marks a dispatch the engine's
    /// auto-merge sweep issued rather than a click — same pipeline (including
    /// the folder's per-stage prompts), different actor on the timeline.
    ///
    /// A click that arrives while the folder's one merge slot is busy is
    /// QUEUED rather than refused: the intent is parked on the row and the
    /// merge pump dispatches it when the slot frees, so accepting a whole
    /// review column is one pass of clicks instead of a wait per landing. An
    /// unattended dispatch never queues — the sweep runs its own train.
    ///
    /// Returns as soon as the generation is LIVE — the CAS committed and the
    /// agent answered — not when the merge lands, and not when the round's
    /// prompt goes in either: a resumed session may compact first, and that
    /// turn is the run's own work (see [`Self::spawn_merge_launch`]). So the
    /// errors this reports are the ones a caller can act on — a refusal that
    /// left the task untouched, or a dispatch that could not get an agent up —
    /// while anything after that reaches the user on the card.
    ///
    /// `claim` is set only when the pump is spending a merge the user queued
    /// earlier; it binds every write here to that exact parked intent (see
    /// [`QueuedMergeClaim`]). A click passes `None` and always wins.
    pub async fn merge_task(
        self: &Arc<Self>,
        task_id: i32,
        message: Option<String>,
        delete_worktree: bool,
        instructions: Option<String>,
        auto: bool,
    ) -> Result<MergeDispatch, String> {
        self.merge_task_inner(task_id, message, delete_worktree, instructions, auto, None)
            .await
    }

    pub(super) async fn merge_task_inner(
        self: &Arc<Self>,
        task_id: i32,
        message: Option<String>,
        delete_worktree: bool,
        instructions: Option<String>,
        auto: bool,
        claim: Option<&QueuedMergeClaim>,
    ) -> Result<MergeDispatch, String> {
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| e.to_string())?;
        if task.status != WorkTaskStatus::Review {
            return Err("task is not in review".to_string());
        }
        // A pull-request task's work belongs on the pull request's own branch,
        // where its author and reviewers are looking. Landing it straight onto
        // the base here would take the pull request's changes in behind their
        // backs — under a local commit, with the pull request left open and
        // apparently unmerged. The gate is HERE rather than in the UI because
        // this path is reachable by id (an old frontend, a direct API call) and
        // because it covers queueing and the pump's later dispatch too.
        if task.source_kind.as_deref() == Some(SOURCE_KIND_PR) {
            return Err(
                "this task came from a pull request — deliver it back to that pull request \
                 instead of merging it here"
                    .to_string(),
            );
        }
        let message = message
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        // Same normalization as the message: whitespace-only is "the user typed
        // nothing", and nothing is what the prompt must then omit entirely.
        let instructions = instructions
            .map(|i| i.trim().to_string())
            .filter(|i| !i.is_empty());
        let settings = work_task_service::settings_get_effective(&self.db.conn, task.folder_id)
            .await
            .unwrap_or_default();
        let strategy = if settings.merge_strategy == "merge" {
            "merge"
        } else {
            "squash"
        }
        .to_string();
        let (root, _wt, base_branch, work_branch) = self.merge_coordinates(&task).await?;

        // Dispatch under the per-folder git lock: one merge per project at a
        // time, with the base state validated right before the CAS.
        let lock = self.folder_lock(task.folder_id).await;
        let _guard = lock.lock().await;

        let another_merging =
            work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Merging])
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .any(|t| t.folder_id == task.folder_id);
        if another_merging {
            if auto {
                // The sweep drains its column one landing at a time and
                // re-sweeps on every settle; a parked unattended intent would
                // outlive the setting that asked for it.
                return Err(
                    "another task of this project is already merging — wait for it".to_string()
                );
            }
            // Take (or keep) a place in line. Reusing the existing `queued_at`
            // is what makes re-queuing — the user reopening the dialog to
            // change the message, or the pump re-parking a merge whose slot got
            // taken first — an edit rather than a trip to the back of the queue.
            let queued_at = claim
                .map(|c| c.queued_at)
                .or_else(|| {
                    work_task_service::queued_merge(task.pending_merge.as_deref())
                        .map(|q| q.queued_at)
                })
                .unwrap_or_else(chrono::Utc::now);
            let intent = WorkTaskQueuedMerge {
                message,
                delete_worktree,
                instructions,
                queued_at,
            };
            let queued = work_task_service::queue_merge(
                &self.db.conn,
                task_id,
                &intent,
                task.run_seq,
                claim.map(|c| c.raw.as_str()),
            )
            .await
            .map_err(|e| e.to_string())?;
            if !queued {
                return Err(missed_queue_cas(claim));
            }
            self.emit_upsert(task_id);
            return Ok(MergeDispatch::Queued);
        }
        let head = resolve_git_head(&root.path).await.map_err(|e| e.to_string())?;
        if head.branch.as_deref() != Some(base_branch.as_str()) {
            // Not necessarily a branch the user wandered off: a task created
            // FOR another branch has a base the project folder may never have
            // been on. So the ask is "put it there", not "put it back".
            return Err(format!(
                "project folder is on '{}', expected '{base_branch}' — switch it to \
                 '{base_branch}' to merge",
                head.branch.as_deref().unwrap_or("detached HEAD")
            ));
        }
        match task_git::staged_clean(&root.path).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(
                    "the project folder has staged changes — commit or unstage them first"
                        .to_string(),
                )
            }
            Err(e) => return Err(e.to_string()),
        }
        let pre_merge_head = task_git::rev_parse(&root.path, "HEAD")
            .await
            .map_err(|e| e.to_string())?;

        let state = WorkTaskMergeState {
            op: WorkTaskMergeOp::Land,
            pre_merge_head,
            message: message.clone().unwrap_or_default(),
            strategy: strategy.clone(),
            delete_worktree,
            auto_message: message.is_none(),
            instructions: instructions.clone(),
            ..Default::default()
        };
        // Keep recovery away from the dispatch window (begin → live conn).
        // Losing the claim REFUSES the dispatch rather than continuing without
        // ownership: the claim and the CAS are independent, so a claim-loser
        // can still win review→merging, and the claim-holder's own CAS failure
        // would then release the only registry entry — leaving the winning
        // generation unowned for exactly the window this registry exists to
        // cover. The wording keeps `is_benign_merge_race` true, so the
        // unattended sweep skips this round instead of bannering the card.
        let Some(in_flight) = self.claim_in_flight(task_id).await else {
            return Err(
                "this task is already merging — wait for the operation in flight".to_string(),
            );
        };
        // `task.run_seq` was read before the folder lock; the CAS binds the
        // dispatch to that exact generation, so waiting out the lock behind an
        // attempt that failed (and bannered the row) misses instead of
        // redispatching — the auto path's no-retry latch depends on this.
        let began = work_task_service::begin_merge(
            &self.db.conn,
            task_id,
            &state,
            task.run_seq,
            auto,
            claim.map(|c| c.raw.as_str()),
        )
        .await;
        // The folder lock covered what it is for: validating the base state and
        // committing the CAS as one step. The row now reads `merging`, which is
        // what makes the next dispatch queue, and the launch below wants the
        // TASK lock — which `remove_worktree_locked` takes before this one.
        drop(_guard);
        let result = match began {
            Err(e) => Err(e.to_string()),
            Ok(None) => Err(match claim {
                Some(_) => missed_queue_cas(claim),
                None => "task left review before the merge began".to_string(),
            }),
            Ok(Some(_run_seq)) => {
                self.emit_upsert(task_id);
                // The claim goes with it: from here the launch owns this
                // generation, and it is the only thing that releases it.
                return self
                    .spawn_merge_launch(
                        task_id,
                        task.folder_id,
                        LaunchMode::Merge {
                            root_path: root.path.clone(),
                            base_branch: base_branch.clone(),
                            work_branch: work_branch.clone(),
                            strategy,
                            message,
                            instructions,
                        },
                        in_flight,
                    )
                    .await;
            }
        };
        // Only a dispatch that never spent the CAS gets here, so the claim is
        // still this call's to give back.
        self.release_in_flight(task_id, in_flight).await;
        self.emit_upsert(task_id);
        result
    }

    /// Run a merge generation's launch off-thread and wait only for it to
    /// become LIVE (see [`Self::launch`]'s `dispatched`).
    ///
    /// The caller is a click on the merge dialog's button, and everything past
    /// the live agent — the pre-prompt context compaction above all, which is
    /// deliberately unbounded in time — is the run's own work, reported on the
    /// card through `task://changed` like every other round. Awaiting it here
    /// is what used to hold that dialog open, spinner and all, for the whole
    /// compaction of a full context window.
    ///
    /// Inherits the in-flight claim: it must outlive the WHOLE launch (it is
    /// what keeps `recover_merging` off a generation that is still setting up),
    /// so releasing it at dispatch would hand a compacting merge to the very
    /// recovery pass this registry exists to hold back.
    ///
    /// A launch that fails before going live is reported to the caller AND
    /// settled here, so the two never disagree about a merge that never ran.
    async fn spawn_merge_launch(
        self: &Arc<Self>,
        task_id: i32,
        folder_id: i32,
        mode: LaunchMode,
        in_flight: u64,
    ) -> Result<MergeDispatch, String> {
        let (dispatched, live) = DispatchSignal::new();
        let engine = self.clone();
        tokio::spawn(async move {
            // A merge generation never transitions out of `merging` here, so
            // the sequence sink stays unused — a failure goes back to review
            // through `back_to_review` below.
            let merge_seq = LaunchSeq::default();
            let result = engine
                .launch(task_id, mode, &merge_seq, Some(&dispatched))
                .await;
            if let Err(e) = &result {
                engine
                    .back_to_review(task_id, format!("merge dispatch failed: {e}"), None)
                    .await;
            }
            // Settle first, THEN release, and only then let the caller go: a
            // dispatch that returns an error has to leave a task nothing is
            // holding — otherwise the click's next act (re-merging, say) races
            // this generation's own teardown.
            engine.release_in_flight(task_id, in_flight).await;
            // Whatever happened, the caller stops waiting: `fire` is a no-op
            // once the launch has already reported itself live, so a failure
            // AFTER the dispatch (the round's own prompt refused, say) is left
            // to the card rather than replayed into a dialog that has closed.
            dispatched.fire(result.clone());
            engine.emit_upsert(task_id);
            if result.is_err() {
                // The folder's merge slot never really got spent — let the next
                // queued landing (or the auto-merge train) have it.
                engine.spawn_merge_pump(folder_id);
            }
        });
        // A sender dropped without firing means the launch returned early
        // without dispatching (a cancel gate, a lost CAS): the row is still
        // `merging` and nobody failed it, exactly as before this ran off-thread
        // — `recover_merging` settles it from git truth on the next tick.
        live.await.unwrap_or(Ok(())).map(|()| MergeDispatch::Dispatched)
    }

    /// Settle a finished merge generation from git truth: landed ⟺ the base
    /// HEAD moved and contains the work (branch ancestry for merge commits,
    /// tree equality for squashes). Anything else goes back to review with the
    /// reason, after cleaning any half-done merge out of the project folder.
    pub(super) async fn settle_merge_generation(
        self: &Arc<Self>,
        task: &crate::db::entities::work_task::Model,
        stop_reason: &str,
        summary: Option<&str>,
    ) {
        let task_id = task.id;
        let Some(state) = task
            .merge_state
            .as_deref()
            .and_then(|s| serde_json::from_str::<WorkTaskMergeState>(s).ok())
        else {
            self.back_to_review(task_id, "merge state lost — please merge again".to_string(), None)
                .await;
            return;
        };
        let root = match get_folder_core(&self.db, task.folder_id).await {
            Ok(r) => r,
            Err(e) => {
                self.back_to_review(task_id, e.to_string(), None).await;
                return;
            }
        };
        let lock = self.folder_lock(task.folder_id).await;
        let _guard = lock.lock().await;

        match self.merge_landed_commit(task, &state, &root.path).await {
            Ok(Some(commit)) => {
                let landed = work_task_service::merge_landed(&self.db.conn, task_id, &commit)
                    .await
                    .unwrap_or(false);
                if landed {
                    self.emit_upsert(task_id);
                    // Only on the transition — `merge_landed` is a CAS, so a
                    // second settle of the same generation posts nothing.
                    self.spawn_forge_writeback(task_id, WritebackOutcome::Merged(commit));
                    if state.delete_worktree {
                        self.remove_worktree_locked(task_id, None).await;
                    }
                }
            }
            Ok(None) => {
                self.clean_merge_residue(&root.path).await;
                let reason = match stop_reason {
                    "end_turn" => match summary.map(str::trim).filter(|s| !s.is_empty()) {
                        Some(s) => format!(
                            "the agent finished without landing the merge: {}",
                            first_chars(s, 300)
                        ),
                        None => "the agent finished without landing the merge — review and \
                                 merge again"
                            .to_string(),
                    },
                    "cancelled" => "the merge run was stopped before landing".to_string(),
                    other => format!("the merge run failed before landing: {other}"),
                };
                self.back_to_review(task_id, reason, None).await;
            }
            Err(e) => {
                self.back_to_review(task_id, format!("could not verify the merge: {e}"), None)
                    .await;
            }
        }
    }

    /// `Some(base HEAD)` when git truth says this task landed on the base.
    /// The commit message is deliberately not consulted — it may have been
    /// written by the agent.
    pub(super) async fn merge_landed_commit(
        &self,
        task: &crate::db::entities::work_task::Model,
        state: &WorkTaskMergeState,
        root_path: &str,
    ) -> Result<Option<String>, String> {
        let head = task_git::rev_parse(root_path, "HEAD")
            .await
            .map_err(|e| e.to_string())?;
        if head == state.pre_merge_head {
            return Ok(None);
        }
        let Some(work_branch) = task.work_branch.as_deref() else {
            return Ok(None);
        };
        let ancestor = task_git::is_ancestor(root_path, work_branch, &head)
            .await
            .unwrap_or(false);
        let same_tree = task_git::trees_equal(root_path, &head, work_branch)
            .await
            .unwrap_or(false);
        Ok((ancestor || same_tree).then_some(head))
    }

    /// Clean a half-done landing out of the project folder: an in-progress
    /// merge (MERGE_HEAD) or a staged-but-uncommitted squash.
    pub(super) async fn clean_merge_residue(&self, root_path: &str) {
        let residue = task_git::has_merge_head(root_path).await.unwrap_or(false)
            || !task_git::staged_clean(root_path).await.unwrap_or(true);
        if residue {
            let _ = task_git::reset_merge(root_path).await;
        }
    }

    pub(super) async fn back_to_review(
        &self,
        task_id: i32,
        error: String,
        conflict_files: Option<Vec<String>>,
    ) {
        let _ = work_task_service::merge_back_to_review(
            &self.db.conn,
            task_id,
            None,
            Some(error),
            conflict_files,
        )
        .await;
        self.emit_upsert(task_id);
    }

    /// merging → review for a DELIVERY, bound to the generation the caller is
    /// acting on. Crash recovery can spend seconds at the forge before it
    /// decides, and by then the row may already belong to a delivery someone
    /// started since — `status = merging` alone would let the stale pass bounce
    /// the live one.
    pub(super) async fn bounce_delivery(
        &self,
        task: &crate::db::entities::work_task::Model,
        error: String,
    ) {
        let _ = work_task_service::merge_back_to_review(
            &self.db.conn,
            task.id,
            Some(task.run_seq),
            Some(error),
            None,
        )
        .await;
        self.emit_upsert(task.id);
    }

    /// Resolve (project folder, worktree folder, base branch, work branch) or
    /// explain what's missing.
    pub(super) async fn merge_coordinates(
        &self,
        task: &crate::db::entities::work_task::Model,
    ) -> Result<
        (
            crate::models::FolderDetail,
            crate::models::FolderDetail,
            String,
            String,
        ),
        String,
    > {
        let root = get_folder_core(&self.db, task.folder_id)
            .await
            .map_err(|e| e.to_string())?;
        let wt_id = task
            .worktree_folder_id
            .ok_or_else(|| "task has no worktree".to_string())?;
        let wt = get_folder_core(&self.db, wt_id)
            .await
            .map_err(|e| e.to_string())?;
        if !Path::new(&wt.path).exists() {
            return Err("the task worktree no longer exists on disk".to_string());
        }
        let base_branch = task
            .base_branch
            .clone()
            .ok_or_else(|| "task has no recorded base branch".to_string())?;
        let work_branch = task
            .work_branch
            .clone()
            .ok_or_else(|| "task has no recorded work branch".to_string())?;
        Ok((root, wt, base_branch, work_branch))
    }

    // ── in-flight ownership (merge / delivery) ──────────────────────────────

    /// Claim in-flight ownership of a task, so the reconcile sweep leaves its
    /// `merging` row alone. `None` when another operation in this process
    /// already holds it.
    pub(super) async fn claim_in_flight(&self, task_id: i32) -> Option<u64> {
        let token = self
            .in_flight_token
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        match self.merging.lock().await.entry(task_id) {
            std::collections::hash_map::Entry::Occupied(_) => None,
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(token);
                Some(token)
            }
        }
    }

    /// Release a claim — and ONLY our own. Comparing the token is what keeps a
    /// losing attempt from freeing the winner's task.
    pub(super) async fn release_in_flight(&self, task_id: i32, token: u64) {
        let mut held = self.merging.lock().await;
        if held.get(&task_id) == Some(&token) {
            held.remove(&task_id);
        }
    }

    // ── deliver to the forge (push + pull request) ──────────────────────────
}
