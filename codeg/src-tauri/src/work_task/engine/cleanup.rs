use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：cleanup
impl TaskEngine {
    /// Remove a task's worktree + branch (user action from the card, the
    /// post-merge checkbox, or the cancel dialog's). Takes the task lock and
    /// then the per-folder git lock, in that order.
    ///
    /// Refuses — with the reason both returned and flagged on the row — while
    /// the worktree still holds uncommitted files. See the check itself for why
    /// that line is drawn here and not at the confirmation.
    ///
    /// The TASK lock is what makes this safe against a launch, and the folder
    /// lock cannot stand in for it: [`Self::launch`] holds the task lock across
    /// its whole setup — `ensure_worktree` included — and takes no folder lock
    /// at all, so a folder-scoped removal runs straight through a checkout that
    /// is being created. Reachable whenever the row leaves a launchable state
    /// and comes back: cancel (`canceled`) → the user requeues (`todo`, a pure
    /// DB write that waits on nothing) → the pump claims and launches, all
    /// while this call sits between its status read and its removal. Nothing
    /// else about that sequence is wrong — the fresh run mints its own worktree
    /// — but deleting the directory out from under `ensure_worktree` is.
    ///
    /// Hence read the status twice: once before the lock so a `running` task
    /// fails fast instead of queueing behind its own init command (which can be
    /// a whole `pnpm install`), and once after, because only the second read is
    /// serialized against the launch it is trying to exclude.
    ///
    /// Ordering is deadlock-free by inspection: the task lock is only ever
    /// taken here, in [`Self::cancel`], and in [`Self::launch`], and none of
    /// those three is reachable while a folder lock is held (`launch` runs in
    /// its own spawned task, and `cancel`'s only nested wait is the pump lock).
    pub async fn cleanup_task(&self, task_id: i32) -> Result<(), CleanupBlocked> {
        /// Statuses whose worktree is either in use or about to be — the launch
        /// path owns it, not the user.
        fn in_use(status: &WorkTaskStatus) -> bool {
            matches!(
                status,
                WorkTaskStatus::Queued
                    | WorkTaskStatus::Preparing
                    | WorkTaskStatus::Running
                    | WorkTaskStatus::AwaitingInput
                    | WorkTaskStatus::Merging
            )
        }
        const REFUSAL: &str = "cancel or finish the task before removing its worktree";

        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| CleanupBlocked::failed(e.to_string()))?;
        if in_use(&task.status) {
            return Err(CleanupBlocked::failed(REFUSAL));
        }
        let task_guard = self.task_lock(task_id).await;
        let _task_guard = task_guard.lock().await;
        // Re-read under the lock: whatever claimed the row while we waited is
        // now either fully set up (and refused here) or still blocked on this
        // lock at the top of `launch` — where it re-reads its own status and
        // gives up if a cancel moved the row on.
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| CleanupBlocked::failed(e.to_string()))?;
        if in_use(&task.status) {
            return Err(CleanupBlocked::failed(REFUSAL));
        }
        let lock = self.folder_lock(task.folder_id).await;
        let _guard = lock.lock().await;
        // Uncommitted work is the one thing behind this call that git cannot
        // give back, and nothing on the way in ever named it: the cancel
        // dialog's checkbox promises "its worktree and work branch", the card's
        // button says "Delete worktree", and neither mentions files the agent
        // left unstaged. The removal underneath is `worktree remove --force`,
        // so without this check a stop pressed mid-edit — the very moment a
        // checkout is most likely to be dirty — takes the edits with it.
        //
        // The other two surfaces that remove a worktree already draw this line:
        // `complete_task` refuses with this same sentence, and the branch
        // dropdown's removal re-asks under an explicit "those changes cannot be
        // recovered" confirmation. This one just took it. Committed work is a
        // different matter and stays deletable — `branch -D` is what the
        // checkbox spells out, and the reflog still holds those commits.
        //
        // Recorded AND returned, because the callers hear different channels:
        // the direct button surfaces the `Err` as a toast, `work_task_cancel_core`
        // swallows it and reads `cleanup_state` off the card instead, and the
        // delete path — which would otherwise tombstone that card and the reason
        // with it — stops on `holds_work` rather than swallowing.
        if self.worktree_holds_uncommitted(&task).await {
            let _ = work_task_service::set_cleanup_state(
                &self.db.conn,
                task_id,
                true,
                Some(worktree_kept(RETRY_THE_CLEANUP)),
            )
            .await;
            self.emit_upsert(task_id);
            return Err(CleanupBlocked {
                message: worktree_kept(RETRY_THE_CLEANUP),
                holds_work: true,
            });
        }
        // No `emit_upsert` here: the removal owns that broadcast now, and only
        // it can tell a pass that changed the row from one that found nothing
        // to do.
        self.remove_worktree_locked(task_id, None).await;
        Ok(())
    }

    /// Git-first-then-DB worktree removal, and the `task://changed` that tells
    /// clients about it. Caller holds the folder git lock.
    ///
    /// The broadcast lives HERE rather than at the call sites: this is the only
    /// thing that knows whether the row moved, and the acceptance paths reach it
    /// AFTER their own `emit_upsert` (`settle_merge_generation`,
    /// `recover_merging`) — so a caller-side convention leaves the card holding
    /// a snapshot taken before the worktree was detached, and its "worktree
    /// removed" badge (`worktreeWasRemoved`, keyed on `worktree_folder_id`)
    /// never appears until a full refetch. `converge_worktree_removal` owns its
    /// folder / conversation / tab broadcasts for the same reason.
    ///
    /// `expected_tip` is forwarded to [`task_git::remove_worktree_and_branch`]:
    /// `Some(oid)` deletes the work branch only while it still points there.
    /// The local-merge paths pass `None` — they settle from git truth about the
    /// base, not from an OID they published.
    pub(super) async fn remove_worktree_locked(&self, task_id: i32, expected_tip: Option<&str>) {
        if self.remove_worktree_inner(task_id, expected_tip).await {
            self.emit_upsert(task_id);
        }
    }

    /// The removal itself. Returns whether it wrote to the task row — the
    /// broadcast in [`Self::remove_worktree_locked`] rides that answer, so a
    /// pass that decided there was nothing to do stays silent.
    ///
    /// Order matters: the git removal runs first; only after it succeeds does
    /// the DB transaction re-parent the worktree's conversations onto the
    /// project folder (stamping `origin_cwd`), close its tabs, and soft-delete
    /// the folder row. A git failure flags `cleanup_state='failed'` (retryable
    /// from the card) and leaves the DB untouched; a `done` task never leaves
    /// `done` either way — including a branch that moved out from under an
    /// `expected_tip`, which lands here as exactly that kind of git failure.
    async fn remove_worktree_inner(&self, task_id: i32, expected_tip: Option<&str>) -> bool {
        let Ok(task) = work_task_service::get_model(&self.db.conn, task_id).await else {
            return false;
        };
        let Some(wt_id) = task.worktree_folder_id else {
            // Nothing left to remove. A cleanup flag surviving past the
            // detach would offer a retry that can never succeed — clear it.
            if task.cleanup_state.is_some() {
                let _ =
                    work_task_service::set_cleanup_state(&self.db.conn, task_id, false, None).await;
                return true;
            }
            return false;
        };
        // Precondition: no live connection of ours on this task. `index` alone
        // cannot answer that — it is a correlation table, not a liveness one,
        // and an entry whose connection died stays put until something retires
        // it. Believing such an entry costs the user a worktree they asked to
        // remove and a `failed` flag whose retry can never clear (nothing will
        // ever arrive to remove the entry). Ask the manager, and retire what it
        // says is gone.
        let mut has_conn = false;
        let claimed: Vec<String> = {
            self.index
                .lock()
                .await
                .iter()
                .filter(|(_, (tid, _))| *tid == task_id)
                .map(|(conn_id, _)| conn_id.clone())
                .collect()
        };
        for conn_id in claimed {
            if self.manager.get_state_and_emitter(&conn_id).await.is_some() {
                has_conn = true;
            } else {
                self.retire_connection(&conn_id, task_id).await;
            }
        }
        if has_conn {
            let _ = work_task_service::set_cleanup_state(
                &self.db.conn,
                task_id,
                true,
                Some("task still has a live agent connection".to_string()),
            )
            .await;
            return true;
        }

        let root = match get_folder_core(&self.db, task.folder_id).await {
            Ok(r) => r,
            Err(e) => {
                let _ = work_task_service::set_cleanup_state(
                    &self.db.conn,
                    task_id,
                    true,
                    Some(e.to_string()),
                )
                .await;
                return true;
            }
        };
        let Ok(wt) = get_folder_core(&self.db, wt_id).await else {
            // Folder row already gone (a retried cleanup) — just detach, and
            // still tell clients to drop it: one holding a stale copy would
            // otherwise keep rendering a worktree no refetch would return.
            let _ = work_task_service::clear_worktree(&self.db.conn, task_id).await;
            emit_folder_deleted(&self.emitter, wt_id);
            return true;
        };

        if let Err(e) = task_git::remove_worktree_and_branch(
            &root.path,
            &wt.path,
            task.work_branch.as_deref(),
            expected_tip,
        )
        .await
        {
            let _ = work_task_service::set_cleanup_state(
                &self.db.conn,
                task_id,
                true,
                Some(e.to_string()),
            )
            .await;
            return true;
        }

        converge_worktree_removal(
            &self.db,
            &self.emitter,
            task_id,
            wt_id,
            task.folder_id,
            &wt.path,
        )
        .await;
        true
    }

    // ── reconcile ───────────────────────────────────────────────────────────

    pub(super) async fn reconcile_once(self: &Arc<Self>) {
        // running / awaiting_input whose worker died without a TurnComplete.
        let active = work_task_service::list_by_status(
            &self.db.conn,
            &[WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput],
        )
        .await
        .unwrap_or_default();
        for task in active {
            let Some(conn_id) = task.connection_id.clone() else {
                continue;
            };
            if self.manager.get_state_and_emitter(&conn_id).await.is_some() {
                continue; // live — on_event settles it authoritatively
            }
            // Connection gone. If the produced conversation reached a terminal
            // status the TurnComplete was merely dropped — settle from it.
            self.retire_connection(&conn_id, task.id).await;
            let conv_status = match task.conversation_id {
                Some(cid) => self.conversation_status(cid).await,
                None => None,
            };
            let changed = match conv_status {
                Some(ConversationStatus::PendingReview) | Some(ConversationStatus::Completed) => {
                    let stats = self.snapshot_diff_stats(task.id).await;
                    let settled = work_task_service::settle_review(
                        &self.db.conn,
                        task.id,
                        task.run_seq,
                        None,
                        stats,
                    )
                    .await
                    .unwrap_or(false);
                    if settled {
                        // The dropped TurnComplete owed review its preflight
                        // and its auto-merge chance — same hook as the live
                        // settle path.
                        self.spawn_post_review(task.id, task.run_seq);
                    }
                    settled
                }
                Some(ConversationStatus::Cancelled) => {
                    work_task_service::cancel_running_generation(
                        &self.db.conn,
                        task.id,
                        task.run_seq,
                    )
                    .await
                    .unwrap_or(false)
                }
                _ => work_task_service::fail(
                    &self.db.conn,
                    task.id,
                    &[WorkTaskStatus::Running, WorkTaskStatus::AwaitingInput],
                    Some(task.run_seq),
                    "interrupted",
                    Some("task lost its worker".to_string()),
                )
                .await
                .unwrap_or(false),
            };
            if changed {
                self.emit_upsert(task.id);
            }
        }

        // preparing rows that no in-flight launch owns → back to the queue.
        // `launching` is authoritative here: this process holds the exclusive
        // engine lock, and the entry outlives the whole launch (including its
        // failure handling). Without this sweep an orphan would sit in
        // `preparing` forever — `next_queued` only picks `queued`, so it would
        // lose the self-healing a stuck `queued` row gets today.
        let preparing =
            work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Preparing])
                .await
                .unwrap_or_default();
        if !preparing.is_empty() {
            let launching: HashSet<i32> = self.launching.lock().await.keys().copied().collect();
            for task in preparing {
                if launching.contains(&task.id) {
                    continue;
                }
                match work_task_service::abandon_setup(&self.db.conn, task.id, task.run_seq).await {
                    Ok(true) => {
                        tracing::info!(
                            "[work_task] requeued orphaned setup of task {}",
                            task.id
                        );
                        self.emit_upsert(task.id);
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!("[work_task] abandon setup error: {e}"),
                }
            }
        }

        // merging not owned by this process's in-flight merges → git truth.
        // Spawned off-thread: recovery waits on the per-folder git lock, and an
        // in-flight merge on that folder must not stall the event loop here.
        let merging = work_task_service::list_by_status(&self.db.conn, &[WorkTaskStatus::Merging])
            .await
            .unwrap_or_default();
        for task in merging {
            let engine = self.clone();
            tokio::spawn(async move {
                engine.recover_merging(task.id).await;
                engine.pump_folder(task.folder_id).await;
                // Recovery freed the folder's merge slot (landed or bounced) —
                // resume the queue / auto-merge train where the crash cut it.
                engine.merge_pump(task.folder_id).await;
            });
        }

        // Pending backlog: queued tasks whose slot freed while no event fired,
        // plus todo tasks of auto_process folders (the pump checks the flag).
        for folder_id in work_task_service::folders_with_pending(&self.db.conn)
            .await
            .unwrap_or_default()
        {
            self.pump_folder(folder_id).await;
        }

        // Review backlog: merges the user queued whose pump never ran (queued
        // before a restart, or a settle lost in the crash window), plus the
        // auto-merge folders' own backlog (a dispatch lost between the settle
        // and the merge, and tasks already sitting in review when the setting
        // was switched on). The pump re-checks per folder, so this is a cheap
        // scan for folders with neither.
        self.sweep_merge_backlog(None).await;
    }

    // ── helpers ─────────────────────────────────────────────────────────────

    pub(super) async fn task_lock(&self, task_id: i32) -> Arc<Mutex<()>> {
        let mut locks = self.task_locks.lock().await;
        locks
            .entry(task_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(super) async fn folder_lock(&self, folder_id: i32) -> Arc<Mutex<()>> {
        let mut locks = self.folder_locks.lock().await;
        locks
            .entry(folder_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(super) fn emit_upsert(&self, task_id: i32) {
        emit_event(
            &self.emitter,
            WORK_TASK_CHANGED_EVENT,
            WorkTaskChange::Upsert { id: task_id },
        );
    }

    pub(super) fn emit_changed_all(&self) {
        emit_event(
            &self.emitter,
            WORK_TASK_CHANGED_EVENT,
            WorkTaskChange::Refresh,
        );
    }

    pub(super) async fn conversation_status(&self, conv_id: i32) -> Option<ConversationStatus> {
        conversation::Entity::find_by_id(conv_id)
            .one(&self.db.conn)
            .await
            .ok()
            .flatten()
            .map(|m| m.status)
    }

    pub(super) async fn cancel_conversation(&self, conversation_id: i32) {
        if let Ok(Some(row)) = conversation::Entity::find_by_id(conversation_id)
            .one(&self.db.conn)
            .await
        {
            let mut active = row.into_active_model();
            active.status = Set(ConversationStatus::Cancelled);
            if active.update(&self.db.conn).await.is_ok() {
                emit_conversation_upsert(&self.emitter, &self.db.conn, conversation_id).await;
            }
        }
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
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

pub(super) static ENGINE: OnceLock<Arc<TaskEngine>> = OnceLock::new();
