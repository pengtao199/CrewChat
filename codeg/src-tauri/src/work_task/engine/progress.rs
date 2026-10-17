use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：progress
impl TaskEngine {
    pub(super) async fn on_turn_complete(self: &Arc<Self>, conn_id: &str, stop_reason: &str) {
        let entry = { self.index.lock().await.get(conn_id).copied() };
        let Some((task_id, run_seq)) = entry else {
            return; // not a task run
        };

        let summary = self.capture_summary(conn_id).await;
        self.retire_connection(conn_id, task_id).await;
        let _ = self.manager.disconnect(conn_id).await;

        let task = work_task_service::get_model(&self.db.conn, task_id).await.ok();

        // A merge generation settles from git truth whatever the stop reason —
        // the agent may have landed the merge and then errored or been stopped.
        if let Some(t) = task
            .as_ref()
            .filter(|t| t.status == WorkTaskStatus::Merging && t.run_seq == run_seq)
        {
            self.settle_merge_generation(t, stop_reason, summary.as_deref())
                .await;
            self.pump_folder(t.folder_id).await;
            // The folder's one merge slot just freed — the next merge the user
            // queued (or the auto-merge train) can land now.
            self.spawn_merge_pump(t.folder_id);
            return;
        }

        let changed = match stop_reason {
            "end_turn" => {
                // A `task_complete` report from this generation decides the
                // settle: blocked → failed(verdict_blocked); success /
                // needs_review → review. The verdict column is cleared on every
                // claim, so a present verdict is always this generation's — and
                // its summary (written with it) outranks the captured
                // last-assistant text.
                let verdict = task
                    .as_ref()
                    .filter(|t| t.run_seq == run_seq)
                    .and_then(|t| t.verdict.clone());
                if verdict.as_deref() == Some("blocked") {
                    let error = task
                        .as_ref()
                        .and_then(|t| t.result_summary.clone())
                        .unwrap_or_else(|| "agent reported the task as blocked".to_string());
                    work_task_service::fail(
                        &self.db.conn,
                        task_id,
                        &[WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput],
                        Some(run_seq),
                        "verdict_blocked",
                        Some(error),
                    )
                    .await
                    .unwrap_or(false)
                } else {
                    let own_summary = if verdict.is_some() {
                        task.as_ref().and_then(|t| t.result_summary.clone())
                    } else {
                        None
                    };
                    let stats = self.snapshot_diff_stats(task_id).await;
                    let settled = work_task_service::settle_review(
                        &self.db.conn,
                        task_id,
                        run_seq,
                        own_summary.or(summary),
                        stats,
                    )
                    .await
                    .unwrap_or(false);
                    if settled {
                        self.spawn_post_review(task_id, run_seq);
                    }
                    settled
                }
            }
            "cancelled" => {
                // A cancelled task generation is a cancellation, not an agent
                // failure. Keep it generation-scoped: a delayed event must not
                // cancel a task that already reached review or was requeued.
                work_task_service::cancel_running_generation(&self.db.conn, task_id, run_seq)
                    .await
                    .unwrap_or(false)
            }
            other => work_task_service::fail(
                &self.db.conn,
                task_id,
                &[WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput],
                Some(run_seq),
                "agent_error",
                Some(format!("agent stopped: {other}")),
            )
            .await
            .unwrap_or(false),
        };
        if changed {
            self.emit_upsert(task_id);
        }
        // A slot freed up — keep the queue draining.
        if let Ok(task) = work_task_service::get_model(&self.db.conn, task_id).await {
            self.pump_folder(task.folder_id).await;
        }
    }

    /// Track an outstanding blocking request and flip running ⇄ awaiting_input
    /// on the empty↔non-empty edges of the per-task set.
    ///
    /// `conn_id` may be the task's own connection or one of its delegation
    /// children: a sub-agent parked on a permission blocks the run just as
    /// surely as the top-level agent does, and is in fact harder to notice
    /// (#447). A child's keys are prefixed with its connection id so
    /// [`Self::forget_delegation_child`] can retract them as a group.
    pub(super) async fn track_request(&self, conn_id: &str, key: String, outstanding: bool) {
        let Some(((task_id, run_seq), is_own_connection)) =
            self.task_for_connection(conn_id).await
        else {
            return;
        };
        let key = if is_own_connection {
            key
        } else {
            format!("{conn_id}#{key}")
        };
        let flip = {
            let mut awaiting = self.awaiting.lock().await;
            let set = awaiting.entry(task_id).or_default();
            if outstanding {
                set.insert(key);
                set.len() == 1
            } else {
                set.remove(&key);
                if set.is_empty() {
                    awaiting.remove(&task_id);
                    true
                } else {
                    false
                }
            }
        };
        if flip {
            let flipped =
                work_task_service::flip_awaiting(&self.db.conn, task_id, run_seq, outstanding)
                    .await
                    .unwrap_or(false);
            if flipped {
                self.emit_upsert(task_id);
            }
        }
    }

    // ── codeg-mcp task reporting tools ──────────────────────────────────────

    /// `task_progress`: attribute the report through the connection index and
    /// append an `agent_progress` event (the card/timeline milestone).
    pub async fn record_progress(&self, conn_id: &str, message: &str) -> TaskReportAck {
        let entry = { self.index.lock().await.get(conn_id).copied() };
        let Some((task_id, run_seq)) = entry else {
            return TaskReportAck::rejected("this session is not executing a work task");
        };
        // Generation guard: a stale connection's report is a no-op.
        match work_task_service::get_model(&self.db.conn, task_id).await {
            Ok(task) if task.run_seq == run_seq => {}
            _ => return TaskReportAck::rejected("the task moved on to a new run"),
        }
        if let Err(e) = work_task_service::record_event(
            &self.db.conn,
            task_id,
            "agent_progress",
            "agent",
            // Stamped with the generation that reported it: the card's progress
            // line is a claim about what the run is doing NOW, and a merge (or
            // a retry, or a follow-up) that inherited the previous round's last
            // milestone was narrating work it is not doing.
            Some(serde_json::json!({ "message": message, "run_seq": run_seq })),
        )
        .await
        {
            return TaskReportAck::rejected(&format!("could not record progress: {e}"));
        }
        self.emit_upsert(task_id);
        TaskReportAck::recorded()
    }

    /// `task_complete`: stash the verdict + summary on the current generation;
    /// the TurnComplete settle reads them to decide review vs failed.
    pub async fn record_complete(
        &self,
        conn_id: &str,
        verdict: &str,
        summary: Option<&str>,
    ) -> TaskReportAck {
        let entry = { self.index.lock().await.get(conn_id).copied() };
        let Some((task_id, run_seq)) = entry else {
            return TaskReportAck::rejected("this session is not executing a work task");
        };
        match work_task_service::set_verdict(&self.db.conn, task_id, run_seq, verdict, summary)
            .await
        {
            Ok(true) => {
                self.emit_upsert(task_id);
                TaskReportAck::recorded()
            }
            Ok(false) => TaskReportAck::rejected("the task is not running anymore"),
            Err(e) => TaskReportAck::rejected(&format!("could not record verdict: {e}")),
        }
    }

    // ── preflight (acceptance red/green light) ──────────────────────────────

    /// After a settle into review: run the folder's preflight command (if one
    /// is configured), then give auto-merge its chance. One spawned task, in
    /// that order — the sweep requires a green light whenever a preflight is
    /// configured, so the light must be on the row before the sweep reads it.
    /// Fire-and-forget: the light is written CAS (review + run_seq), so a task
    /// that moved on ignores a slow finish, and the sweep re-checks status.
    pub(super) fn spawn_post_review(self: &Arc<Self>, task_id: i32, run_seq: i32) {
        let engine = self.clone();
        tokio::spawn(async move {
            engine.run_preflight(task_id, run_seq).await;
            if let Ok(task) = work_task_service::get_model(&engine.db.conn, task_id).await {
                engine.merge_pump(task.folder_id).await;
            }
        });
    }

    async fn run_preflight(&self, task_id: i32, run_seq: i32) {
        let Ok(task) = work_task_service::get_model(&self.db.conn, task_id).await else {
            return;
        };
        let settings = work_task_service::settings_get_effective(&self.db.conn, task.folder_id)
            .await
            .unwrap_or_default();
        // A free-form command wins over a folder-command reference; the
        // reference must still exist and belong to this project folder.
        let custom = settings
            .preflight_command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        let (display_name, command_line) = match custom {
            Some(cmd) => (cmd.to_string(), cmd.to_string()),
            None => {
                let Some(command_id) = settings.preflight_command_id else {
                    return;
                };
                let Ok(Some(command)) = folder_command::Entity::find_by_id(command_id)
                    .one(&self.db.conn)
                    .await
                else {
                    return;
                };
                if command.folder_id != task.folder_id {
                    return;
                }
                (command.name, command.command)
            }
        };
        let Some(wt_id) = task.worktree_folder_id else {
            return;
        };
        let Ok(wt) = get_folder_core(&self.db, wt_id).await else {
            return;
        };
        if !Path::new(&wt.path).exists() {
            return;
        }

        let mut light = WorkTaskPreflight {
            status: "running".to_string(),
            command: display_name,
            exit_code: None,
            output_tail: None,
        };
        match work_task_service::set_preflight(&self.db.conn, task_id, run_seq, &light).await {
            Ok(true) => self.emit_upsert(task_id),
            _ => return, // task already moved on
        }

        match run_shell_capture(&command_line, &wt.path).await {
            Ok((code, tail)) => {
                let passed = code == Some(0);
                light.status = if passed { "passed" } else { "failed" }.to_string();
                light.exit_code = code;
                // The tail only matters when the light is red.
                light.output_tail = (!passed && !tail.is_empty()).then_some(tail);
            }
            Err(e) => {
                light.status = "failed".to_string();
                light.output_tail = Some(format!("could not run the command: {e}"));
            }
        }
        if work_task_service::set_preflight(&self.db.conn, task_id, run_seq, &light)
            .await
            .unwrap_or(false)
        {
            self.emit_upsert(task_id);
        }
    }

    /// Best-effort diff-stat snapshot of what THIS TASK produced — measured
    /// from [`own_work_anchor`], not from the review baseline (see that
    /// function for why the two differ on a pull-request task).
    pub(super) async fn snapshot_diff_stats(&self, task_id: i32) -> Option<(i32, i32, i32)> {
        let task = work_task_service::get_model(&self.db.conn, task_id).await.ok()?;
        let wt_id = task.worktree_folder_id?;
        let wt = get_folder_core(&self.db, wt_id).await.ok()?;
        let anchor = own_work_anchor(&wt.path, &task).await?;
        diff_stats_from(&wt.path, &anchor).await
    }

    /// Boot pass: re-measure the counters of every task sitting in review.
    ///
    /// They were snapshotted when the run settled and nothing rewrites them
    /// afterwards, so a row can outlive the state it describes — a worktree the
    /// user committed to (or reverted) in the meantime, and rows written before
    /// [`own_work_anchor`] existed, which credited a pull-request task with the
    /// pull request's own change set and cost it the "complete" button. The
    /// counters decide which acceptance the board offers, so the correction has
    /// to reach the row itself; recomputing per card is not an option (the list
    /// endpoint would run one `git diff` per task, per refresh).
    ///
    /// Best-effort throughout: an unreadable worktree keeps whatever the row
    /// already says, which is exactly the pre-existing behaviour.
    pub(super) async fn refresh_review_diff_stats(self: &Arc<Self>) {
        let Ok(rows) = work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Review])
            .await
        else {
            return;
        };
        for task in rows {
            let Some(wt) = self.live_worktree(&task).await else {
                continue;
            };
            let Some(anchor) = own_work_anchor(&wt.path, &task).await else {
                continue;
            };
            let Some(stats) = diff_stats_from(&wt.path, &anchor).await else {
                continue;
            };
            if (task.files_changed, task.additions, task.deletions)
                == (Some(stats.0), Some(stats.1), Some(stats.2))
            {
                continue;
            }
            match work_task_service::refresh_diff_stats(
                &self.db.conn,
                task.id,
                task.run_seq,
                stats,
            )
            .await
            {
                Ok(true) => {
                    tracing::info!(
                        "[work_task] task {}: diff stat corrected to {} file(s), +{}/-{}",
                        task.id,
                        stats.0,
                        stats.1,
                        stats.2
                    );
                    self.emit_upsert(task.id);
                }
                Ok(false) => {} // left review meanwhile — its own settle owns the row
                Err(e) => tracing::warn!(
                    "[work_task] task {}: could not refresh the diff stat: {e}",
                    task.id
                ),
            }
        }
    }

    /// Best-effort: the turn's final assistant text (cleared at next turn
    /// start) becomes the task's result summary.
    async fn capture_summary(&self, conn_id: &str) -> Option<String> {
        let (state, _) = self.manager.get_state_and_emitter(conn_id).await?;
        let text = state.read().await.last_assistant_text.clone();
        text.filter(|t| !t.trim().is_empty())
    }

    // ── accept without merging ─────────────────────────────────────────────

    /// Accept a reviewed task without dispatching a merge generation: review →
    /// done, optionally taking the worktree with it.
    ///
    /// Two ways in, and the board offers exactly one of them per task:
    /// - the recorded diff stat is empty — nothing to land. The stat is a
    ///   snapshot from when the run settled, so git (not the row) re-decides
    ///   under the lock;
    /// - the worktree is GONE (folder row removed, or its directory deleted
    ///   from disk) — a merge generation cannot run at all, so completing is
    ///   the only acceptance left. The leftovers are converged like any other
    ///   removal, except the work branch survives whenever it still holds
    ///   commits the base never received.
    ///
    /// The whole decision runs inside the folder's git lock, with the CAS in
    /// the middle: check, settle and remove form one critical section, leaving
    /// no window in which the task can gain a commit between the check that
    /// cleared it and the `branch -D` that would take it away.
    ///
    /// Two probes on the live-worktree path, because neither alone sees
    /// everything the removal destroys. The anchor diff
    /// ([`Self::has_landable_changes`]) covers commits made on the branch after
    /// the run settled — invisible to `git status` — as well as uncommitted
    /// files, untracked ones included. What it cannot see is a worktree that
    /// differs from its own HEAD while matching the anchor: work committed and
    /// then edited back out is a `git status` finding and an empty diff.
    /// Anything either one finds keeps the worktree on disk.
    pub async fn complete_task(
        self: &Arc<Self>,
        task_id: i32,
        delete_worktree: bool,
    ) -> Result<(), String> {
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| e.to_string())?;
        if task.status != WorkTaskStatus::Review {
            return Err("task is not in review".to_string());
        }

        // Held across the check → CAS → removal. Uncontended in the common
        // case: the merge path holds this only for its dispatch, not for the
        // generation that follows.
        let lock = self.folder_lock(task.folder_id).await;
        let _guard = lock.lock().await;

        let live_wt = self.live_worktree(&task).await;
        // Which of the two acceptances this is. The second one's branch may
        // still hold commits nobody landed, so the comment must not say
        // "nothing to land" about it.
        let nothing_to_land = live_wt.is_some();
        let reason = match &live_wt {
            Some(wt) => {
                if self.has_landable_changes(&task, &wt.path).await? {
                    return Err(
                        "this task changed files after all — merge it instead of completing it"
                            .to_string(),
                    );
                }
                "completed without merging: no changes"
            }
            None => "completed without merging: the worktree is gone",
        };
        if !work_task_service::complete_without_merge(&self.db.conn, task_id, reason)
            .await
            .map_err(|e| e.to_string())?
        {
            return Err("task left review before it could be completed".to_string());
        }
        self.emit_upsert(task_id);
        // The third settlement gets a comment too: the setting promises one
        // whenever a forge task finishes, and a task that landed nothing is an
        // outcome the thread's readers want as much as the other two — told as
        // what did not happen to their item, never as "accepted" (see
        // `writeback_comment_body`). The CAS above is what makes it one comment
        // and not one per attempt.
        self.spawn_forge_writeback(task_id, WritebackOutcome::Accepted { nothing_to_land });

        if live_wt.is_none() {
            // Nothing usable is left behind the worktree pointer — converge
            // the bookkeeping regardless of the checkbox (there is no worktree
            // left to keep), sparing only a branch that still holds work.
            self.converge_missing_worktree(&task).await;
            self.emit_upsert(task_id);
        } else if delete_worktree {
            if self.worktree_holds_uncommitted(&task).await {
                let _ = work_task_service::set_cleanup_state(
                    &self.db.conn,
                    task_id,
                    true,
                    Some(worktree_kept(RETRY_THE_CLEANUP)),
                )
                .await;
                self.emit_upsert(task_id);
            } else {
                // Broadcast omitted: the removal announces its own outcome.
                self.remove_worktree_locked(task_id, None).await;
            }
        }
        Ok(())
    }

    /// The task's recorded worktree while it can still serve a merge: folder
    /// row live and directory on disk. `None` covers "never recorded", "row
    /// removed" and "directory gone" alike.
    pub(super) async fn live_worktree(
        &self,
        task: &crate::db::entities::work_task::Model,
    ) -> Option<crate::models::FolderDetail> {
        let wt_id = task.worktree_folder_id?;
        let detail = get_folder_core(&self.db, wt_id).await.ok()?;
        Path::new(&detail.path).exists().then_some(detail)
    }

    /// Converge the leftovers of a worktree that is no longer usable (folder
    /// row removed, or directory gone from disk): prune the stale git
    /// registration, soft-delete the folder row, re-parent its conversations —
    /// the same sequence every other removal runs — EXCEPT that the work
    /// branch survives whenever it still holds commits the base never
    /// received. This path is reached without the user ever confirming a
    /// deletion of work, and `branch -D` is its only irreversible step; a kept
    /// branch stays visible in the branch selector for a manual merge or
    /// delete. Caller holds the folder git lock.
    async fn converge_missing_worktree(&self, task: &crate::db::entities::work_task::Model) {
        let task_id = task.id;
        let Some(wt_id) = task.worktree_folder_id else {
            return; // already detached — nothing to converge
        };
        let root = get_folder_core(&self.db, task.folder_id).await.ok();
        let wt = get_folder_core(&self.db, wt_id).await.ok();
        let (Some(root), Some(wt)) = (root, wt) else {
            // The folder rows are already gone — detach, and tell clients
            // still holding a stale copy to drop it.
            let _ = work_task_service::clear_worktree(&self.db.conn, task_id).await;
            emit_folder_deleted(&self.emitter, wt_id);
            return;
        };
        let mut branch_to_delete = task.work_branch.as_deref();
        if let Some(branch) = branch_to_delete {
            if task_git::branch_holds_unlanded_work(
                &root.path,
                branch,
                task.base_branch.as_deref(),
                task.base_sha.as_deref(),
            )
            .await
            {
                tracing::info!(
                    "[work_task] task {task_id}: keeping branch {branch} — it still holds \
                     unlanded commits"
                );
                let _ = work_task_service::record_event(
                    &self.db.conn,
                    task_id,
                    "user_action",
                    "engine",
                    Some(serde_json::json!({ "action": "branch_kept", "branch": branch })),
                )
                .await;
                branch_to_delete = None;
            }
        }
        if let Err(e) =
            task_git::remove_worktree_and_branch(&root.path, &wt.path, branch_to_delete, None).await
        {
            let _ = work_task_service::set_cleanup_state(
                &self.db.conn,
                task_id,
                true,
                Some(e.to_string()),
            )
            .await;
            return;
        }
        converge_worktree_removal(&self.db, &self.emitter, task_id, wt_id, task.folder_id, &wt.path)
            .await;
    }

    /// Whether the task worktree still has anything uncommitted (tracked edits
    /// or untracked files). No worktree recorded and no folder row both read as
    /// "nothing to lose" — there is no checkout for a removal to destroy.
    pub(super) async fn worktree_holds_uncommitted(
        &self,
        task: &crate::db::entities::work_task::Model,
    ) -> bool {
        let Some(wt_id) = task.worktree_folder_id else {
            return false;
        };
        let Ok(wt) = get_folder_core(&self.db, wt_id).await else {
            return false;
        };
        path_holds_uncommitted(&wt.path).await
    }

    /// Live git truth for "would an acceptance still have something to take
    /// from this task?" — work committed on the branch after the run settled
    /// (which `git status` reports as a clean worktree) as well as work the
    /// agent never committed, new files included. Measured from
    /// [`own_work_anchor`], the same place the recorded diff stat is taken
    /// from, so the board's offer and this check cannot disagree. `wt_path` is
    /// the already-verified live worktree (see [`Self::live_worktree`]).
    pub(super) async fn has_landable_changes(
        &self,
        task: &crate::db::entities::work_task::Model,
        wt_path: &str,
    ) -> Result<bool, String> {
        let Some(anchor) = own_work_anchor(wt_path, task).await else {
            return Ok(false);
        };
        let files = task_git::diff_numstat_with_untracked(wt_path, &anchor)
            .await
            .map_err(|e| format!("could not read the task's changes: {e}"))?;
        Ok(!files.is_empty())
    }

    // ── merge (two-stage, persisted intent, per-folder git mutex) ───────────
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Cap on a task-derived conversation title. Long enough for a real card name,
/// short enough that the sidebar row is not one giant ellipsis.
pub(super) const CONVERSATION_TITLE_MAX_CHARS: usize = 80;

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
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
