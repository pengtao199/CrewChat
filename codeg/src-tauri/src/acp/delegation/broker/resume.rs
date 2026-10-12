// broker submodule: resume (gateway restart re-attach) (split from mod.rs)
use super::*;


/// Cap on the free-form `reason` a `resume_delegation` call may carry into the
/// child's continuation prompt. The reason is interruption CONTEXT, not new
/// instructions — the cap (shared with `TASK_PREVIEW_CAP`'s budget) keeps it
/// from smuggling a task-sized payload past the "no new iterations" contract.
pub(crate) const RESUME_REASON_CAP: usize = 2 * 1024;


/// The wire-stable `error_code` on every refused `resume_delegation` report.
/// Distinct from the task's own status so the caller can tell "this task is
/// completed" (status) from "and that is why it was not resumed" (code).
pub const NOT_RESUMABLE_CODE: &str = "not_resumable";


/// Build the fixed continuation prompt a resumed child receives. Deliberately
/// NOT caller-controlled beyond the bounded `reason`: `resume_delegation`
/// continues the ORIGINAL task and must not be usable as a second
/// `delegate_to_agent` (see the tool schema's contract).
pub fn build_resume_prompt(reason: Option<&str>) -> String {
    let mut prompt = String::from(
        "This delegated task was interrupted before completion and is now being \
         resumed. Re-read the original task from the earlier messages in this \
         session, assess what has already been done (partial work may exist on \
         disk), continue from where it stopped, and finish the ORIGINAL task. \
         Do not take on any new work beyond that original scope.",
    );
    if let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) {
        prompt.push_str("\nContext on why the run was interrupted / resumed: ");
        prompt.push_str(&truncate_on_char_boundary(reason, RESUME_REASON_CAP));
    }
    prompt
}


/// The `Running` ack returned by `resume_delegation` once the child is back up.
/// Same shape as [`running_ack`], resume-flavored first sentence — which must
/// NOT start with `"Delegation successful. task_id="`: Cursor's completion-time
/// result sniff (`cursor_companion_title_from_content`) anchors on that exact
/// prefix to re-title identity-less calls as `delegate_to_agent`.
/// A confirmed resume.
///
/// The opening "Delegation resumed" is load-bearing, like the "Not resumed" of
/// [`not_resumable_report`]: `companion::render_task_report` renders only
/// `message` as content text and keeps the rest in `structuredContent`, which
/// some hosts drop wholesale (OpenCode — see `acp::connection`). There the
/// words are all that separates this from a refusal or an
/// [`unknown_report`], so the card can still tell a real resume from the
/// model naming somebody else's task. `isAffirmedResume` in
/// `src/lib/delegation-card.ts` reads it; keep the two spellings in step.
pub(crate) fn resume_ack(
    call_id: String,
    child_conversation_id: i32,
    agent_type: AgentType,
) -> DelegationTaskReport {
    let message = format!(
        "Delegation resumed. task_id={call_id} (unchanged). Call \
         get_delegation_status with this id in the task_ids array (optionally \
         wait_ms) to collect the result, or cancel_delegation to stop it."
    );
    DelegationTaskReport {
        task_id: Some(call_id),
        status: TaskStatus::Running,
        child_conversation_id: Some(child_conversation_id),
        agent_type: Some(agent_type),
        text: None,
        error_code: None,
        message: Some(message),
        duration_ms: None,
        blocked_on: None,
    }
}


/// A refused resume: reports the task's ACTUAL current status (so the caller
/// learns where the task really stands) with `error_code: "not_resumable"` and
/// a message that opens with "Not resumed" — unambiguous against the ack even
/// on hosts that only surface the content text.
///
/// That prefix is load-bearing, not decorative: `isRefusedResume` in
/// `src/lib/delegation-card.ts` falls back to it wherever `error_code` did not
/// survive, and without it a refusal renders as a resumed sub-agent card. Keep
/// the two spellings in step.
pub(crate) fn not_resumable_report(
    task_id: &str,
    status: TaskStatus,
    child_conversation_id: Option<i32>,
    agent_type: Option<AgentType>,
    why: &str,
) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status,
        child_conversation_id,
        agent_type,
        text: None,
        error_code: Some(NOT_RESUMABLE_CODE.to_string()),
        message: Some(format!("Not resumed: {why}")),
        duration_ms: None,
        blocked_on: None,
    }
}

impl DelegationBroker {
    pub async fn resume_delegation(&self, req: ResumeDelegationRequest) -> DelegationTaskReport {
        // Same parent-cancel coverage contract as `start_delegation`: register
        // the in-flight setup FIRST so a parent cancel landing anywhere from
        // here to park reaches it; deregistered on every exit path.
        let inflight_id = self
            .pending
            .inner
            .lock()
            .await
            .register_inflight(&req.parent_connection_id);
        // Pre-cancel short-circuit — the companion already received
        // `notifications/cancelled` for this `tools/call` before we started.
        // No tool-call claim to drain here: the resume tool never claims
        // delegate-keyed ACP ids.
        if let Some(handle) = req.external_handle.as_deref() {
            if self.take_pre_canceled_handle(handle).await {
                self.drop_inflight(inflight_id).await;
                return report_from_outcome(
                    Some(req.task_id.clone()),
                    None,
                    &DelegationOutcome::from_err(
                        DelegationError::Canceled {
                            reason: "canceled before resume".into(),
                        },
                        None,
                    ),
                    None,
                );
            }
        }
        let cfg = self.config_snapshot().await;
        if !cfg.enabled {
            self.drop_inflight(inflight_id).await;
            return report_from_outcome(
                Some(req.task_id.clone()),
                None,
                &DelegationOutcome::from_err(
                    DelegationError::Canceled {
                        reason: "delegation disabled".into(),
                    },
                    None,
                ),
                None,
            );
        }

        // --- In-memory gate ---------------------------------------------------
        // One lock pass over the live maps. Only a CANCELED, caller-owned cached
        // entry may proceed (its removal is deferred to the registration lock so
        // a failed resume leaves the record answering status queries); a running
        // task and fresher terminal states refuse, and cross-parent hits stay
        // opaque — exactly the `classify_locked` scoping rules.
        //
        // A proceed also CLAIMS the id in `resuming` (same lock), which is what
        // stops two concurrent resumes of one task from both passing this gate
        // and spawning two children onto the same agent session. In the
        // not-in-memory branch ownership isn't proven yet, so a failed claim is
        // carried as `claimed: false` and only refused AFTER the DB ownership
        // check — refusing it here would leak the task's existence to a caller
        // who merely guessed the id.
        enum MemGate {
            Proceed { claimed: bool },
            Refuse(Box<DelegationTaskReport>),
        }
        let gate = {
            let mut inner = self.pending.inner.lock().await;
            if let Some(c) = inner.completed.get(&req.task_id) {
                if c.parent_connection_id != req.parent_connection_id {
                    MemGate::Refuse(Box::new(unknown_report(&req.task_id)))
                } else {
                    // Copy the report fields out so the `completed` borrow ends
                    // before the Canceled arm's mutable `resuming` claim.
                    let (status, child_conversation_id, agent_type) =
                        (c.status, c.child_conversation_id, c.agent_type);
                    match status {
                        TaskStatus::Canceled => {
                            // Caller-owned — safe to name the contention.
                            if inner.resuming.insert(req.task_id.clone()) {
                                MemGate::Proceed { claimed: true }
                            } else {
                                MemGate::Refuse(Box::new(not_resumable_report(
                                    &req.task_id,
                                    status,
                                    Some(child_conversation_id),
                                    Some(agent_type),
                                    "a resume of this task is already in progress.",
                                )))
                            }
                        }
                        TaskStatus::Completed => MemGate::Refuse(Box::new(not_resumable_report(
                            &req.task_id,
                            status,
                            Some(child_conversation_id),
                            Some(agent_type),
                            "the task already completed — resume only applies to a \
                             canceled or interrupted task. Start a new \
                             delegate_to_agent call for follow-up work.",
                        ))),
                        _ => MemGate::Refuse(Box::new(not_resumable_report(
                            &req.task_id,
                            status,
                            Some(child_conversation_id),
                            Some(agent_type),
                            "the task ended in failure rather than being \
                             interrupted — resume only applies to a canceled or \
                             interrupted task. Start a new delegate_to_agent call \
                             to retry it.",
                        ))),
                    }
                }
            } else if let Some(r) = inner.running.get(&req.task_id) {
                if r.parent_connection_id != req.parent_connection_id {
                    MemGate::Refuse(Box::new(unknown_report(&req.task_id)))
                } else {
                    MemGate::Refuse(Box::new(not_resumable_report(
                        &req.task_id,
                        TaskStatus::Running,
                        Some(r.child_conversation_id),
                        Some(r.agent_type),
                        "the task is still running — resume only applies to a \
                         canceled or interrupted task. Poll it with \
                         get_delegation_status (or stop it with cancel_delegation) \
                         instead.",
                    )))
                }
            } else {
                let claimed = inner.resuming.insert(req.task_id.clone());
                MemGate::Proceed { claimed }
            }
        };
        let claimed = match gate {
            MemGate::Refuse(report) => {
                self.drop_inflight(inflight_id).await;
                return *report;
            }
            MemGate::Proceed { claimed } => claimed,
        };

        // Everything past the gate runs in a helper so this single exit point
        // can release the claim on EVERY path — success (the task is in
        // `running` by then, which re-arms the gate's normal refusals) and
        // failure alike. A `claimed: false` outcome belongs to the concurrent
        // resume that holds it; never released here.
        let claim_key = req.task_id.clone();
        let report = self
            .resume_delegation_gated(req, inflight_id, cfg, claimed)
            .await;
        if claimed {
            self.pending.inner.lock().await.resuming.remove(&claim_key);
        }
        report
    }


    /// The post-gate body of [`Self::resume_delegation`]: DB context + status
    /// gates, child re-spawn, continuation send, and registration. Split out so
    /// the caller can release the `resuming` claim at one exit point.
    pub(crate) async fn resume_delegation_gated(
        &self,
        req: ResumeDelegationRequest,
        inflight_id: u64,
        cfg: DelegationConfig,
        claimed: bool,
    ) -> DelegationTaskReport {
        // --- DB context + status gate -----------------------------------------
        // The DB row is authoritative for resumability even when the in-memory
        // cache still remembers the cancel: the user may have driven the child
        // session onward in the meantime, and resuming over that would be a new
        // iteration in disguise.
        let Some(ctx) = self
            .status_lookup
            .find_resume_context_by_call_id(&req.task_id)
            .await
        else {
            self.drop_inflight(inflight_id).await;
            return unknown_report(&req.task_id);
        };
        if ctx.parent_id != Some(req.parent_conversation_id) {
            self.drop_inflight(inflight_id).await;
            return unknown_report(&req.task_id);
        }
        // Deferred claim-contention refusal (see the gate): the caller owns the
        // task, so naming the in-flight resume leaks nothing.
        if !claimed {
            self.drop_inflight(inflight_id).await;
            return not_resumable_report(
                &req.task_id,
                ctx.status,
                Some(ctx.child_conversation_id),
                Some(ctx.agent_type),
                "a resume of this task is already in progress.",
            );
        }
        match ctx.status {
            // Recorded canceled — the canonical interrupted state.
            TaskStatus::Canceled => {}
            // `in_progress` row: a live connection means the child session is
            // genuinely active (either as somebody's running delegation or
            // driven by the user in a tab) — hands off. No live connection
            // means a hard crash stranded the row mid-run: the exact
            // "unexpected termination" this tool exists for.
            TaskStatus::Running => {
                if self
                    .spawner
                    .has_live_connection_for_conversation(ctx.child_conversation_id)
                    .await
                {
                    self.drop_inflight(inflight_id).await;
                    return not_resumable_report(
                        &req.task_id,
                        TaskStatus::Running,
                        Some(ctx.child_conversation_id),
                        Some(ctx.agent_type),
                        "the child session is currently active — resume only \
                         applies to a canceled or interrupted task.",
                    );
                }
            }
            TaskStatus::Completed => {
                self.drop_inflight(inflight_id).await;
                return not_resumable_report(
                    &req.task_id,
                    TaskStatus::Completed,
                    Some(ctx.child_conversation_id),
                    Some(ctx.agent_type),
                    "the task already completed — resume only applies to a \
                     canceled or interrupted task. Start a new delegate_to_agent \
                     call for follow-up work.",
                );
            }
            TaskStatus::Failed | TaskStatus::Unknown => {
                self.drop_inflight(inflight_id).await;
                return unknown_report(&req.task_id);
            }
        }
        let Some(external_id) = ctx.external_id.clone() else {
            self.drop_inflight(inflight_id).await;
            return not_resumable_report(
                &req.task_id,
                ctx.status,
                Some(ctx.child_conversation_id),
                Some(ctx.agent_type),
                "no agent session id was recorded for the child session, so \
                 there is nothing to load. Start a new delegate_to_agent call \
                 instead.",
            );
        };
        let Some(working_dir) = ctx.working_dir.clone() else {
            self.drop_inflight(inflight_id).await;
            return not_resumable_report(
                &req.task_id,
                ctx.status,
                Some(ctx.child_conversation_id),
                Some(ctx.agent_type),
                "the child session's project folder no longer exists. Start a \
                 new delegate_to_agent call in a valid directory instead.",
            );
        };

        // --- Re-spawn the child, resuming its agent session --------------------
        // Same per-agent defaults as a fresh spawn; no depth re-check — the
        // child row already exists at its chain position, resume adds no node.
        let (preferred_mode_id, preferred_config_values) = cfg
            .agent_defaults
            .get(&ctx.agent_type)
            .map(|d: &AgentDelegationDefaults| (d.mode_id.clone(), d.config_values.clone()))
            .unwrap_or((None, BTreeMap::new()));
        // Checkpoint #1: a parent cancel during the gates — nothing spawned yet.
        if self.take_inflight_cancel(inflight_id).await {
            return report_from_outcome(
                Some(req.task_id.clone()),
                Some(ctx.agent_type),
                &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                None,
            );
        }
        let spawned = match self
            .spawner
            .spawn_for_resume(
                &req.parent_connection_id,
                ctx.agent_type,
                Some(working_dir),
                &external_id,
                preferred_mode_id,
                preferred_config_values,
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                self.drop_inflight(inflight_id).await;
                return report_from_outcome(
                    Some(req.task_id.clone()),
                    Some(ctx.agent_type),
                    &DelegationOutcome::from_err(
                        DelegationError::SpawnFailed(format!("resume: {e}")),
                        Some(ctx.child_conversation_id),
                    ),
                    None,
                );
            }
        };
        let child_connection_id = spawned.connection_id.clone();
        // Checkpoint #2: a parent cancel during spawn — the connection exists
        // but no prompt went out; disconnect-only teardown. A DEDUP-REUSED
        // connection (the user has the child session open — see
        // `ResumedSpawn::reused`) is not ours to tear down: nothing was sent
        // on it, so abandoning the resume leaves it exactly as found.
        if self.take_inflight_cancel(inflight_id).await {
            if !spawned.reused {
                let _ = self.spawner.disconnect(&child_connection_id).await;
            }
            return report_from_outcome(
                Some(req.task_id.clone()),
                Some(ctx.agent_type),
                &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                None,
            );
        }

        let call_id = req.task_id.clone();
        tracing::Span::current().record("child_connection_id", child_connection_id.as_str());
        // Label for meta writes / the started event: the child row's title was
        // seeded from the original task text, so it is the closest thing to the
        // original `task_preview` the broker no longer holds.
        let task_preview = ctx
            .title
            .as_deref()
            .map(|t| truncate_on_char_boundary(t, TASK_PREVIEW_CAP))
            .unwrap_or_else(|| "(resumed delegation)".to_string());
        // The resumed run reuses the ORIGINAL parent-side tool_use_id persisted
        // on the row so the parent's original delegation card re-binds; a row
        // without one gets a synthetic id, which `write_meta_if_real` /
        // `emit_*_if_real` skip.
        let parent_tool_use_id = ctx
            .parent_tool_use_id
            .clone()
            .unwrap_or_else(|| format!("delegation-{}", uuid::Uuid::new_v4()));

        // Reserve BEFORE sending — identical race contract to
        // `start_delegation`: once the continuation prompt is dispatched, a
        // fast TurnComplete (`complete_call`, keyed by `call_id` — live again
        // because the row's `delegation_call_id` never changed) or an immediate
        // child failure (`cancel_by_child_connection`, keyed by connection) can
        // fire before the entry is parked; the reservation buffers either for
        // the disposition block to drain.
        self.pending
            .inner
            .lock()
            .await
            .reserve(&call_id, &child_connection_id);

        if let Err(e) = self
            .spawner
            .send_resume_prompt(
                &child_connection_id,
                build_resume_prompt(req.reason.as_deref()),
                ctx.folder_id,
                ctx.child_conversation_id,
            )
            .await
        {
            {
                let mut inner = self.pending.inner.lock().await;
                inner.unreserve(&call_id, &child_connection_id);
                inner.deregister_inflight(inflight_id);
            }
            // Tear the connection down iff WE spawned it. A fresh connection is
            // ours to disconnect even when the failed send got far enough to
            // bind it to the conversation (a post-link status-write / enqueue
            // failure) — skipping it there would leak a live, bound connection
            // that blocks every future resume behind the "currently active"
            // gate. A DEDUP-REUSED connection is the opposite: it belonged to
            // whoever was already driving it (the user's open tab, having
            // raced past the live-connection gate), so it is left alone. The
            // spawn-time `reused` fact is the only reliable discriminator — a
            // post-hoc liveness probe can't tell our own just-bound connection
            // from a foreign one.
            if !spawned.reused {
                let _ = self.spawner.disconnect(&child_connection_id).await;
            }
            return report_from_outcome(
                Some(call_id),
                Some(ctx.agent_type),
                &DelegationOutcome::from_err(
                    DelegationError::SpawnFailed(format!("resume send: {e}")),
                    Some(ctx.child_conversation_id),
                ),
                None,
            );
        }

        let started_at = Instant::now();

        // Re-mark the parent's original card in-flight and re-announce the live
        // delegation so the frontend re-binds the child sub-thread.
        self.write_meta_if_real(
            &req.parent_connection_id,
            &parent_tool_use_id,
            build_delegation_meta(
                "running",
                Some(&child_connection_id),
                Some(ctx.child_conversation_id),
                None,
                None,
                None,
                Some(&task_preview),
                Some(&call_id),
            ),
        )
        .await;
        self.emit_started_if_real(
            &req.parent_connection_id,
            &parent_tool_use_id,
            &child_connection_id,
            ctx.child_conversation_id,
            ctx.agent_type,
            &task_preview,
            &call_id,
        )
        .await;

        // --- Register running, or resolve a terminal that beat us --------------
        // Same atomic disposition as `start_delegation` (see the long comment
        // there), with one resume-specific addition: the STALE canceled cache
        // entry (if any) is removed inside this same lock — after this point the
        // task is either running again or freshly terminal, and either way the
        // old record must not shadow it (`classify_locked` consults `completed`
        // first) nor double-count the byte valve on re-insert.
        enum Disposition {
            ChildTerminal(DelegationOutcome),
            ParentCanceled,
            Running,
        }
        let setup_duration_ms = started_at.elapsed().as_millis() as u64;
        let disposition = {
            let mut inner = self.pending.inner.lock().await;
            inner.remove_completed(&call_id);
            let child_terminal: Option<(u64, DelegationOutcome)> =
                if let Some((stamp, outcome)) = inner.take_early_complete(&call_id) {
                    Some((stamp, outcome))
                } else {
                    inner
                        .take_early_cancel(&child_connection_id)
                        .map(|(stamp, reason)| {
                            (
                                stamp,
                                DelegationOutcome::from_err(
                                    DelegationError::Canceled { reason },
                                    Some(ctx.child_conversation_id),
                                ),
                            )
                        })
                };
            let parent_canceled_at = inner.inflight_canceled_at(inflight_id);
            inner.unreserve(&call_id, &child_connection_id);
            let record = |inner: &mut PendingInner, outcome: &DelegationOutcome| {
                inner.insert_completed(
                    &call_id,
                    build_completed(
                        &req.parent_connection_id,
                        ctx.child_conversation_id,
                        ctx.agent_type,
                        setup_duration_ms,
                        outcome,
                    ),
                );
            };
            match (child_terminal, parent_canceled_at) {
                (Some((child_stamp, outcome)), Some(cancel_stamp)) => {
                    inner.deregister_inflight(inflight_id);
                    if child_stamp < cancel_stamp {
                        record(&mut inner, &outcome);
                        Disposition::ChildTerminal(outcome)
                    } else {
                        record(
                            &mut inner,
                            &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                        );
                        Disposition::ParentCanceled
                    }
                }
                (Some((_, outcome)), None) => {
                    inner.deregister_inflight(inflight_id);
                    record(&mut inner, &outcome);
                    Disposition::ChildTerminal(outcome)
                }
                (None, Some(_)) => {
                    inner.deregister_inflight(inflight_id);
                    record(
                        &mut inner,
                        &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                    );
                    Disposition::ParentCanceled
                }
                (None, None) => {
                    inner.running.insert(
                        call_id.clone(),
                        RunningTask {
                            child_connection_id: child_connection_id.clone(),
                            child_conversation_id: ctx.child_conversation_id,
                            parent_connection_id: req.parent_connection_id.clone(),
                            parent_tool_use_id: parent_tool_use_id.clone(),
                            agent_type: ctx.agent_type,
                            task_preview: task_preview.clone(),
                            task_id: call_id.clone(),
                            external_handle: req.external_handle.clone(),
                            started_at,
                            last_surfaced_block: None,
                        },
                    );
                    inner.deregister_inflight(inflight_id);
                    Disposition::Running
                }
            }
        };

        match disposition {
            Disposition::ChildTerminal(outcome) => {
                self.finalize_delegation(
                    &req.parent_connection_id,
                    &parent_tool_use_id,
                    &child_connection_id,
                    ctx.child_conversation_id,
                    ctx.agent_type,
                    setup_duration_ms,
                    &outcome,
                    &task_preview,
                    &call_id,
                )
                .await;
                self.result_notify.notify_waiters();
                return report_from_outcome(
                    Some(call_id),
                    Some(ctx.agent_type),
                    &outcome,
                    Some(setup_duration_ms),
                );
            }
            Disposition::ParentCanceled => {
                self.write_meta_if_real(
                    &req.parent_connection_id,
                    &parent_tool_use_id,
                    build_delegation_meta(
                        "failed",
                        Some(&child_connection_id),
                        Some(ctx.child_conversation_id),
                        Some("canceled"),
                        None,
                        Some(setup_duration_ms),
                        Some(&task_preview),
                        Some(&call_id),
                    ),
                )
                .await;
                self.emit_completed_if_real(
                    &req.parent_connection_id,
                    &parent_tool_use_id,
                    &child_connection_id,
                    ctx.child_conversation_id,
                    ctx.agent_type,
                    DelegationResultSummary::Err {
                        error_code: "canceled".to_string(),
                    },
                )
                .await;
                let _ = self.spawner.cancel(&child_connection_id).await;
                let _ = self.spawner.disconnect(&child_connection_id).await;
                self.result_notify.notify_waiters();
                return report_from_outcome(
                    Some(call_id),
                    Some(ctx.agent_type),
                    &canceled_outcome(ctx.child_conversation_id, "parent canceled"),
                    Some(setup_duration_ms),
                );
            }
            Disposition::Running => {}
        }

        // Second pre-cancel check — a `notifications/cancelled` that landed
        // between the entry-side check and the registration above. Mirror of
        // `start_delegation`'s second check.
        if let Some(handle) = req.external_handle.as_deref() {
            if self.take_pre_canceled_handle(handle).await {
                let canceled_duration_ms = {
                    let mut inner = self.pending.inner.lock().await;
                    if inner.running.remove(&call_id).is_some() {
                        let outcome = canceled_outcome(
                            ctx.child_conversation_id,
                            "canceled before await",
                        );
                        let duration_ms = started_at.elapsed().as_millis() as u64;
                        inner.insert_completed(
                            &call_id,
                            build_completed(
                                &req.parent_connection_id,
                                ctx.child_conversation_id,
                                ctx.agent_type,
                                duration_ms,
                                &outcome,
                            ),
                        );
                        Some(duration_ms)
                    } else {
                        None
                    }
                };
                if let Some(duration_ms) = canceled_duration_ms {
                    self.write_meta_if_real(
                        &req.parent_connection_id,
                        &parent_tool_use_id,
                        build_delegation_meta(
                            "failed",
                            Some(&child_connection_id),
                            Some(ctx.child_conversation_id),
                            Some("canceled"),
                            None,
                            Some(duration_ms),
                            Some(&task_preview),
                            Some(&call_id),
                        ),
                    )
                    .await;
                    self.emit_completed_if_real(
                        &req.parent_connection_id,
                        &parent_tool_use_id,
                        &child_connection_id,
                        ctx.child_conversation_id,
                        ctx.agent_type,
                        DelegationResultSummary::Err {
                            error_code: "canceled".to_string(),
                        },
                    )
                    .await;
                    let _ = self.spawner.cancel(&child_connection_id).await;
                    let _ = self.spawner.disconnect(&child_connection_id).await;
                    self.result_notify.notify_waiters();
                    return report_from_outcome(
                        Some(call_id),
                        Some(ctx.agent_type),
                        &canceled_outcome(ctx.child_conversation_id, "canceled before await"),
                        Some(duration_ms),
                    );
                }
            }
        }

        resume_ack(call_id, ctx.child_conversation_id, ctx.agent_type)
    }
}
