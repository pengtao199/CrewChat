use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：launch
impl TaskEngine {
    pub(super) fn spawn_launch(self: &Arc<Self>, task_id: i32, folder_id: i32, mode: LaunchMode) {
        self.spawn_launch_owned(task_id, folder_id, mode, None);
    }

    /// `token: Some(_)` when the caller already claimed the slot (the pump,
    /// whose loop must see the task as in-flight the moment it continues);
    /// `None` to claim it here.
    pub(super) fn spawn_launch_owned(
        self: &Arc<Self>,
        task_id: i32,
        folder_id: i32,
        mode: LaunchMode,
        token: Option<u64>,
    ) {
        let engine = self.clone();
        tokio::spawn(async move {
            let token = match token {
                Some(token) => token,
                None => engine.claim_launch_slot(task_id, folder_id).await,
            };
            // A merge generation stays `merging` and is NEVER failed: it
            // recovers from git truth (`recover_merging`, driven by the
            // reconcile tick) so a half-merge can go back to review.
            let is_merge = matches!(mode, LaunchMode::Merge { .. });
            // The generation the launch actually operated on, published as soon
            // as it has read the row. A setup failure must be attributed to THAT
            // generation: a cancel + requeue (which bumps run_seq) can land
            // while a slow setup is still unwinding, and failing the row by its
            // *current* sequence would kill the fresh run instead.
            let launched_seq = LaunchSeq::default();
            let result = engine.launch(task_id, mode, &launched_seq, None).await;
            let errored = result.is_err();
            if let Err(e) = result {
                tracing::info!("[work_task] launch {task_id}: {e}");
                if let (false, Some(seq)) = (is_merge, launched_seq.get()) {
                    let failed = work_task_service::fail(
                        &engine.db.conn,
                        task_id,
                        &[WorkTaskStatus::Queued, WorkTaskStatus::Preparing],
                        Some(seq),
                        "setup_error",
                        Some(e),
                    )
                    .await
                    .unwrap_or(false);
                    if failed {
                        engine.emit_upsert(task_id);
                    }
                }
            }
            // Released only after the failure is settled: while the entries are
            // held, the reconcile sweep cannot mistake this row for an orphaned
            // `preparing`, and a cancel still has a kill slot to write to.
            engine.release_setup_slot(task_id, launched_seq.get()).await;
            engine.release_launch_slot(task_id, token).await;
            if errored {
                // A slot may have opened up (or this task left the queue) —
                // keep draining.
                engine.pump_folder(folder_id).await;
            }
        });
    }

    // ── launch ──────────────────────────────────────────────────────────────

    /// `dispatched` is fired once the generation is LIVE — the agent answered,
    /// the conversation is bound and the row carries both coordinates — which
    /// is the point a caller that only wanted to start the run can stop
    /// waiting. Everything after it (the pre-prompt compaction turn, the
    /// prompt itself) belongs to the run, not to the click that asked for it.
    /// See [`DispatchSignal`]; `None` for the launches nobody awaits.
    pub(super) async fn launch(
        self: &Arc<Self>,
        task_id: i32,
        mode: LaunchMode,
        launched_seq: &LaunchSeq,
        dispatched: Option<&DispatchSignal>,
    ) -> Result<(), String> {
        let lock = self.task_lock(task_id).await;
        let _guard = lock.lock().await;

        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| e.to_string())?;
        if task.status != mode.expected_status() {
            return Ok(()); // canceled (or otherwise moved on) before we got here
        }
        let run_seq = task.run_seq;
        launched_seq.set(run_seq);
        let root = get_folder_core(&self.db, task.folder_id)
            .await
            .map_err(|e| e.to_string())?;

        // Effective agent + config: task override > folder task settings >
        // folder default agent. Audited via a config_effective event (values
        // are inherited live, never frozen).
        let cfg: WorkTaskConfig =
            serde_json::from_str(&task.config).unwrap_or_default();
        let settings = work_task_service::settings_get_effective(&self.db.conn, task.folder_id)
            .await
            .unwrap_or_default();
        let (agent_str, mode_id, config_values) = effective_agent_config(&cfg, &settings, &root);
        let agent_str = agent_str.ok_or_else(|| {
            "no agent configured: set a task agent or a folder default".to_string()
        })?;
        let agent_type = parse_agent_type(&agent_str)?;

        // Cheap validation before any side effects; the full prompt is composed
        // after the spawn, once we know whether the session actually resumed.
        if matches!(mode, LaunchMode::Fresh) && cfg.prompt_blocks.is_empty() {
            return Err("prompt is empty".to_string());
        }

        // Out of the queue: from here the task holds its slot and does real
        // work (worktree, init command, agent spawn), so the board must stop
        // calling it "queued". Losing the CAS means a concurrent cancel or a
        // newer generation took over. A merge generation stays `merging`.
        if !matches!(mode, LaunchMode::Merge { .. }) {
            // Reserve the kill slot BEFORE the row can read as `preparing`, and
            // hold it for the whole phase. A cancel cannot wait for the task
            // lock (this launch holds it across the entire init command), so it
            // must always find somewhere to record itself — including during
            // `ensure_worktree`, which takes seconds on a big repo.
            // `spawn_launch_owned` releases the slot, including when the CAS
            // below loses.
            self.reserve_setup_slot(task_id, run_seq).await;
            if !work_task_service::begin_setup(&self.db.conn, task_id, run_seq)
                .await
                .map_err(|e| e.to_string())?
            {
                return Ok(());
            }
            self.emit_upsert(task_id);
        }

        // Cancel gate before the expensive part of setup, so a cancel that
        // landed while we were reading config never reaches `git worktree add`.
        if !still_expected(&self.db.conn, task_id, run_seq, mode.in_flight_status()).await {
            return Ok(());
        }

        // Worktree: reuse the recorded one when it still exists (retry/return),
        // else mint a fresh one pinned to the base recorded FIRST (no drift
        // window between reading the branch and creating the worktree). A merge
        // generation never creates one — merging a fresh empty worktree would
        // "land" as a no-op tree match.
        let wt = if matches!(mode, LaunchMode::Merge { .. }) {
            self.existing_worktree(&task).await?
        } else {
            let wt = self.ensure_worktree(&task, &root, &settings).await?;
            // Run the folder's init command (deps install etc.) before the
            // agent ever sees the tree. Gated on the setup marker, NOT on "the
            // tree was just created": an init that was killed (cancel) or cut
            // short (restart) leaves a half-installed tree that must initialize
            // again. A failure is a setup error — the task must not start
            // half-initialized.
            if let Some(command) = settings
                .init_command
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                if !setup_marker_present(&wt.path).await {
                    self.run_init_command(task_id, run_seq, command, &wt.path)
                        .await?;
                    write_setup_marker(&wt.path).await;
                }
            }
            wt
        };

        let _ = work_task_service::record_event(
            &self.db.conn,
            task_id,
            "config_effective",
            "engine",
            Some(serde_json::json!({
                "agent": agent_str,
                "mode": mode_id,
                "model": config_values.get("model"),
            })),
        )
        .await;

        // Announce the worktree folder before any conversation upsert so every
        // client can group the conversation (idempotent re-broadcast).
        if let Ok(detail) = get_folder_core(&self.db, wt.folder_id).await {
            emit_folder_upsert(&self.emitter, detail);
        }

        // Resume the previous session for retry/return/merge when we have one.
        let resume_session_id = match mode {
            LaunchMode::Fresh => None,
            LaunchMode::Retry | LaunchMode::Return { .. } | LaunchMode::Merge { .. } => {
                match task.conversation_id {
                    Some(conv_id) => conversation::Entity::find_by_id(conv_id)
                        .one(&self.db.conn)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|c| c.external_id),
                    None => None,
                }
            }
        };

        let runtime_env =
            build_session_runtime_env(&self.db, agent_type, resume_session_id.as_deref(), &self.data_dir)
                .await
                .map_err(|e| e.to_string())?;
        verify_agent_installed(agent_type)
            .await
            .map_err(|e| e.to_string())?;

        // Cancel gate before spawning the CLI.
        if !still_expected(&self.db.conn, task_id, run_seq, mode.in_flight_status()).await {
            return Ok(());
        }

        let mut resumed = resume_session_id.is_some();
        let conn_id = match self
            .manager
            .spawn_agent(
                agent_type,
                Some(wt.path.clone()),
                resume_session_id.clone(),
                runtime_env.clone(),
                "work_task".to_string(),
                self.emitter.clone(),
                mode_id.clone(),
                config_values.clone(),
            
            None,)
            .await
        {
            Ok(id) => id,
            Err(e) if resumed => {
                // Resume failed (e.g. the agent lost the session) → fall back
                // to a fresh session in the same worktree, recorded on the
                // timeline.
                tracing::info!("[work_task] resume failed for task {task_id}: {e}; falling back");
                let _ = work_task_service::record_event(
                    &self.db.conn,
                    task_id,
                    "resume_fallback",
                    "engine",
                    Some(serde_json::json!({ "error": e.to_string() })),
                )
                .await;
                resumed = false;
                self.manager
                    .spawn_agent(
                        agent_type,
                        Some(wt.path.clone()),
                        None,
                        runtime_env,
                        "work_task".to_string(),
                        self.emitter.clone(),
                        mode_id.clone(),
                        config_values.clone(),
                    
            None,)
                    .await
                    .map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };

        // Conversation row: reuse when resuming the same session; otherwise a
        // fresh row (fresh runs and resume fallbacks).
        let conversation_id = if resumed {
            task.conversation_id.expect("resumed implies conversation")
        } else {
            let title = conversation_title_for_task(&task.title);
            let id = match create_conversation_core(
                &self.db.conn,
                wt.folder_id,
                agent_type,
                Some(title),
                None,
            )
            .await
            {
                Ok(id) => id,
                Err(e) => {
                    let _ = self.manager.disconnect(&conn_id).await;
                    return Err(e.to_string());
                }
            };
            // The card's name IS this session's identity — freeze it the way a
            // manual rename would, or the per-turn auto-title backfill replaces
            // it with whatever the agent's session file parses to (for agents
            // with no title of their own: the first line of the composed
            // prompt, e.g. "项目：/Users/…"). Issue #495.
            //
            // Strictly before the upsert below: that broadcast is how any
            // client first learns this id, so locking first makes a backfill on
            // this row impossible rather than merely unlikely. A failure here
            // only costs the nice title — never the launch.
            if let Err(e) = conversation_service::lock_title(&self.db.conn, id).await {
                tracing::warn!(
                    "[work_task] task {task_id}: could not lock conversation {id} title: {e}"
                );
            }
            id
        };
        emit_conversation_upsert(&self.emitter, &self.db.conn, conversation_id).await;

        // Publish the run's live coordinates NOW, while the status stays what
        // it was (`preparing`, or `merging` for a merge round).
        //
        // A resumed launch may spend minutes on the pre-prompt compaction just
        // below, and until this write the row named the previous generation's
        // dead connection or nothing at all — so every surface that streams a
        // task's session (the board's 查看会话 drawer above all) showed the
        // LAST round's finished transcript while this one's agent was visibly
        // working. Only for a resume: a fresh session cannot compact, reaches
        // `mark_running` in the next breath, and would only gain a row pointing
        // at a conversation its own failure path may still cancel.
        //
        // Losing the CAS means a cancel or a newer generation got here first,
        // and the rest of this launch is that generation's to skip — unwound
        // exactly like the `mark_running` loss below.
        if resumed {
            let bound = if matches!(mode, LaunchMode::Merge { .. }) {
                work_task_service::mark_merging_live(
                    &self.db.conn,
                    task_id,
                    run_seq,
                    conversation_id,
                    &conn_id,
                )
                .await
            } else {
                work_task_service::mark_preparing_live(
                    &self.db.conn,
                    task_id,
                    run_seq,
                    conversation_id,
                    &conn_id,
                )
                .await
            };
            match bound {
                Ok(true) => self.emit_upsert(task_id),
                Ok(false) => {
                    let _ = self.manager.disconnect(&conn_id).await;
                    return Ok(());
                }
                Err(e) => {
                    let _ = self.manager.disconnect(&conn_id).await;
                    return Err(e.to_string());
                }
            }
        }

        // The generation is live: whoever was only waiting for the dispatch to
        // START (the merge click) is released here, before the compaction turn
        // that would otherwise hold their dialog open for its whole duration.
        if let Some(dispatched) = dispatched {
            dispatched.fire(Ok(()));
        }

        // A resumed session carries every earlier round's context. Give the
        // folder's threshold (when set) a chance to shrink it BEFORE this
        // round's prompt goes in — see `compact_before_prompt` for why this
        // sits ahead of the index insert.
        if resumed {
            self.compact_before_prompt(CompactRequest {
                task_id,
                run_seq,
                conn_id: &conn_id,
                agent_type,
                settings: &settings,
                folder_id: wt.folder_id,
                conversation_id,
                in_flight: mode.in_flight_status(),
            })
            .await;
        }

        let mut blocks =
            match compose_prompt(&cfg, &task, &mode, &settings, resumed, &self.db.conn).await {
                Ok(blocks) => blocks,
                // The one path that abandons the connection without a teardown
                // (it always has). A compaction just fenced this connection's
                // completion, and nothing downstream would ever lift that fence.
                Err(e) => {
                    self.compaction_turns.lock().await.remove(&conn_id);
                    return Err(e);
                }
            };
        // Re-encode attached images for the agent that actually answered the
        // handshake. A task's blocks are STORED — the composer picked their
        // encoding from a transient probe that may not have landed, possibly
        // months ago, and the task's agent can change between then and this
        // run. The live session's advertised capabilities are the only truth.
        //
        // Only for a prompt that actually carries an image, and only then do we
        // wait: `spawn_agent` returns before a FRESH session's handshake
        // completes, so the capabilities are typically still unpublished here.
        // Every other launch (the overwhelming majority) pays nothing.
        if blocks.iter().any(carries_image) {
            match self
                .manager
                .wait_for_prompt_capabilities(&conn_id, IMAGE_CAPABILITY_WAIT)
                .await
            {
                Some(caps) => reencode_images(&mut blocks, &caps),
                None => tracing::warn!(
                    "[work_task] task {task_id}: agent never advertised prompt capabilities; \
                     sending attached images as stored"
                ),
            }
        }

        // Register for completion correlation BEFORE prompting so a fast
        // TurnComplete can't race ahead of the index entry.
        self.index
            .lock()
            .await
            .insert(conn_id.clone(), (task_id, run_seq));

        // queued → running (CAS on run_seq) — or, for a merge generation, just
        // record the live coordinates while the status stays merging. Losing
        // means a concurrent cancel/settle — tear down without side effects.
        let marked = if matches!(mode, LaunchMode::Merge { .. }) {
            work_task_service::mark_merging_live(
                &self.db.conn,
                task_id,
                run_seq,
                conversation_id,
                &conn_id,
            )
            .await
        } else {
            work_task_service::mark_running(
                &self.db.conn,
                task_id,
                run_seq,
                conversation_id,
                &conn_id,
            )
            .await
        };
        // A write that FAILED (a busy database, say) leaves this generation in
        // exactly the state a lost CAS does — never marked live, no prompt
        // sent — so it unwinds the same way instead of propagating out with the
        // connection still registered. `launch`'s caller fails the row on an
        // `Err`, and a failed row holding an index entry for a live connection
        // is the stale-binding hazard the run_seq CAS exists to prevent.
        let marked = match marked {
            Ok(marked) => marked,
            Err(e) => {
                self.forget_connection(&conn_id).await;
                let _ = self.manager.disconnect(&conn_id).await;
                if !resumed {
                    self.cancel_conversation(conversation_id).await;
                }
                return Err(e.to_string());
            }
        };
        if !marked {
            self.forget_connection(&conn_id).await;
            let _ = self.manager.disconnect(&conn_id).await;
            if !resumed {
                self.cancel_conversation(conversation_id).await;
            }
            return Ok(());
        }
        self.emit_upsert(task_id);

        let prompt_head = prompt_head(&blocks);
        match self
            .manager
            .send_prompt_linked_with_message_id(
                &self.db,
                &conn_id,
                blocks,
                Some(wt.folder_id),
                Some(conversation_id),
                None,
                None,
            )
            .await
        {
            Ok(_) => {
                // Timeline round marker: lets the transcript viewer label this
                // prompt's turn with its phase (work / retry / return / merge).
                let _ = work_task_service::record_event(
                    &self.db.conn,
                    task_id,
                    "round",
                    "engine",
                    Some(serde_json::json!({
                        "kind": mode.round_kind(),
                        "intent": mode.round_intent().map(FollowUpIntent::as_str),
                        "run_seq": run_seq,
                        "prompt_head": prompt_head,
                    })),
                )
                .await;
                Ok(())
            }
            Err(e) => {
                self.forget_connection(&conn_id).await;
                let _ = self.manager.disconnect(&conn_id).await;
                if !resumed {
                    self.cancel_conversation(conversation_id).await;
                }
                Err(e.to_string())
            }
        }
    }

    /// Resolve (and if needed create) the task's worktree. On creation the base
    /// branch + sha are recorded BEFORE `git worktree add` runs against that
    /// exact sha, and the directory goes wherever the folder's settings put it
    /// (next to the project folder by default).
    pub(super) async fn ensure_worktree(
        &self,
        task: &crate::db::entities::work_task::Model,
        root: &crate::models::FolderDetail,
        settings: &WorkTaskFolderSettings,
    ) -> Result<WorktreeRef, String> {
        if let Some(wt_id) = task.worktree_folder_id {
            if let Ok(detail) = get_folder_core(&self.db, wt_id).await {
                if Path::new(&detail.path).exists() {
                    return Ok(WorktreeRef {
                        folder_id: detail.id,
                        path: detail.path,
                    });
                }
            }
        }

        // The directory is gone, but the branch may not be: a retry / follow-up
        // after the checkout was removed must continue the work already
        // committed on the branch — not restart on a fresh base while those
        // commits sit stranded on a branch nothing points to. Only when the
        // branch cannot be re-checked-out does the fresh mint below take over
        // (re-recording base + branch).
        if let Some(wt) = self.recreate_worktree_from_branch(task, root, settings).await {
            return Ok(wt);
        }

        // Where the task's branch starts, and what its diff is measured
        // against. Normally both are the project folder's current HEAD; a task
        // created FOR a particular branch starts at that branch's tip instead,
        // and a task that IS a pull request starts at that pull request's head
        // and measures against the merge base (see `pr_checkout_point`).
        let (base_branch, base_sha, start_at) = match self.pr_checkout_point(task, root).await? {
            Some(point) => point,
            None => match chosen_base_branch(task) {
                Some(branch) => {
                    // A branch that is gone by the time the task is claimed
                    // FAILS the setup: the choice exists so the work lands
                    // somewhere specific, and quietly branching from the
                    // checkout instead would be the wrong answer delivered
                    // without a word. The LOCAL branch specifically — the
                    // merge lands into the project checkout, which can only be
                    // on a local branch — and looking it up this way is also
                    // what keeps an arbitrary string out of the git commands
                    // (and out of the merge prompt) downstream: only a name
                    // git itself resolved under `refs/heads/` gets through.
                    let tip = task_git::local_branch_tip(&root.path, &branch)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| {
                            format!(
                                "the task's base branch '{branch}' does not exist in the \
                                 project folder"
                            )
                        })?;
                    (branch, tip.clone(), tip)
                }
                None => {
                    let head = resolve_git_head(&root.path).await.map_err(|e| e.to_string())?;
                    let base_branch = head.branch.ok_or_else(|| {
                        "project folder is not on a branch (detached HEAD?)".to_string()
                    })?;
                    let base_sha = task_git::rev_parse(&root.path, "HEAD")
                        .await
                        .map_err(|e| e.to_string())?;
                    (base_branch, base_sha.clone(), base_sha)
                }
            },
        };

        let branch = format!("task/{}", task.id);
        let dir = format!("{}-task-{}", basename(&root.path), task.id);
        // A configured root that does not exist yet (the folder's first task)
        // needs no `mkdir`: `git worktree add` creates the leading directories
        // along with the checkout.
        let mut wt_path = worktree_path_in(&root.path, settings.worktree_root.as_deref(), &dir);
        let mut branch_used = branch.clone();

        if let Err(e) = git_worktree_add(
            root.path.clone(),
            branch.clone(),
            wt_path.clone(),
            Some(start_at.clone()),
        )
        .await
        {
            // A leftover from a prior attempt may collide — retry once with a
            // generation-scoped suffix.
            let suffix = format!("r{}b", task.run_seq);
            branch_used = format!("{branch}-{suffix}");
            wt_path = worktree_path_in(
                &root.path,
                settings.worktree_root.as_deref(),
                &format!("{dir}-{suffix}"),
            );
            git_worktree_add(
                root.path.clone(),
                branch_used.clone(),
                wt_path.clone(),
                Some(start_at.clone()),
            )
            .await
            .map_err(|_| format!("worktree add failed: {e}"))?;
        }

        // Whatever a pull-request checkout fetched is now held by the task's
        // own branch, so the names it was fetched under can go.
        self.drop_pr_fetch_refs(task, root).await;

        let wt = open_worktree_folder_core(&self.db, wt_path, task.folder_id)
            .await
            .map_err(|e| e.to_string())?;
        work_task_service::attach_worktree(
            &self.db.conn,
            task.id,
            wt.id,
            &base_branch,
            &base_sha,
            &branch_used,
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(WorktreeRef {
            folder_id: wt.id,
            path: wt.path,
        })
    }

    /// `(base branch, base sha, start commit)` for a task triggered from a
    /// pull request; `None` for every other task, which starts at the project
    /// folder's HEAD.
    ///
    /// Three decisions live here:
    ///
    /// - **Start at the pull request's head**, fetched through `pull/{n}/head`
    ///   — a ref GitHub keeps even after the head branch is deleted, and the
    ///   only one that works without write access to it.
    /// - **Pin to the OID recorded at trigger time**, not to whatever the ref
    ///   points at now: the user triggered a specific state, and a push landing
    ///   while the task sat in the queue must not silently change the subject.
    ///   When that commit is no longer on the server (a force-push), this
    ///   fails with an explanation instead of checking out a stranger.
    /// - **Measure the diff from the MERGE BASE**, not from the head. The task
    ///   is reviewed as "the pull request plus whatever the agent did", so the
    ///   pull request's own changes have to be inside the diff; anchoring at
    ///   the head would hide exactly what is under review. Anchoring at the
    ///   base branch's tip would be wrong the other way — every commit the base
    ///   gained since would show up as this task's work.
    async fn pr_checkout_point(
        &self,
        task: &crate::db::entities::work_task::Model,
        root: &crate::models::FolderDetail,
    ) -> Result<Option<(String, String, String)>, String> {
        if task.source_kind.as_deref() != Some(SOURCE_KIND_PR) {
            return Ok(None);
        }
        let meta = task
            .source_meta
            .as_deref()
            .and_then(|s| serde_json::from_str::<ForgeSourceMeta>(s).ok())
            .ok_or_else(|| "the task's source information is unreadable".to_string())?;
        let (Some(base_ref), Some(head_sha)) = (meta.base_ref.as_deref(), meta.head_sha.as_deref())
        else {
            return Err(
                "this pull request task is missing its branch information — trigger it again"
                    .to_string(),
            );
        };

        // Per-task ref names: these fetches run in the shared project folder,
        // where `FETCH_HEAD` belongs to whoever wrote it last.
        let base_ref_local = format!("refs/codeg/task-{}/pr-base", task.id);
        let head_ref_local = format!("refs/codeg/task-{}/pr-head", task.id);
        // Fetched with the account the task was triggered by, over an explicit
        // URL — NOT through the folder's `origin`. This is the engine's only
        // setup step that touches the network, and a private repository's
        // origin may be SSH or may have no cached credential at all; going
        // through the pinned identity is what makes a private repository work
        // here at all, and it is the same identity the delivery will push as.
        let ctx = DeliveryCtx {
            conn: &self.db.conn,
            data_dir: &self.data_dir,
            provider: meta.provider,
            server_host: &meta.server_host,
            account_id: &meta.account_id,
            owner_repo: &meta.owner_repo,
        };
        let base_tip = self
            .forge
            .fetch_ref(
                &ctx,
                &root.path,
                &format!("refs/heads/{base_ref}"),
                &base_ref_local,
            )
            .await
            .map_err(|e| {
                format!("could not fetch '{base_ref}', the pull request's base branch: {e}")
            })?;
        // Both forges publish a proposed change's head under a server-side ref
        // — that is what makes it fetchable without adding the contributor's
        // remote — but they spell it differently, and the wrong spelling is
        // simply a ref that does not exist.
        let fetched_head = self
            .forge
            .fetch_ref(
                &ctx,
                &root.path,
                &meta.provider.change_head_ref(meta.number),
                &head_ref_local,
            )
            .await
            .map_err(|e| {
                format!(
                    "could not fetch {} #{}: {e}",
                    meta.provider.change_noun(),
                    meta.number
                )
            })?;

        if !fetched_head.eq_ignore_ascii_case(head_sha)
            && !task_git::commit_present(&root.path, head_sha)
                .await
                .unwrap_or(false)
        {
            return Err(format!(
                "pull request #{} was force-pushed since this task was created, and the commit \
                 it was triggered on ({}) is no longer there — trigger it again from the \
                 workbench to work on the current head",
                meta.number,
                first_chars(head_sha, 7)
            ));
        }
        let base_sha = task_git::merge_base(&root.path, &base_tip, head_sha)
            .await
            .map_err(|e| {
                format!("could not find where pull request #{} branched off: {e}", meta.number)
            })?;
        // The refs stay for now — see `drop_pr_fetch_refs`: until the task's
        // branch exists, they are the only thing keeping what we just fetched
        // reachable. A failed setup leaves them behind, which costs nothing:
        // the next attempt force-updates the same two names.
        Ok(Some((base_ref.to_string(), base_sha, head_sha.to_string())))
    }

    /// Drop the refs `pr_checkout_point` fetched under. Called only once the
    /// task's branch exists: those refs are what keeps the fetched commits
    /// reachable until then, and deleting them earlier would leave a window in
    /// which a concurrent `git gc` could prune what we are about to check out.
    async fn drop_pr_fetch_refs(
        &self,
        task: &crate::db::entities::work_task::Model,
        root: &crate::models::FolderDetail,
    ) {
        if task.source_kind.as_deref() != Some(SOURCE_KIND_PR) {
            return;
        }
        for suffix in ["pr-base", "pr-head"] {
            task_git::delete_ref(&root.path, &format!("refs/codeg/task-{}/{suffix}", task.id)).await;
        }
    }

    /// Try to re-create the task's worktree from its still-existing work
    /// branch (`git worktree add <path> <branch>`, prior commits intact), at
    /// the recorded folder's path — or the standard naming when that row is
    /// gone. `None` when the branch is gone too, the path is occupied, or git
    /// refuses (e.g. the branch is checked out elsewhere) — the caller then
    /// mints a fresh worktree.
    async fn recreate_worktree_from_branch(
        &self,
        task: &crate::db::entities::work_task::Model,
        root: &crate::models::FolderDetail,
        settings: &WorkTaskFolderSettings,
    ) -> Option<WorktreeRef> {
        let branch = task.work_branch.as_deref()?;
        // The recorded base stays authoritative for the branch's history; the
        // three are only ever written together, but a row missing them must
        // fall through to the fresh mint that records them.
        let base_branch = task.base_branch.as_deref()?;
        let base_sha = task.base_sha.as_deref()?;
        // The LOCAL branch specifically — an unqualified lookup would let a
        // same-name tag answer here and the add below would check out a
        // detached HEAD instead of the branch.
        match task_git::local_branch_tip(&root.path, branch).await {
            Ok(Some(_)) => {}
            _ => return None, // branch gone too — nothing to continue from
        }
        // Prefer the recorded folder row's path so the row binds back to the
        // same workspace entry; fall back to the standard naming.
        let recorded = match task.worktree_folder_id {
            Some(wt_id) => get_folder_core(&self.db, wt_id).await.ok().map(|d| d.path),
            None => None,
        };
        let path = recorded.unwrap_or_else(|| {
            worktree_path_in(
                &root.path,
                settings.worktree_root.as_deref(),
                &format!("{}-task-{}", basename(&root.path), task.id),
            )
        });
        if Path::new(&path).exists() {
            return None; // something else lives there now
        }
        if let Err(e) = task_git::worktree_add_existing_branch(&root.path, &path, branch).await {
            tracing::info!(
                "[work_task] task {}: could not re-create the worktree from branch \
                 {branch}: {e}",
                task.id
            );
            return None;
        }
        let wt = match open_worktree_folder_core(&self.db, path.clone(), task.folder_id).await {
            Ok(wt) => wt,
            Err(e) => {
                // The checkout exists but has no folder row; the fresh-mint
                // fallback will collide on the path and suffix itself, so the
                // launch still proceeds.
                tracing::warn!(
                    "[work_task] task {}: recreated worktree at {path} but could not open \
                     its folder: {e}",
                    task.id
                );
                return None;
            }
        };
        // Re-attach under the ORIGINAL base: the branch's history is diffed
        // against it, and re-recording today's HEAD would misstate the change
        // set the review shows.
        if let Err(e) = work_task_service::attach_worktree(
            &self.db.conn,
            task.id,
            wt.id,
            base_branch,
            base_sha,
            branch,
        )
        .await
        {
            tracing::warn!(
                "[work_task] task {}: could not re-attach the recreated worktree: {e}",
                task.id
            );
        }
        Some(WorktreeRef {
            folder_id: wt.id,
            path: wt.path,
        })
    }

    /// The task's recorded worktree, required to exist on disk — the merge
    /// generation must never mint a fresh (empty) one.
    async fn existing_worktree(&self, task: &crate::db::entities::work_task::Model) -> Result<WorktreeRef, String> {
        let wt_id = task
            .worktree_folder_id
            .ok_or_else(|| "task has no worktree".to_string())?;
        let detail = get_folder_core(&self.db, wt_id)
            .await
            .map_err(|e| e.to_string())?;
        if !Path::new(&detail.path).exists() {
            return Err("the task worktree no longer exists on disk".to_string());
        }
        Ok(WorktreeRef {
            folder_id: detail.id,
            path: detail.path,
        })
    }

    /// Run the folder's worktree init command in an uninitialized worktree.
    /// The outcome is always recorded on the timeline; a failure aborts the
    /// launch (setup error) so the agent never starts half-initialized.
    async fn run_init_command(
        &self,
        task_id: i32,
        run_seq: i32,
        command: &str,
        cwd: &str,
    ) -> Result<(), String> {
        let run = self.run_setup_shell(task_id, run_seq, command, cwd).await;
        let (exit_code, tail) = match &run {
            Ok((code, tail)) => (*code, tail.clone()),
            Err(e) => (None, e.clone()),
        };
        let ok = matches!(run, Ok((Some(0), _)));
        let _ = work_task_service::record_event(
            &self.db.conn,
            task_id,
            "init_command",
            "engine",
            Some(serde_json::json!({
                "command": command,
                "exit_code": exit_code,
                "output_tail": (!ok && !tail.is_empty()).then_some(tail.clone()),
            })),
        )
        .await;
        if ok {
            Ok(())
        } else {
            let code = exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
            Err(format!("worktree init command failed (exit {code}): {tail}"))
        }
    }

    /// Open the kill slot for a generation entering setup. Held for the whole
    /// preparing phase so a cancel always has somewhere to record itself, even
    /// before (or after) a child process exists.
    async fn reserve_setup_slot(&self, task_id: i32, run_seq: i32) {
        self.setup_children.lock().await.insert(
            task_id,
            SetupChild {
                kill_requested: false,
                run_seq,
                wake: Arc::new(Notify::new()),
            },
        );
    }

    /// Close the slot once the launch is over — but only if it is still this
    /// generation's, so a slow unwinding launch cannot drop the slot a newer
    /// one is relying on.
    async fn release_setup_slot(&self, task_id: i32, run_seq: Option<i32>) {
        let Some(run_seq) = run_seq else { return };
        let mut children = self.setup_children.lock().await;
        if children.get(&task_id).is_some_and(|s| s.run_seq == run_seq) {
            children.remove(&task_id);
        }
    }

    /// `run_shell_capture` for the setup phase: the child is bound to the
    /// task's kill slot so a cancel kills its process TREE instead of letting a
    /// multi-minute install run to completion after the user gave up.
    ///
    /// Every cancel window is covered, and the kill always happens HERE, in the
    /// task that owns the child:
    /// - before the spawn → the slot already says `kill_requested`, so no child
    ///   is ever started;
    /// - during the spawn → `notify_one` leaves a permit, so the wait loop's
    ///   first `notified()` returns immediately and kills;
    /// - while waiting → the wake fires and we kill, then keep awaiting the
    ///   child so it is still reaped normally;
    /// - after the child was reaped → nobody holds its pid any more, so a
    ///   recycled pid can never be signalled.
    async fn run_setup_shell(
        &self,
        task_id: i32,
        run_seq: i32,
        line: &str,
        cwd: &str,
    ) -> Result<(Option<i32>, String), String> {
        let wake = {
            // The slot is opened by `launch` before setup begins; re-assert it
            // if it somehow went missing, so the child is never unkillable.
            let mut children = self.setup_children.lock().await;
            let slot = children.entry(task_id).or_insert_with(|| SetupChild {
                kill_requested: false,
                run_seq,
                wake: Arc::new(Notify::new()),
            });
            if slot.kill_requested {
                return Err("canceled before the init command started".to_string());
            }
            slot.run_seq = run_seq;
            slot.wake.clone()
        };
        let child = shell_command(line, cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();

        let wait = child.wait_with_output();
        tokio::pin!(wait);
        let mut killed = false;
        let out = loop {
            tokio::select! {
                out = &mut wait => break out,
                // Kill once, then go back to awaiting the child: the process
                // is only reaped by the `wait` branch, so `pid` is still ours.
                _ = wake.notified(), if !killed => {
                    killed = true;
                    kill_process_tree(pid).await;
                }
            }
        };
        let out = out.map_err(|e| e.to_string())?;
        Ok(combine_capture(&out))
    }

    /// Stop the setup of `task_id`, but only when the slot belongs to
    /// generation `run_seq` — a stale cancel must not stop a task that was
    /// already requeued and relaunched. Records the request and wakes the
    /// runner; the actual kill happens in `run_setup_shell`, which is the only
    /// place that knows the child is still alive.
    pub(super) async fn kill_setup_child(&self, task_id: i32, run_seq: i32) {
        let wake = {
            let mut children = self.setup_children.lock().await;
            match children.get_mut(&task_id) {
                Some(slot) if slot.run_seq == run_seq => {
                    slot.kill_requested = true;
                    slot.wake.clone()
                }
                _ => return,
            }
        };
        wake.notify_one();
    }
}
