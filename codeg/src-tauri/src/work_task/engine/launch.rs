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

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// How a launch composes its prompt.
pub(super) enum LaunchMode {
    /// First run: the task's own prompt blocks.
    Fresh,
    /// Retry after failure: resume the session if possible and ask to continue.
    Retry,
    /// A follow-up on a reviewed task: the user's text, framed by their intent,
    /// plus whatever the composer attached out of band (images, pasted bytes).
    Return {
        intent: FollowUpIntent,
        feedback: String,
        attachments: Vec<serde_json::Value>,
    },
    /// Merge generation: the agent lands the task onto the base branch itself
    /// (sync base into the worktree, resolve conflicts, merge into base). The
    /// task sits in `merging` for the whole turn; the engine settles from git
    /// truth, never from the agent's word.
    Merge {
        root_path: String,
        base_branch: String,
        work_branch: String,
        /// "squash" | "merge"
        strategy: String,
        /// `None` → the agent writes the commit message itself.
        message: Option<String>,
        /// Whatever the user added in the merge dialog, verbatim. `None` when
        /// they left it empty — which is every merge dispatched before this
        /// existed, and the auto-merge sweep's every landing.
        instructions: Option<String>,
    },
}

impl LaunchMode {
    /// The task status a launch of this mode expects to find when it starts.
    fn expected_status(&self) -> WorkTaskStatus {
        match self {
            LaunchMode::Merge { .. } => WorkTaskStatus::Merging,
            _ => WorkTaskStatus::Queued,
        }
    }

    /// The status the task holds for the REST of the launch, once setup has
    /// begun — what the cancel gates re-check. A merge generation stays
    /// `merging` throughout; every other mode moves `queued → preparing` before
    /// it touches the worktree.
    fn in_flight_status(&self) -> WorkTaskStatus {
        match self {
            LaunchMode::Merge { .. } => WorkTaskStatus::Merging,
            _ => WorkTaskStatus::Preparing,
        }
    }

    /// Timeline `round` label for the prompt this mode composes. Every
    /// follow-up intent shares the `return` stage: the folder's per-stage
    /// prompt settings stay four stages wide, and the intent rides the `round`
    /// event beside this label for the transcript's phase divider.
    fn round_kind(&self) -> &'static str {
        match self {
            LaunchMode::Fresh => "work",
            LaunchMode::Retry => "retry",
            LaunchMode::Return { .. } => "return",
            LaunchMode::Merge { .. } => "merge",
        }
    }

    /// The follow-up intent this launch carries, for the `round` marker.
    fn round_intent(&self) -> Option<FollowUpIntent> {
        match self {
            LaunchMode::Return { intent, .. } => Some(*intent),
            _ => None,
        }
    }

    /// Whether this launch may write to the worktree. A question is answered in
    /// chat; everything else is work.
    fn is_read_only(&self) -> bool {
        matches!(
            self,
            LaunchMode::Return {
                intent: FollowUpIntent::Question,
                ..
            }
        )
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Pick the launch mode for a pump-driven launch from the task's history: a
/// task with a prior conversation continues (retry semantics); one with no
/// session to continue starts fresh. Explicit returns launch directly with
/// `LaunchMode::Return`.
///
/// The bound session IS the whole criterion, which is what lets a transition
/// decide how the next start behaves: `retry` (failed → queued) keeps the link
/// and therefore continues, while `requeue_canceled` drops it so a task the
/// user put back on the board runs from the top. `compose_prompt` must not
/// read `Fresh` as "this task has no history" — see its doc.
pub(super) fn launch_mode_for(task: &crate::db::entities::work_task::Model) -> LaunchMode {
    if task.conversation_id.is_some() {
        LaunchMode::Retry
    } else {
        LaunchMode::Fresh
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The branch the task was created FOR, if its config names one. A blank
/// choice is no choice: the editor writes the empty string for "the project
/// folder's current branch", which is also what every task predating the
/// field carries.
pub(super) fn chosen_base_branch(task: &crate::db::entities::work_task::Model) -> Option<String> {
    serde_json::from_str::<WorkTaskConfig>(&task.config)
        .ok()?
        .base_branch
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Layered agent config: task override wins wholesale; else the folder's task
/// settings; else the folder's default agent with no extra options.
pub(super) fn effective_agent_config(
    cfg: &WorkTaskConfig,
    settings: &WorkTaskFolderSettings,
    root: &crate::models::FolderDetail,
) -> (
    Option<String>,
    Option<String>,
    std::collections::BTreeMap<String, String>,
) {
    if let Some(agent) = cfg.agent_type.clone() {
        return (Some(agent), cfg.mode_id.clone(), cfg.config_values.clone());
    }
    if let Some(agent) = settings.default_agent_type.clone() {
        return (
            Some(agent),
            settings.mode_id.clone(),
            settings.config_values.clone(),
        );
    }
    let folder_default = root
        .default_agent_type
        .as_ref()
        .and_then(|a| serde_json::to_value(a).ok())
        .and_then(|v| v.as_str().map(String::from));
    (
        folder_default,
        settings.mode_id.clone(),
        settings.config_values.clone(),
    )
}

/// Compose the prompt for a launch mode. Fresh runs replay the task's blocks.
/// Retry/return compose against the session we actually got: a resumed session
/// already carries the task context, while a fresh fallback session needs the
/// full original description again. Every prompt ends with the worktree guard,
/// then with whatever the folder's settings add for this stage.
///
/// "Fresh" is a mode, NOT a promise that the task is pristine: `requeue_canceled`
/// drops the session link so a re-queued task starts over, which sends a task
/// that already ran — worktree, branch and all — back through the fresh arm. Both
/// replaying arms therefore read the instruction log rather than the mode.
pub(super) async fn compose_prompt(
    cfg: &WorkTaskConfig,
    task: &crate::db::entities::work_task::Model,
    mode: &LaunchMode,
    settings: &WorkTaskFolderSettings,
    resumed: bool,
    conn: &sea_orm::DatabaseConnection,
) -> Result<Vec<PromptInputBlock>, String> {
    let mut blocks: Vec<PromptInputBlock> = Vec::new();
    let original: Vec<PromptInputBlock> = cfg
        .prompt_blocks
        .iter()
        .map(|v| serde_json::from_value::<PromptInputBlock>(v.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("bad prompt blocks: {e}"))?;

    // Whether this launch is (re)doing the task's ORIGINAL work order, as
    // opposed to follow-up work the user asked for later. The worktree guard
    // below keys its licence on this: a report-deliverable task forbids code
    // changes on its original order, but a returned "now apply the fix" turn
    // is precisely a change order and must get the normal write licence.
    let mut original_work_order = false;
    // A replay standing in for an unanswered question: it already says "do not
    // change any files for it", so the guard must not hand back the commit
    // grant three blocks later.
    let mut replayed_question = false;

    match mode {
        LaunchMode::Fresh => {
            if original.is_empty() {
                return Err("prompt is empty".to_string());
            }
            // Read the log, not the mode. A fresh launch used to mean a task
            // with no history at all; since a requeue drops the session link it
            // also means "run this again from the top", and that task may owe
            // the user an instruction its stopped generation never answered —
            // a returned "rework this" is silently lost otherwise, and a
            // returned question comes back as a work order with a full commit
            // grant. Same two reads as the retry arm, for the same reasons.
            let scan = instruction_scan(conn, task.id).await;
            original_work_order = scan.interrupted.is_none();
            replayed_question = matches!(scan.interrupted, Some(FollowUpIntent::Question));
            // Only when a generation actually got as far as its own turn
            // (`started_at` is written by `mark_running` and never cleared): a
            // task canceled during setup has nothing in its worktree to warn
            // about, and its prompt stays byte-identical to a first run's.
            if task.started_at.is_some() {
                blocks.push(PromptInputBlock::Text {
                    text: "You are running this task again from the top: an earlier run was \
                           stopped and the task was put back on the board, so this session \
                           carries none of that run's context. Its worktree may still hold \
                           that run's work — check the current state first and build on \
                           whatever is already there instead of redoing it. The original task \
                           was:"
                        .to_string(),
                });
            }
            blocks.extend(original);
            push_replay(&mut blocks, &scan, resumed, task.id);
        }
        LaunchMode::Retry => {
            blocks.push(PromptInputBlock::Text {
                text: "The previous run of this task was interrupted. Continue working in \
                       this worktree and complete the task."
                    .to_string(),
            });
            if !resumed {
                blocks.push(PromptInputBlock::Text {
                    text: "The original task follows for reference:".to_string(),
                });
                blocks.extend(original);
            }
            // Replay whatever instruction the interrupted generation still
            // owed the user, framed the way they meant it — an unanswered
            // question must not come back as a work order.
            let scan = instruction_scan(conn, task.id).await;
            // A retry stands in for whichever turn was interrupted, and the
            // licence follows THAT turn — `interrupted`, not the newest note:
            // a retry/requeue note refines the turn it interrupts (a hint, a
            // screenshot), it does not change what kind of turn it was. With
            // no unsettled follow-up underneath, this is the original order
            // again.
            original_work_order = scan.interrupted.is_none();
            replayed_question = matches!(scan.interrupted, Some(FollowUpIntent::Question));
            push_replay(&mut blocks, &scan, resumed, task.id);
        }
        LaunchMode::Return {
            intent,
            feedback,
            attachments,
        } => {
            if !resumed {
                // Session resume failed — the fresh session has no context, so
                // replay the task before the feedback.
                blocks.push(PromptInputBlock::Text {
                    text: "You are picking up a task whose previous session could not be \
                           resumed. The worktree already contains that session's work. \
                           The original task was:"
                        .to_string(),
                });
                blocks.extend(original);
            }
            blocks.push(PromptInputBlock::Text {
                text: follow_up_text(*intent, feedback),
            });
            blocks.extend(attachment_blocks(attachments, task.id));
        }
        LaunchMode::Merge {
            root_path,
            base_branch,
            work_branch,
            strategy,
            message,
            instructions,
        } => {
            if !resumed {
                blocks.push(PromptInputBlock::Text {
                    text: "You are picking up a task whose previous session could not be \
                           resumed. The worktree already contains that session's work. \
                           The original task was:"
                        .to_string(),
                });
                blocks.extend(original);
            }
            let land_command = if strategy == "merge" {
                format!(
                    "git -C \"{root_path}\" merge --no-ff -m \"<message>\" {work_branch}"
                )
            } else {
                format!(
                    "git -C \"{root_path}\" merge --squash {work_branch} && \
                     git -C \"{root_path}\" commit -m \"<message>\""
                )
            };
            // Indented under step 3 rather than left dangling after it: it says
            // what to substitute for that step's `<message>`, and read as a
            // fourth line it looked like a fourth instruction.
            let message_rule = match message {
                Some(m) => format!("   Use exactly this commit message:\n   {m}"),
                None => "   For `<message>`, write a concise Conventional Commits \
                         message yourself, summarizing what this task changed."
                    .to_string(),
            };
            // The user's own words get their own paragraph, before the closing
            // rules — those rules stay last on purpose (same reason the standing
            // worktree guard below is the last built-in block): whatever the
            // user asked for, it does not license a push or a self-deletion.
            let extra = match instructions {
                Some(i) => format!("\nAlso follow these instructions from the user:\n{i}\n"),
                None => String::new(),
            };
            blocks.push(PromptInputBlock::Text {
                text: format!(
                    "The user accepted this task — land it onto the base branch \
                     `{base_branch}` now, doing all git operations yourself:\n\
                     1. Commit any uncommitted changes in this worktree to the current \
                     branch (`{work_branch}`).\n\
                     2. Run `git merge {base_branch}` here and resolve every conflict so \
                     both the base's changes and this task's intent survive; complete the \
                     merge commit.\n\
                     3. Land onto the base checkout at `{root_path}`:\n   {land_command}\n\
                     {message_rule}\n\
                     {extra}\n\
                     Do NOT push, do NOT delete this worktree or its branch, and do not \
                     change anything else on the base branch — leave the checkout at \
                     `{root_path}` on `{base_branch}`, never switch branches there. \
                     Finish with one short line saying what landed."
                ),
            });
        }
    }

    // The standing worktree guard — a merge generation replaces it with its
    // own instructions (it exists to forbid exactly what a merge must do).
    //
    // It is the LAST built-in block, so its licence clause is the last thing
    // the agent reads: a read-only turn — and a report-deliverable original
    // order — has to swap that clause out, or "commit to the current branch as
    // you like" would quietly undo the intent's own "don't touch any file"
    // instruction several blocks earlier.
    if !matches!(mode, LaunchMode::Merge { .. }) {
        let branch = task
            .work_branch
            .as_deref()
            .map(|b| format!(" (branch `{b}`)"))
            .unwrap_or_default();
        let base = task
            .base_branch
            .as_deref()
            .map(|b| format!(" (`{b}`)"))
            .unwrap_or_default();
        let licence = if mode.is_read_only() || replayed_question {
            format!(
                "This turn is a question, not a work order: answer it in your reply and do NOT \
                 create, edit, delete or commit any file, and do not merge into, rebase onto, or \
                 push the base branch{base}. If answering would require a change, describe the \
                 change instead of making it."
            )
        } else if original_work_order && cfg.deliverable.as_deref() == Some(DELIVERABLE_REPORT) {
            // Report-deliverable order (forge plan-first / review-only):
            // the generic "commit as you like" grant would quietly undo the
            // task's own "analysis only" instruction several blocks earlier —
            // the licence must defer to the task text, not overrule it.
            format!(
                "This turn delivers a report, not code changes: put the full findings in your \
                 final reply — the user reads them from there, and may return this task for \
                 follow-up work afterwards. Commit only what the task's instructions \
                 explicitly allow (often nothing), and do NOT merge into, rebase onto, or \
                 push the base branch{base}."
            )
        } else {
            format!(
                "Commit to the current branch as you like, but do NOT merge into, rebase onto, \
                 or push the base branch{base} — the user lands the result after review. Finish \
                 with a short summary of what you did."
            )
        };
        blocks.push(PromptInputBlock::Text {
            text: format!(
                "—— Work task context ——\nYou are working inside a dedicated git worktree for \
                 this task{branch}. {licence}\nIf the `task_progress` and `task_complete` tools \
                 are available to you, report milestones with `task_progress` as you go, and \
                 call `task_complete` once right before you finish (verdict `success`, \
                 `needs_review`, or `blocked`, plus a short summary).",
            ),
        });
    }

    // Whatever the user added in task settings, always last: it refines the
    // built-in wording above it, and staying at the end keeps `prompt_head`
    // (the transcript's round marker) on the prompt's own opening text.
    blocks.extend(stage_prompt_block(settings, mode.round_kind()));
    Ok(blocks)
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The user's own instructions for a stage: the `all` text (every stage) then
/// the stage's own, as one trailing block. Empty when neither is configured.
pub(super) fn stage_prompt_block(
    settings: &WorkTaskFolderSettings,
    stage: &str,
) -> Option<PromptInputBlock> {
    let extras: Vec<&str> = [STAGE_PROMPT_ALL, stage]
        .into_iter()
        .filter_map(|key| settings.stage_prompts.get(key))
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .collect();
    if extras.is_empty() {
        return None;
    }
    Some(PromptInputBlock::Text {
        text: format!("—— Additional instructions ——\n{}", extras.join("\n\n")),
    })
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The prompt text for a follow-up on a reviewed task. The intent decides the
/// framing, which is the whole point of having intents: the same sentence from
/// the user means "fix this", "also do this" or "explain this" depending on it,
/// and an agent told it was *returned* work will start editing either way.
pub(super) fn follow_up_text(intent: FollowUpIntent, feedback: &str) -> String {
    match intent {
        // Historical wording, kept verbatim: this is what an unlabelled
        // follow-up composes, so the default path is unchanged.
        FollowUpIntent::Revise => format!(
            "The user reviewed your work on this task and returned it with the following \
             feedback. Address it in this same worktree:\n\n{feedback}"
        ),
        FollowUpIntent::Continue => format!(
            "The user reviewed your work on this task and accepted it as it stands. Keep going \
             in this same worktree with the following additional work — do not redo or second-\
             guess what is already there:\n\n{feedback}"
        ),
        FollowUpIntent::Question => format!(
            "The user has a question about your work on this task. Answer it directly in your \
             reply. This is a question, not a work order: do not create, edit, delete or commit \
             any file unless the user explicitly asks you to. If answering would require a \
             change, describe the change instead of making it.\n\n{feedback}"
        ),
        FollowUpIntent::Verify => {
            let base = "The user wants this task checked over before they accept it. Review your \
                        own work: read the full diff of this worktree against the base branch \
                        looking for bugs, leftovers, debug code and anything the task asked for \
                        but you did not do; run the project's own checks or tests; and fix what \
                        you find. Report what you checked and what you changed.";
            if feedback.is_empty() {
                base.to_string()
            } else {
                format!("{base}\n\nWhat they want you to pay attention to:\n\n{feedback}")
            }
        }
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The attachment blocks recorded on a `user_action` payload, if any. Absent on
/// every event written before follow-ups could carry attachments, so a missing
/// or malformed field simply means "nothing was attached".
pub(super) fn payload_blocks(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    payload
        .get("blocks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Parse stored attachment blocks, dropping (with a warning) any that no longer
/// deserialize. Unlike the task's own prompt, an attachment is an addition to
/// the instruction — losing one must not stop the run that carries the rest.
pub(super) fn attachment_blocks(raw: &[serde_json::Value], task_id: i32) -> Vec<PromptInputBlock> {
    raw.iter()
        .filter_map(|v| match serde_json::from_value::<PromptInputBlock>(v.clone()) {
            Ok(block) => Some(block),
            Err(e) => {
                tracing::warn!("[work_task] task {task_id}: dropping bad attachment block: {e}");
                None
            }
        })
        .collect()
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// How long a launch waits for the agent to advertise what a prompt may carry,
/// before sending its attached images in whatever shape they were stored. Only
/// image-bearing prompts ever wait, and only until the handshake lands — which
/// is the same round trip the prompt itself is about to need anyway.
pub(super) const IMAGE_CAPABILITY_WAIT: Duration = Duration::from_secs(20);

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Whether this block carries image bytes in either of the two wire encodings.
pub(super) fn carries_image(block: &PromptInputBlock) -> bool {
    match block {
        PromptInputBlock::Image { .. } => true,
        PromptInputBlock::Resource {
            mime_type, blob, ..
        } => {
            blob.is_some()
                && mime_type
                    .as_deref()
                    .is_some_and(|m| m.starts_with("image/"))
        }
        _ => false,
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Swap every attached image into the shape the connected agent advertised.
///
/// Two wire encodings carry the same bytes: a native `Image` block, and a
/// `Resource` whose `blob` holds them under an image mime type (what an agent
/// that rejects image content but accepts embedded context takes). The composer
/// picks one at compose time from a probe; this picks again at dispatch, when
/// the session has actually said what it accepts.
///
/// Grok needs neither swap — it advertises both bits — but its images still get
/// sorted per mime further downstream, in `acp::connection`, which is the last
/// point that sees the blocks.
///
/// Only image-carrying blocks are touched, and only to move between those two
/// encodings — never to invent or drop content. An agent that advertises
/// neither is left alone: there is no third shape to reach for, and rewriting
/// into one it also rejects would only obscure the error it is about to raise.
pub(super) fn reencode_images(blocks: &mut [PromptInputBlock], caps: &PromptCapabilitiesInfo) {
    for (index, block) in blocks.iter_mut().enumerate() {
        match block {
            PromptInputBlock::Image {
                data,
                mime_type,
                uri,
            } if !caps.image && caps.embedded_context => {
                *block = PromptInputBlock::Resource {
                    // A path-less image (a pasted screenshot) needs some stable
                    // identifier; its position in this prompt is one.
                    uri: uri
                        .clone()
                        .unwrap_or_else(|| format!("clipboard://work-task-image-{index}")),
                    mime_type: Some(mime_type.clone()),
                    text: None,
                    blob: Some(std::mem::take(data)),
                };
            }
            PromptInputBlock::Resource {
                uri,
                mime_type,
                text: None,
                blob: Some(blob),
            } if caps.image
                && !caps.embedded_context
                && mime_type
                    .as_deref()
                    .is_some_and(|m| m.starts_with("image/")) =>
            {
                *block = PromptInputBlock::Image {
                    data: std::mem::take(blob),
                    mime_type: mime_type.clone().unwrap_or_default(),
                    uri: Some(uri.clone()),
                };
            }
            _ => {}
        }
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// A restart note reaches the agent as context, not as the task itself.
pub(super) fn restart_note_block(note: &str) -> PromptInputBlock {
    PromptInputBlock::Text {
        text: format!(
            "The user restarted this task and left this note — take it into account:\n\n{note}"
        ),
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// One-shot sink for the generation a launch actually operated on. The launch
/// fills it as soon as it has read the row; `spawn_launch` reads it afterwards
/// so a setup failure is attributed to the right generation.
#[derive(Default)]
pub(super) struct LaunchSeq(std::sync::Mutex<Option<i32>>);

impl LaunchSeq {
    // Poison-tolerant, matching the locks in `office_watch` and
    // `background_watch`. The guarded value is a plain `Option<i32>` that
    // cannot be left half-written, so a panic elsewhere in the launch has
    // nothing to corrupt here, while `expect` would turn that unrelated panic
    // into a permanent failure of every later launch. The schedule tick reaches
    // this with nobody at the keyboard.
    fn set(&self, run_seq: i32) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(run_seq);
    }

    fn get(&self) -> Option<i32> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// One-shot "this generation is live" report from a launch to whoever asked
/// for it — today the merge click, which must not sit on the rest of the run.
///
/// Fire-once by construction, and callable from either side of the launch: the
/// launch itself fires `Ok` the moment the agent is up and the row carries its
/// coordinates, while the task that drives it fires the launch's error for
/// every way of failing before that point. Whichever comes first wins; the
/// other is a no-op, so a failure AFTER a successful dispatch stays on the card
/// instead of being reported to a caller that has already been told the merge
/// started.
pub(super) struct DispatchSignal(std::sync::Mutex<Option<oneshot::Sender<Result<(), String>>>>);

impl DispatchSignal {
    pub(super) fn new() -> (Self, oneshot::Receiver<Result<(), String>>) {
        let (tx, rx) = oneshot::channel();
        (Self(std::sync::Mutex::new(Some(tx))), rx)
    }

    pub(super) fn fire(&self, result: Result<(), String>) {
        // Poison-tolerant for the same reason as `LaunchSeq`: the fire-once
        // guarantee is the `take()`, not the lock's poison flag, and refusing
        // to fire after an unrelated panic would leave the caller waiting on a
        // oneshot that is never sent.
        let sender = self.0.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Ownership of a task's in-flight launch slot: which folder it counts
/// against, plus a token so only the launch that took the slot can release it.
pub(super) struct LaunchOwner {
    pub(super) folder_id: i32,
    pub(super) token: u64,
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// A task's setup (init command) kill slot, open for the whole preparing phase.
///
/// The slot deliberately does NOT hold the child's pid. Only the task that
/// spawned the child ever kills it, so a pid can never be signalled after it
/// has been reaped (and possibly recycled by the OS onto an unrelated
/// process). A canceller just raises `kill_requested` and rings `wake`.
pub(super) struct SetupChild {
    /// A cancel arrived: refuse to start an init command, and kill a live one.
    pub(super) kill_requested: bool,
    /// Generation that owns the slot — a stale cancel must not stop a newer run.
    pub(super) run_seq: i32,
    /// Rung by the canceller, awaited by whoever is running the child.
    pub(super) wake: Arc<Notify>,
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Marker file (in the worktree's PRIVATE git dir, so it can never show up in
/// `git status` or be committed) recording that the folder's init command
/// completed successfully in this worktree.
pub(super) const SETUP_MARKER: &str = "codeg-task-init-ok";

pub(super) async fn setup_marker_present(wt_path: &str) -> bool {
    match task_git::git_dir(wt_path).await {
        Ok(dir) => dir.join(SETUP_MARKER).exists(),
        // Can't resolve the git dir → assume not initialized. Re-running an
        // init command is wasteful at worst; skipping one is a broken tree.
        Err(_) => false,
    }
}

pub(super) async fn write_setup_marker(wt_path: &str) {
    match task_git::git_dir(wt_path).await {
        Ok(dir) => {
            if let Err(e) = tokio::fs::write(dir.join(SETUP_MARKER), b"").await {
                tracing::warn!("[work_task] could not write the setup marker: {e}");
            }
        }
        Err(e) => tracing::warn!("[work_task] could not resolve the worktree git dir: {e}"),
    }
}

/// Best-effort kill of a child's whole process tree. Killing only the direct
/// child is not enough: the init command runs as `sh -c "<line>"`, and the real
/// work (`pnpm install` and its downloads) happens in descendants that would
/// otherwise keep running. Deliberately not `kill_on_drop`, which SIGKILLs the
/// shell first and reparents the descendants out of reach — same rationale as
/// `commands::office_tools::stream_install_or_kill_tree`.
pub(super) async fn kill_process_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    if let Err(e) = kill_tree::tokio::kill_tree(pid).await {
        tracing::warn!("[work_task] kill_tree failed for setup pid {pid}: {e}");
    }
}

pub(super) async fn still_expected(
    conn: &sea_orm::DatabaseConnection,
    task_id: i32,
    run_seq: i32,
    expected: WorkTaskStatus,
) -> bool {
    matches!(
        work_task_service::get_model(conn, task_id).await,
        Ok(t) if t.status == expected && t.run_seq == run_seq
    )
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Head of the first text block of a prompt — the transcript viewer matches
/// user turns against it to place round dividers.
pub(super) fn prompt_head(blocks: &[PromptInputBlock]) -> String {
    blocks
        .iter()
        .find_map(|b| match b {
            PromptInputBlock::Text { text } => Some(first_chars(text.trim(), 160)),
            _ => None,
        })
        .unwrap_or_default()
}

/// Run one shell command line in `cwd`, capturing combined output. stdin is
/// null (a command waiting on input sees EOF); no timeout by design — the
/// result write is generation-guarded, so a runaway command can only waste its
/// own process. Returns (exit code, trailing output capped to
/// `PREFLIGHT_TAIL_CHARS`).
pub(super) async fn run_shell_capture(line: &str, cwd: &str) -> Result<(Option<i32>, String), String> {
    let out = shell_command(line, cwd)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(combine_capture(&out))
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The platform's shell invocation for one command line, ready to `output()` or
/// `spawn()`. stdin is null so a command waiting on input sees EOF.
pub(super) fn shell_command(line: &str, cwd: &str) -> tokio::process::Command {
    #[cfg(not(windows))]
    let mut command = {
        let mut c = crate::process::tokio_command("/bin/sh");
        c.arg("-c").arg(line);
        c
    };
    #[cfg(windows)]
    let mut command = {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let mut c = crate::process::tokio_command(comspec);
        c.arg("/C").arg(line);
        c
    };
    command.current_dir(cwd);
    command.stdin(std::process::Stdio::null());
    command
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// (exit code, trailing combined output) of a finished shell capture.
pub(super) fn combine_capture(out: &std::process::Output) -> (Option<i32>, String) {
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (
        out.status.code(),
        tail_chars(&combined, PREFLIGHT_TAIL_CHARS),
    )
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
pub(super) fn tail_chars(s: &str, n: usize) -> String {
    let trimmed = s.trim_end();
    let count = trimmed.chars().count();
    if count <= n {
        return trimmed.to_string();
    }
    trimmed.chars().skip(count - n).collect()
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
pub(super) fn parse_agent_type(s: &str) -> Result<AgentType, String> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|_| format!("unknown agent type: {s}"))
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
pub(super) fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
