use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：deliver
impl TaskEngine {
    /// Accept a reviewed forge-sourced task by DELIVERING it: push the work
    /// branch to the repository the work belongs in — the issue's own
    /// repository, or for a pull-request task the repository its head branch
    /// lives in (a fork, when it came from one) — then adopt or open the pull
    /// request that carries it. The third way to `done`, alongside a local
    /// merge and an acceptance with nothing to land.
    ///
    /// Unlike a merge this needs no agent — there are no conflicts to resolve,
    /// just a push and two REST calls — so the engine runs it itself and
    /// settles from the forge's answer instead of from an agent's word.
    ///
    /// Every precondition is checked BEFORE the review→merging CAS, so a
    /// refused delivery leaves the task exactly as it was. After the CAS, any
    /// failure returns the task to review with the reason on the card; every
    /// step is idempotent, so the retry is just another click.
    ///
    /// `delete_worktree` is the same offer the other two acceptances make, and
    /// it rides on the delivery rather than gating it — see
    /// [`Self::remove_delivered_worktree`].
    pub async fn deliver_pr(
        self: &Arc<Self>,
        task_id: i32,
        pr_title: Option<String>,
        draft: bool,
        delete_worktree: bool,
    ) -> Result<String, String> {
        let task = work_task_service::get_model(&self.db.conn, task_id)
            .await
            .map_err(|e| e.to_string())?;
        if task.status != WorkTaskStatus::Review {
            return Err("task is not in review".to_string());
        }
        // The gate lives HERE, not in the UI: this command is reachable by id
        // from an old frontend or a direct web API call.
        let from_pull = match task.source_kind.as_deref() {
            Some(SOURCE_KIND_ISSUE) => false,
            Some(SOURCE_KIND_PR) => true,
            _ => {
                return Err(
                    "only tasks triggered from a forge issue or pull request can be delivered"
                        .to_string(),
                )
            }
        };
        let meta = task
            .source_meta
            .as_deref()
            .and_then(|s| serde_json::from_str::<ForgeSourceMeta>(s).ok())
            .ok_or_else(|| "the task's source information is unreadable".to_string())?;

        let (_root, wt, base_branch, work_branch) = self.merge_coordinates(&task).await?;
        // Where the push lands. An issue's task publishes its own branch; a
        // pull request's task pushes back to the branch that pull request
        // already tracks, so its author sees the work in the review they
        // opened rather than in a second one.
        let remote_branch = if from_pull {
            let head_ref = meta
                .head_ref
                .clone()
                .filter(|r| !r.trim().is_empty())
                .ok_or_else(|| {
                    "this pull request task does not know which branch to push back to — \
                     trigger it again"
                        .to_string()
                })?;
            // The push lands in the recorded HEAD repository — the fork, when
            // the pull request comes from one. Resolvability is re-checked
            // here, before the CAS, so a row whose fork codeg cannot name
            // (written by an older build, or hydrated while the fork was
            // already gone) is refused with the task left exactly as it was.
            pull_push_repo(&meta)?;
            head_ref
        } else {
            work_branch.clone()
        };
        // A FAILING clean check, deliberately not `worktree_holds_uncommitted`:
        // that one reads a git error as "clean" because its caller (worktree
        // removal) is tolerant of a worktree already off disk. Publishing a
        // branch on the strength of a question git refused to answer is a very
        // different bet.
        match task_git::has_changes(&wt.path).await {
            Ok(false) => {}
            Ok(true) => {
                return Err("the task worktree still has uncommitted changes — send it back for \
                            one more round so the agent commits them, then deliver"
                    .to_string())
            }
            Err(e) => return Err(format!("could not read the task worktree's state: {e}")),
        }
        // GitHub answers an empty pull request with a 422, and a push back that
        // carries nothing leaves the pull request untouched while the task
        // records a delivery. Refuse before the CAS and point at the button that
        // actually applies — the likeliest task here is a review-only one,
        // whose worktree holds the pull request and nothing else.
        if !self.has_landable_changes(&task, &wt.path).await? {
            return Err(if from_pull {
                "this task added nothing to the pull request — complete it instead of pushing back"
            } else {
                "this task has nothing to deliver — complete it instead of opening a pull request"
            }
            .to_string());
        }
        // The pull request is diffed against the base branch AS THE REMOTE HAS
        // IT, not against the local one the task branched from. If those have
        // drifted apart — unpushed commits on the local base — the pull request
        // would carry work the review never showed, and merging it would land
        // that work too.
        //
        // This gate FAILS CLOSED. Being unable to read the remote base is not
        // reassurance: a base branch that does not exist there fails the same
        // way, and the push that follows would still publish the branch (it is
        // only the pull request that needs the base to exist), so nothing
        // downstream re-asks this question. Refusing costs a retry; guessing
        // costs published work nobody reviewed.
        let ctx = DeliveryCtx {
            conn: &self.db.conn,
            data_dir: &self.data_dir,
            provider: meta.provider,
            server_host: &meta.server_host,
            account_id: &meta.account_id,
            owner_repo: &meta.owner_repo,
        };
        if let Some(base_sha) = task.base_sha.as_deref() {
            let Some(remote_tip) = self
                .forge
                .remote_base_tip(&ctx, &wt.path, &base_branch)
                .await
            else {
                return Err(format!(
                    "could not read '{base_branch}' from the source repository, so there is no \
                     way to tell whether this task's starting point is published — check that \
                     the branch exists there and that you are online, then deliver"
                ));
            };
            if !task_git::is_ancestor(&wt.path, base_sha, &remote_tip)
                .await
                .unwrap_or(false)
            {
                return Err(format!(
                    "'{base_branch}' has commits here that are not on the remote yet, so the \
                     pull request would carry more than this task's own work — push \
                     '{base_branch}' first, then deliver"
                ));
            }
        }

        let expected_head = task_git::rev_parse(&wt.path, &work_branch)
            .await
            .map_err(|e| format!("could not resolve the task branch: {e}"))?;
        let title = pr_title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| task.title.clone());

        let state = WorkTaskMergeState {
            op: WorkTaskMergeOp::DeliverPr,
            remote_branch: Some(remote_branch.clone()),
            expected_head: Some(expected_head.clone()),
            pr_title: Some(title.clone()),
            draft,
            ..Default::default()
        };

        // Ownership BEFORE the CAS and held across the whole delivery — not
        // just the dispatch, the way a merge does it. A merge hands off to its
        // agent session and `recover_merging` then sees a live connection; a
        // delivery has no session at all, so this claim is the only thing
        // telling the reconcile tick that the row's `merging` is alive and not
        // orphaned. It doubles as mutual exclusion: two clients clicking
        // deliver on the same task must not both run the push.
        let Some(token) = self.claim_in_flight(task_id).await else {
            return Err(
                "this task is already merging — wait for the operation in flight".to_string(),
            );
        };
        let outcome = self
            .deliver_guarded(
                &task,
                &meta,
                &wt.path,
                &base_branch,
                &work_branch,
                &remote_branch,
                from_pull,
                &state,
            )
            .await;
        // Only after the delivery SUCCEEDED: one that bounced back to review
        // still needs its checkout for the retry. Still inside the claim, so a
        // second click cannot start a delivery on a task whose worktree is
        // half removed. `expected_head` is the OID the push published, and the
        // cleanup is allowed to destroy the branch only while it is still that.
        if outcome.is_ok() && delete_worktree {
            self.remove_delivered_worktree(task_id, task.folder_id, &expected_head)
                .await;
        }
        self.release_in_flight(task_id, token).await;
        self.emit_upsert(task_id);
        outcome
    }

    /// Take a delivered task's worktree with it, the way the merge and the
    /// no-merge acceptances do. Runs AFTER the settle, on a task that is
    /// already `done`, and deliberately returns nothing: the delivery is what
    /// the button promised and it has happened — a removal that fails flags
    /// `cleanup_state` and stays retryable from the card, which must not
    /// re-read as "the push failed".
    ///
    /// TWO probes, because each is blind to exactly what the other sees — the
    /// pairing `complete_task` documents, aimed at what a DELIVERY publishes
    /// rather than at what a merge lands:
    ///
    /// - `deliver_pr` refused a dirty worktree before the CAS, but that was
    ///   before the push and the REST calls. `worktree remove --force` drops
    ///   whatever appeared since without a word, and a file that was never
    ///   committed is on no forge at all;
    /// - a COMMIT made in that same window leaves `git status` spotless and is
    ///   just as unpublished — the delivery pushed `published_head`, not it. So
    ///   the branch tip is checked against the OID that actually reached the
    ///   forge, which is the only thing that ever made `branch -D` safe here.
    ///   `has_landable_changes` cannot serve as that probe: it is true of every
    ///   delivery by construction.
    ///
    /// Either probe keeps the worktree, says why, and leaves the retry on the
    /// card. The tip then rides along to the removal itself, so the read above
    /// and the delete cannot straddle a ref that moves in between.
    ///
    /// Takes the folder git lock, so no caller may already hold it.
    async fn remove_delivered_worktree(
        self: &Arc<Self>,
        task_id: i32,
        folder_id: i32,
        published_head: &str,
    ) {
        let lock = self.folder_lock(folder_id).await;
        let _guard = lock.lock().await;
        let Ok(task) = work_task_service::get_model(&self.db.conn, task_id).await else {
            return;
        };
        if let Some(reason) = self.delivered_worktree_keep_reason(&task, published_head).await {
            let _ =
                work_task_service::set_cleanup_state(&self.db.conn, task_id, true, Some(reason))
                    .await;
            self.emit_upsert(task_id);
            return;
        }
        // Broadcast omitted: the removal announces its own outcome.
        self.remove_worktree_locked(task_id, Some(published_head)).await;
    }

    /// Why a delivered task's checkout must outlive the cleanup its user asked
    /// for, or `None` when nothing objects. See
    /// [`Self::remove_delivered_worktree`] for what the two answers protect.
    ///
    /// The tip half FAILS CLOSED: a branch git declines to resolve is not a
    /// branch anyone may `-D`. The `None`s above it are the opposite case and
    /// deliberately so — no worktree recorded, no folder row, no work branch
    /// means there is nothing here for the removal to destroy, and it already
    /// handles each of those itself.
    async fn delivered_worktree_keep_reason(
        &self,
        task: &crate::db::entities::work_task::Model,
        published_head: &str,
    ) -> Option<String> {
        let wt_id = task.worktree_folder_id?;
        let wt = get_folder_core(&self.db, wt_id).await.ok()?;
        if path_holds_uncommitted(&wt.path).await {
            return Some(worktree_kept(RETRY_THE_CLEANUP));
        }
        let branch = task.work_branch.as_deref()?;
        match task_git::rev_parse(&wt.path, branch).await {
            Ok(tip) if tip.eq_ignore_ascii_case(published_head) => None,
            Ok(_) => Some(format!(
                "the worktree was kept: '{branch}' moved on after the delivery, so it holds \
                 commits that were never pushed. Deliver again to publish them, or remove the \
                 worktree by hand once they are safe."
            )),
            Err(e) => Some(format!(
                "the worktree was kept: '{branch}' could not be read, so there is no telling \
                 whether it still holds only the delivered work ({e})"
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn deliver_guarded(
        self: &Arc<Self>,
        task: &crate::db::entities::work_task::Model,
        meta: &ForgeSourceMeta,
        wt_path: &str,
        base_branch: &str,
        work_branch: &str,
        remote_branch: &str,
        from_pull: bool,
        state: &WorkTaskMergeState,
    ) -> Result<String, String> {
        let task_id = task.id;
        let run_seq =
            match work_task_service::begin_delivery(&self.db.conn, task_id, state, task.run_seq)
                .await
            {
                Err(e) => return Err(e.to_string()),
                Ok(None) => return Err("task left review before the delivery began".to_string()),
                Ok(Some(seq)) => seq,
            };
        self.emit_upsert(task_id);

        let expected_head = state.expected_head.clone().unwrap_or_default();
        let title = state.pr_title.clone().unwrap_or_else(|| task.title.clone());
        let attempt = if from_pull {
            self.push_back_to_pull(
                task_id,
                meta,
                wt_path,
                work_branch,
                remote_branch,
                &expected_head,
            )
            .await
        } else {
            self.push_and_open(
                task_id,
                meta,
                wt_path,
                base_branch,
                work_branch,
                &expected_head,
                &title,
                state.draft,
            )
            .await
        };
        match attempt {
            Ok(pr) => match self.settle_delivery(task_id, meta, run_seq, &pr).await {
                Ok(url) => Ok(url),
                Err(e) => {
                    self.back_to_review(task_id, e.clone(), None).await;
                    Err(e)
                }
            },
            Err(e) => {
                self.back_to_review(task_id, e.clone(), None).await;
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn push_and_open(
        &self,
        task_id: i32,
        meta: &ForgeSourceMeta,
        wt_path: &str,
        base_branch: &str,
        work_branch: &str,
        expected_head: &str,
        title: &str,
        draft: bool,
    ) -> Result<ForgePr, String> {
        let ctx = DeliveryCtx {
            conn: &self.db.conn,
            data_dir: &self.data_dir,
            provider: meta.provider,
            server_host: &meta.server_host,
            account_id: &meta.account_id,
            owner_repo: &meta.owner_repo,
        };
        // Fast-forward push of the same commits is a no-op, so a retry after a
        // later step failed costs nothing and changes nothing.
        self.forge
            .push_branch(&ctx, wt_path, &meta.owner_repo, work_branch, work_branch)
            .await
            .map_err(|e| format!("could not push the task branch: {e}"))?;

        // Look before creating: the previous attempt may have opened the pull
        // request and died on the way to settling it.
        let existing = self
            .forge
            .find_pulls(&ctx, work_branch)
            .await
            .map_err(|e| format!("could not check for an existing pull request: {e}"))?;
        match adopt_pull_request(
            existing,
            expected_head,
            work_branch,
            base_branch,
            &meta.owner_repo,
        ) {
            PrAdoption::Merged(pr) | PrAdoption::Open(pr) => Ok(pr),
            PrAdoption::ClosedUnmerged(pr) => Err(format!(
                "pull request #{} for this branch was closed without merging — reopen it, or \
                 send the task back for another round to deliver a new commit",
                pr.number
            )),
            // Someone moved the branch out from under us. Creating is not an
            // option (GitHub allows one open pull request per head/base pair),
            // and adopting would settle this task against a commit it never
            // produced.
            PrAdoption::StaleHead(pr) => Err(format!(
                "pull request #{} already covers this branch but points at a different commit — \
                 someone else pushed to it. Check that branch before delivering again",
                pr.number
            )),
            PrAdoption::NoMatch => {
                let body = pull_request_body(&meta.url, meta.number, task_id);
                self.forge
                    .create_pull(
                        &ctx,
                        &NewPullRequest {
                            title,
                            head: work_branch,
                            base: base_branch,
                            body: &body,
                            draft,
                        },
                    )
                    .await
                    .map_err(|e| format!("could not open the pull request: {e}"))
            }
        }
    }

    /// Deliver a task that IS a pull request: push the work back onto that
    /// pull request's own branch. Nothing is created — the pull request the
    /// task came from is the delivery.
    ///
    /// The push is fast-forward only (as everywhere here), and that is what
    /// makes the check afterwards a lookup rather than a race: a push that
    /// succeeded proves our commits are on that branch, so the pull request is
    /// re-read only to confirm it is still the open pull request for it. Its
    /// head OID is deliberately NOT required to equal ours — someone pushing
    /// on top of our work a second later has not undone the delivery, and
    /// demanding equality would bounce a task whose work is safely published.
    async fn push_back_to_pull(
        &self,
        task_id: i32,
        meta: &ForgeSourceMeta,
        wt_path: &str,
        work_branch: &str,
        remote_branch: &str,
        expected_head: &str,
    ) -> Result<ForgePr, String> {
        let ctx = DeliveryCtx {
            conn: &self.db.conn,
            data_dir: &self.data_dir,
            provider: meta.provider,
            server_host: &meta.server_host,
            account_id: &meta.account_id,
            owner_repo: &meta.owner_repo,
        };
        // Read BEFORE pushing: a pull request that is already closed or merged
        // must not receive commits nobody is going to look at.
        let before = self
            .forge
            .get_pull(&ctx, meta.number)
            .await
            .map_err(|e| format!("could not read pull request #{}: {e}", meta.number))?;
        self.check_pull_target(&before, meta, remote_branch)?;

        let pushed = !before.head_sha.eq_ignore_ascii_case(expected_head);
        let after = if !pushed {
            // The pull request already sits at the commit this task would
            // push: a retry of a delivery whose push landed before something
            // interrupted the settle. There is nothing left to publish, so
            // nothing is pushed — which is also what lets such a retry finish
            // on a fork the account cannot write to. A task with nothing of
            // its own never reaches this branch; `has_landable_changes`
            // refuses it in favour of completing. It is why the closed/merged
            // refusal below does not run here, too: a merged pull request that
            // carries this exact head IS the delivery, and `settle_pull_target`
            // says so.
            before.clone()
        } else {
            if before.merged || before.state != "open" {
                return Err(format!(
                    "pull request #{} is no longer open — reopen it, then deliver",
                    meta.number
                ));
            }
            let push_repo = pull_push_repo(meta)?;
            self.forge
                .push_branch(&ctx, wt_path, &push_repo, work_branch, remote_branch)
                .await
                .map_err(|e| {
                    let own_repo = crate::forge::same_repo(&push_repo, &meta.owner_repo);
                    let where_ = if own_repo {
                        format!("could not push back to '{remote_branch}'")
                    } else {
                        format!("could not push back to '{remote_branch}' on {push_repo}")
                    };
                    // A hint is only added when git actually said something we
                    // recognise. The fork case used to carry the permission
                    // hint unconditionally, which reads as a finding rather
                    // than the guess it was: a push refused for being out of
                    // date came back advising the reader to go change a
                    // setting on the pull request that was already correct.
                    match classify_push_refusal(&e) {
                        PushRefusal::Permission if !own_repo => format!(
                            "{where_}: {e} — pushing to a fork needs its author to allow edits \
                             from maintainers on the {}",
                            meta.provider.change_noun()
                        ),
                        PushRefusal::BranchMoved => format!(
                            "{where_}: {e} — that branch has commits this task does not have, so \
                             the push is not a fast-forward. Bring them into the task's branch, \
                             then deliver again"
                        ),
                        _ => format!("{where_}: {e}"),
                    }
                })?;

            // Re-read so the settled row links the pull request as it is now
            // (and so a pull request closed during the push is not reported
            // as open).
            self.forge.get_pull(&ctx, meta.number).await.map_err(|e| {
                format!(
                    "the work was pushed to '{remote_branch}', but pull request #{} could not be \
                     read back: {e}",
                    meta.number
                )
            })?
        };
        self.settle_pull_target(
            task_id,
            &ctx,
            wt_path,
            &after,
            meta,
            remote_branch,
            expected_head,
            pushed,
        )
        .await?;
        Ok(after)
    }

    /// The pull request a push-back may push INTO: same repository, same head
    /// branch as the task recorded. Anything else means the pull request was
    /// retargeted under us, and the task goes back to a human.
    fn check_pull_target(
        &self,
        pr: &ForgePr,
        meta: &ForgeSourceMeta,
        remote_branch: &str,
    ) -> Result<(), String> {
        let noun = meta.provider.change_noun();
        // Compared against the head repository RECORDED at trigger time (the
        // fork, when the pull request comes from one — rows without one are
        // same-repo by construction), not against the source repository: a
        // fork's pull request legitimately lives elsewhere, and the thing
        // being caught here is the head moving since the task was made.
        let recorded_repo = meta
            .head_repo
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(&meta.owner_repo);
        if !crate::forge::same_repo(&pr.head_repo, recorded_repo) {
            return Err(format!(
                "{noun} #{} now comes from {}, not {recorded_repo} — check it before delivering \
                 again",
                meta.number, pr.head_repo
            ));
        }
        if pr.head_ref != remote_branch {
            return Err(format!(
                "{noun} #{} now tracks branch '{}', not '{remote_branch}' — check it before \
                 delivering again",
                meta.number, pr.head_ref
            ));
        }
        Ok(())
    }

    /// The pull request a push-back may SETTLE against — a stricter question
    /// than where it may push, and asked after any pushing is behind us.
    ///
    /// Everything [`check_pull_target`] asks, plus two things that can change
    /// underneath a push and would otherwise be recorded as a delivery:
    ///
    /// - the BASE must still be the one the task was reviewed against. A
    ///   retargeted pull request shows a different diff than the one a human
    ///   approved, so calling that "delivered" would put this task's name on a
    ///   review nobody did.
    /// - it must still be OPEN, or MERGED. Merged is a success — our commits
    ///   landed, which is exactly what delivery means. Closed-without-merging
    ///   is not: the work sits on a branch whose review someone shut, and the
    ///   card must say so rather than claim it is done.
    ///
    /// `pushed` is only wording: whether this attempt actually published
    /// anything ("the work was pushed, but…") or found the head already there
    /// and skipped the push — an error must not claim a push that never ran.
    #[allow(clippy::too_many_arguments)]
    async fn settle_pull_target(
        &self,
        task_id: i32,
        ctx: &DeliveryCtx<'_>,
        repo_path: &str,
        pr: &ForgePr,
        meta: &ForgeSourceMeta,
        remote_branch: &str,
        expected_head: &str,
        pushed: bool,
    ) -> Result<(), String> {
        self.check_pull_target(pr, meta, remote_branch)?;
        let noun = meta.provider.change_noun();
        let done = if pushed {
            format!("the work was pushed to '{remote_branch}'")
        } else {
            format!("'{remote_branch}' already holds this task's work")
        };
        if let Some(base) = meta.base_ref.as_deref() {
            if pr.base_ref != base {
                return Err(format!(
                    "{done}, but {noun} #{} now targets '{}' instead of '{base}' — it was \
                     retargeted, so check the diff there before delivering again",
                    meta.number, pr.base_ref
                ));
            }
        }
        if pr.merged {
            // Merged is a success only if the merge CONTAINS what we pushed.
            // Both losing orders are real: someone merges the old head while
            // we are pushing (our commits are not in it), and someone
            // fast-forwards on top of our commits and merges that (they are).
            // Equality answers only the second; git answers both.
            if !self
                .merge_carries_our_work(task_id, ctx, repo_path, meta, expected_head, &pr.head_sha)
                .await
            {
                return Err(format!(
                    "{done}, but {noun} #{} was merged at a commit that does not contain it — \
                     this task's work is on that branch and NOT in that merge, so it needs a \
                     new {noun}",
                    meta.number
                ));
            }
            return Ok(());
        }
        if pr.state != "open" {
            return Err(format!(
                "{done}, but {noun} #{} was closed without merging — reopen it, then deliver",
                meta.number
            ));
        }
        Ok(())
    }

    /// Whether the merge that closed this change carries the commit we pushed.
    ///
    /// Equality is the ordinary answer and costs nothing. When the heads
    /// differ, only git can tell "someone fast-forwarded on top of our work
    /// and merged that" (a delivery) from "someone merged the old head while
    /// we were pushing" (not one) — so the change's server-side head ref is
    /// fetched and asked. Anything that cannot be PROVEN is a no: a delivery
    /// we cannot show is not one this may record.
    async fn merge_carries_our_work(
        &self,
        task_id: i32,
        ctx: &DeliveryCtx<'_>,
        repo_path: &str,
        meta: &ForgeSourceMeta,
        expected_head: &str,
        merged_head: &str,
    ) -> bool {
        // No anchor, nothing to prove. (A delivery always records one; this is
        // the "stored state was unreadable" case, and it must not turn into a
        // ref name with an empty component either.)
        if expected_head.trim().is_empty() {
            return false;
        }
        if merged_head.eq_ignore_ascii_case(expected_head) {
            return true;
        }
        // Scoped by TASK, like the other scratch refs this engine writes.
        // Sibling deliveries in one folder do not share a lock and can overlap,
        // and two of them may legitimately be about the same item or even the
        // same commit — a shared name would let one force-update or delete the
        // ref between another's fetch and its ancestry check.
        let probe = format!("refs/codeg/task-{task_id}/merged-probe");
        let fetched = self
            .forge
            .fetch_ref(
                ctx,
                repo_path,
                &meta.provider.change_head_ref(meta.number),
                &probe,
            )
            .await;
        let carried = match fetched {
            Ok(_) => task_git::is_ancestor(repo_path, expected_head, &probe)
                .await
                .unwrap_or(false),
            Err(e) => {
                tracing::info!("[forge] could not check what the merge carries: {e}");
                false
            }
        };
        task_git::delete_ref(repo_path, &probe).await;
        carried
    }

    /// merging → done with the pull request recorded on the row. The worktree
    /// is deliberately KEPT: the pull request is open against this task's
    /// branch, and another review round on it is a normal next step. Cleanup
    /// stays where it already is — the user's, on a finished card.
    async fn settle_delivery(
        self: &Arc<Self>,
        task_id: i32,
        meta: &ForgeSourceMeta,
        run_seq: i32,
        pr: &ForgePr,
    ) -> Result<String, String> {
        let mut meta = meta.clone();
        meta.result_pr = Some(pr.html_url.clone());
        // Serialized out here on purpose — see `complete_delivered`: its
        // transaction must open with a write.
        let meta_json = serde_json::to_string(&meta)
            .map_err(|e| format!("could not record the pull request: {e}"))?;
        let settled = work_task_service::complete_delivered(
            &self.db.conn,
            task_id,
            run_seq,
            &pr.html_url,
            &meta_json,
        )
        .await
        .map_err(|e| e.to_string())?;
        if !settled {
            return Err(format!(
                "the task moved on while pull request {} was being opened — it is open and \
                 unchanged, nothing was lost",
                pr.html_url
            ));
        }
        self.emit_upsert(task_id);
        // Behind the same CAS as the settle, so the retry of a delivery that
        // already finished cannot comment twice.
        self.spawn_forge_writeback(task_id, WritebackOutcome::Delivered(pr.html_url.clone()));
        Ok(pr.html_url.clone())
    }

    // ── write the outcome back to the forge (best-effort) ───────────────────

    /// Tell the thread a task came from that it finished — a comment carrying
    /// the outcome, the link and the diff counters, and nothing else.
    ///
    /// SPAWNED, never awaited by the settle that triggers it. A local merge
    /// settles inside the folder's git lock, and a REST call has no business
    /// holding that lock; a delivery settles while the user waits on the
    /// button. The price is honest best-effort: a process that exits right
    /// after a settle can leave neither a comment nor a failure event, and
    /// nothing retries. Guaranteed delivery needs a persisted outbox, which is
    /// deliberately not this.
    pub(super) fn spawn_forge_writeback(self: &Arc<Self>, task_id: i32, outcome: WritebackOutcome) {
        let engine = self.clone();
        tokio::spawn(async move {
            engine.forge_writeback(task_id, outcome).await;
        });
    }

    pub(super) async fn forge_writeback(self: &Arc<Self>, task_id: i32, outcome: WritebackOutcome) {
        let Ok(task) = work_task_service::get_model(&self.db.conn, task_id).await else {
            return;
        };
        // Only a task that HAS a thread, and only a provider we can post to.
        // Both kinds get a comment; WHERE it goes differs by forge, which is
        // why the item kind is carried rather than assumed (GitHub serves
        // issue and pull-request comments from one endpoint, GitLab from two).
        let item_kind = match task.source_kind.as_deref() {
            Some(SOURCE_KIND_ISSUE) => ForgeItemKind::Issue,
            Some(SOURCE_KIND_PR) => ForgeItemKind::Change,
            _ => return,
        };
        let Some(meta) = task
            .source_meta
            .as_deref()
            .and_then(|s| serde_json::from_str::<ForgeSourceMeta>(s).ok())
        else {
            return;
        };
        // The answer the user gave in the trigger dialog, carried on the task
        // itself. Absent on rows minted before the choice lived there — those
        // stay silent, which is the posture the folder setting it replaced
        // shipped with. This is a write to a place other people are watching,
        // so "no recorded yes" means no.
        if !meta.writeback.unwrap_or(false) {
            return;
        }

        // The counters recorded when the run settled — reading git here would
        // fail on exactly the tasks whose worktree the merge just deleted.
        let stats = task
            .files_changed
            .map(|files| (files, task.additions.unwrap_or(0), task.deletions.unwrap_or(0)));
        let base_branch = task.base_branch.clone().unwrap_or_default();
        let outcome = match &outcome {
            WritebackOutcome::Merged(commit) => TaskOutcome::Merged {
                commit,
                base_branch: &base_branch,
            },
            WritebackOutcome::Delivered(pr_url) => TaskOutcome::Delivered { pr_url },
            WritebackOutcome::Accepted { nothing_to_land } => TaskOutcome::Accepted {
                nothing_to_land: *nothing_to_land,
            },
        };
        // Where it is posted shapes what it may say: a pull request's thread
        // hears what happened to that pull request, in its forge's own noun.
        let body = writeback_comment_body(task_id, meta.provider, item_kind, &outcome, stats);

        let ctx = DeliveryCtx {
            conn: &self.db.conn,
            data_dir: &self.data_dir,
            provider: meta.provider,
            server_host: &meta.server_host,
            account_id: &meta.account_id,
            owner_repo: &meta.owner_repo,
        };
        let (kind, payload) = match self
            .forge
            .comment_issue(&ctx, item_kind, meta.number, &body)
            .await
        {
            Ok(url) => ("forge_writeback", serde_json::json!({ "url": url })),
            // Failure is recorded and dropped: the task is finished either way,
            // and a comment nobody sees must not reopen a settled row.
            Err(error) => (
                "forge_writeback_failed",
                serde_json::json!({ "error": error, "number": meta.number }),
            ),
        };
        let _ =
            work_task_service::record_event(&self.db.conn, task_id, kind, "engine", Some(payload))
                .await;
        // The card's own fields did not change; this is what makes an open
        // detail sheet reload its timeline.
        self.emit_upsert(task_id);
    }

    /// Crash recovery for a delivery: the forge is the only truth about what
    /// the dead process managed to do. No `ls-remote` probe — the four-way
    /// match already requires the pushed OID to be the pull request's head, so
    /// a separate branch-tip read adds a failure mode and no information.
    ///
    /// Every exit binds to the generation this pass READ, never just to
    /// "the row is merging": a recovery that spent seconds at the forge must
    /// not bounce a delivery someone started in the meantime.
    pub(super) async fn recover_delivery(
        self: &Arc<Self>,
        task: &crate::db::entities::work_task::Model,
        state: &WorkTaskMergeState,
    ) {
        let task_id = task.id;
        let (Some(remote_branch), Some(expected_head)) =
            (state.remote_branch.as_deref(), state.expected_head.as_deref())
        else {
            self.bounce_delivery(
                task,
                "the delivery was interrupted and its state is incomplete — deliver again"
                    .to_string(),
            )
            .await;
            return;
        };
        let (Some(meta), Some(base_branch)) = (
            task.source_meta
                .as_deref()
                .and_then(|s| serde_json::from_str::<ForgeSourceMeta>(s).ok()),
            task.base_branch.as_deref(),
        ) else {
            self.bounce_delivery(
                task,
                "the delivery was interrupted and the task's source information is unreadable — \
                 deliver again"
                    .to_string(),
            )
            .await;
            return;
        };

        let ctx = DeliveryCtx {
            conn: &self.db.conn,
            data_dir: &self.data_dir,
            provider: meta.provider,
            server_host: &meta.server_host,
            account_id: &meta.account_id,
            owner_repo: &meta.owner_repo,
        };
        // A push-back knows exactly which pull request it was delivering into —
        // the number is part of the task's identity — so recovery looks it up
        // instead of searching by head branch.
        if task.source_kind.as_deref() == Some(SOURCE_KIND_PR) {
            self.recover_push_back(task, &ctx, &meta, remote_branch, expected_head)
                .await;
            return;
        }
        let found = match self.forge.find_pulls(&ctx, remote_branch).await {
            Ok(prs) => prs,
            Err(e) => {
                self.bounce_delivery(
                    task,
                    format!("could not check the pull request after a restart: {e}"),
                )
                .await;
                return;
            }
        };
        // Anything short of all four criteria goes back to a human. Adopting on
        // a partial match would settle the task against someone else's pull
        // request that merely reused the branch name.
        match adopt_pull_request(
            found,
            expected_head,
            remote_branch,
            base_branch,
            &meta.owner_repo,
        ) {
            PrAdoption::Merged(pr) | PrAdoption::Open(pr) => {
                if let Err(e) = self.settle_delivery(task_id, &meta, task.run_seq, &pr).await {
                    self.bounce_delivery(task, e).await;
                }
            }
            PrAdoption::ClosedUnmerged(pr) => {
                self.bounce_delivery(
                    task,
                    format!(
                        "the delivery was interrupted and pull request #{} was closed without \
                         merging — deliver again if you still want it",
                        pr.number
                    ),
                )
                .await;
            }
            PrAdoption::StaleHead(pr) => {
                self.bounce_delivery(
                    task,
                    format!(
                        "the delivery was interrupted and pull request #{} now points at a \
                         different commit — check that branch before delivering again",
                        pr.number
                    ),
                )
                .await;
            }
            PrAdoption::NoMatch => {
                self.bounce_delivery(
                    task,
                    "the delivery was interrupted before its pull request existed — deliver again"
                        .to_string(),
                )
                .await;
            }
        }
    }

    /// Crash recovery for a push-back: did the dead process get the commits
    /// onto the pull request's branch?
    ///
    /// Only one answer settles the task — the pull request's head IS the commit
    /// this delivery pushed. Deliberately stricter than the live path (which
    /// takes a successful push as proof): here there is no proof, and "someone
    /// pushed after us" and "we never pushed" look identical from the outside.
    /// Bouncing costs one click, and the retry is a no-op push followed by the
    /// normal settle.
    async fn recover_push_back(
        self: &Arc<Self>,
        task: &crate::db::entities::work_task::Model,
        ctx: &DeliveryCtx<'_>,
        meta: &ForgeSourceMeta,
        remote_branch: &str,
        expected_head: &str,
    ) {
        let pr = match self.forge.get_pull(ctx, meta.number).await {
            Ok(pr) => pr,
            Err(e) => {
                self.bounce_delivery(
                    task,
                    format!(
                        "could not read pull request #{} after a restart: {e}",
                        meta.number
                    ),
                )
                .await;
                return;
            }
        };
        // The same question the live path asks after its push — with the head
        // OID added, because a crash left no "the push succeeded" evidence and
        // "someone else pushed" is indistinguishable from "we never did".
        // The task's own worktree may be gone after a crash; the project
        // folder's object store is the same one and always there.
        let repo_path = match get_folder_core(&self.db, task.folder_id).await {
            Ok(folder) => folder.path,
            Err(e) => {
                self.bounce_delivery(task, format!("the delivery was interrupted: {e}")).await;
                return;
            }
        };
        if let Err(reason) = self
            .settle_pull_target(
                task.id,
                ctx,
                &repo_path,
                &pr,
                meta,
                remote_branch,
                expected_head,
                // A recovery cannot know whether the interrupted attempt got
                // its push out; claiming one is the conservative reading.
                true,
            )
            .await
        {
            self.bounce_delivery(task, format!("the delivery was interrupted: {reason}")).await;
            return;
        }
        // For a MERGED change the check above already PROVED the merge carries
        // this task's commit, which is strictly more than the head OID says —
        // asking again here would reject the very case that proof exists for
        // (someone fast-forwarded on top of our work and merged that).
        if !pr.merged && !pr.head_sha.eq_ignore_ascii_case(expected_head) {
            self.bounce_delivery(
                task,
                format!(
                    "the delivery was interrupted and {} #{} does not show this task's commit — \
                     deliver again",
                    meta.provider.change_noun(),
                    meta.number
                ),
            )
            .await;
            return;
        }
        if let Err(e) = self.settle_delivery(task.id, meta, task.run_seq, &pr).await {
            self.bounce_delivery(task, e).await;
        }
    }

    // ── merge pump (the folder's one merge slot) ────────────────────────────
}
