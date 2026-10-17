use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：compact
impl TaskEngine {
    /// Compact the resumed session's context before this round's prompt, when
    /// the folder's settings ask for it (`auto_compact_percent`).
    ///
    /// Placement is the whole design. It runs AFTER the spawn (only a live
    /// session can be compacted, and only a live session can say how full it
    /// is) and BEFORE the `index` insert and `mark_running`, so:
    /// - the compaction turn's `TurnComplete` reaches `on_event` with no index
    ///   entry behind it and is dropped — an indexed one would settle the task
    ///   into review before its actual work started;
    /// - the round's own prompt is sent only once this turn has landed, which
    ///   is the point of the feature.
    ///
    /// The cost of staying out of `index` is that a compaction turn which
    /// somehow blocked on a permission would not flip the card to
    /// `awaiting_input` — it would sit in `preparing` until the user cancels.
    /// Accepted deliberately: compaction is a self-contained summarisation
    /// turn, and the alternative (indexing it) trades a rare stall for a
    /// systematic mis-settle of every task that compacts.
    ///
    /// Out of `index` is NOT out of sight, though. The launch publishes the
    /// run's connection on the row before calling this (see `mark_preparing_live`
    /// / `mark_merging_live`), and this records a `started` timeline entry once
    /// the turn is away, so a wait that can run for minutes is watchable rather
    /// than a card that has apparently stopped.
    ///
    /// Never fails the launch. A compaction that could not be measured, has no
    /// command to run, or ends badly records itself on the timeline and lets
    /// the round proceed: an over-full context makes the next prompt likely to
    /// fail, but refusing to send it makes it certain.
    ///
    /// Only called for a launch that RESUMED — a fresh session's context is
    /// empty by construction.
    pub(super) async fn compact_before_prompt(&self, req: CompactRequest<'_>) {
        let CompactRequest {
            task_id,
            run_seq,
            conn_id,
            agent_type,
            settings,
            folder_id,
            conversation_id,
            in_flight,
        } = req;
        let threshold = settings.auto_compact_percent;
        if threshold <= 0 {
            return;
        }
        let Some(before) = self.context_reading(conn_id, conversation_id).await else {
            // The agent reports no usable occupancy (no live `usage_update`, and
            // a transcript that carries no window either). Recorded rather than
            // silent: with the threshold switched on, "nothing happened" is a
            // result the user needs an explanation for.
            self.record_compact_event(
                task_id,
                run_seq,
                serde_json::json!({
                    "status": "skipped",
                    "reason": "usage_unknown",
                    "threshold_percent": threshold,
                }),
            )
            .await;
            return;
        };
        if !compact::trips_threshold(before.percent, threshold) {
            return;
        }

        let advertised = match self.manager.get_state(conn_id).await {
            Some(state) => state.read().await.available_commands.clone(),
            None => Vec::new(),
        };
        let Some(command) = compact::resolve_compact_command(
            settings.compact_command.as_deref(),
            agent_type,
            &advertised,
        ) else {
            self.record_compact_event(
                task_id,
                run_seq,
                serde_json::json!({
                    "status": "skipped",
                    "reason": "no_command",
                    "threshold_percent": threshold,
                    "before_percent": round1(before.percent),
                }),
            )
            .await;
            return;
        };

        // Cancel gate, then the kill slot, then the gate AGAIN — the same
        // ordering `reserve_setup_slot` uses for the init command. A cancel
        // that lands before the slot exists is caught by the re-check (it flips
        // the row first); one that lands after finds the slot and aborts the
        // turn. Without the re-check, a cancel in between would fall through to
        // the task lock, which this launch holds until the compaction it is
        // trying to stop has finished.
        if !still_expected(&self.db.conn, task_id, run_seq, in_flight).await {
            return;
        }
        self.compacting.lock().await.insert(
            task_id,
            CompactRun {
                run_seq,
                conn_id: conn_id.to_string(),
            },
        );
        if !still_expected(&self.db.conn, task_id, run_seq, in_flight).await {
            self.release_compact_slot(task_id, run_seq).await;
            return;
        }

        // Subscribed BEFORE the send: the turn can complete the instant it is
        // enqueued, and a receiver only ever sees what is published after it
        // subscribes.
        let mut rx = self.bus.subscribe();
        let sent = self
            .manager
            .send_prompt_linked_with_message_id(
                &self.db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: command.clone(),
                }],
                Some(folder_id),
                Some(conversation_id),
                None,
                None,
            )
            .await;
        if let Err(e) = sent {
            self.release_compact_slot(task_id, run_seq).await;
            self.record_compact_event(
                task_id,
                run_seq,
                serde_json::json!({
                    "status": "failed",
                    "command": command,
                    "threshold_percent": threshold,
                    "before_percent": round1(before.percent),
                    "error": e.to_string(),
                }),
            )
            .await;
            return;
        }
        // The turn is away and the wait below is unbounded, so this is the only
        // thing that can explain the gap while it runs: a round that sits for
        // minutes between its dispatch and its prompt is otherwise a timeline
        // with nothing in it, and a compaction that never returns leaves no
        // trace at all. Recorded AFTER the send so it never claims a turn that
        // was refused, and paired with the outcome event below.
        self.record_compact_event(
            task_id,
            run_seq,
            serde_json::json!({
                "status": "started",
                "command": command,
                "threshold_percent": threshold,
                "before_percent": round1(before.percent),
                "before_source": before.source,
            }),
        )
        .await;
        self.emit_upsert(task_id);

        let outcome = self.await_compaction_turn(&mut rx, conn_id).await;
        self.release_compact_slot(task_id, run_seq).await;

        // Read the occupancy back from the LIVE session only. The transcript
        // fallback is write-behind, so re-parsing here would report the
        // pre-compaction figure as the result of the compaction.
        let after = self.live_context_reading(conn_id).await;
        let mut payload = serde_json::json!({
            "status": outcome.status,
            "command": command,
            "threshold_percent": threshold,
            "before_percent": round1(before.percent),
            "before_source": before.source,
        });
        if let Some(map) = payload.as_object_mut() {
            if let Some(used) = before.used {
                map.insert("before_used_tokens".into(), used.into());
            }
            if let Some(size) = before.size {
                map.insert("before_size_tokens".into(), size.into());
            }
            if let Some(after) = after.as_ref() {
                map.insert("after_percent".into(), round1(after.percent).into());
                if let Some(used) = after.used {
                    map.insert("after_used_tokens".into(), used.into());
                }
            }
            if let Some(detail) = outcome.detail {
                map.insert("detail".into(), detail.into());
            }
        }
        self.record_compact_event(task_id, run_seq, payload).await;
    }

    /// Wait out the compaction turn on `conn_id`.
    ///
    /// Deliberately unbounded in TIME — compacting a full context is a real
    /// model turn, and a deadline here would just send the round's prompt into
    /// the very context we were told to shrink. What ends the wait is evidence:
    /// the turn completing, the connection dying, or a cancel (which aborts the
    /// turn through [`Self::abort_compaction`], producing a
    /// `TurnComplete{cancelled}` of its own).
    ///
    /// The bus alone is not enough evidence, which is why the wait also polls
    /// liveness. `apply_event` clears `turn_in_flight` on `TurnComplete` but
    /// NOT on a terminal `Error` or `StatusChanged{Disconnected}` — and a
    /// receiver that lags drops whichever of those arrived. Blocking purely on
    /// `recv()` would then park forever on a connection that is already gone,
    /// holding the task lock and taking any later cancel down with it. The tick
    /// never shortens a live turn; it only notices a dead one.
    ///
    /// Records the completing envelope's seq in `compaction_turns` BEFORE
    /// returning, so the engine's own (possibly lagging) receiver can still
    /// recognise it later and refuse to settle the round with it.
    pub(super) async fn await_compaction_turn(
        &self,
        rx: &mut tokio::sync::broadcast::Receiver<Arc<EventEnvelope>>,
        conn_id: &str,
    ) -> CompactOutcome {
        loop {
            let event = tokio::select! {
                received = rx.recv() => received,
                _ = tokio::time::sleep(COMPACTION_LIVENESS_TICK) => {
                    match self.compaction_still_running(conn_id).await {
                        Some(outcome) => return outcome,
                        None => continue,
                    }
                }
            };
            match event {
                Ok(env) if env.connection_id == conn_id => match &env.payload {
                    AcpEvent::TurnComplete { stop_reason, .. } => {
                        self.mark_compaction_turn(conn_id, env.seq).await;
                        return match stop_reason.as_str() {
                            "cancelled" | "canceled" => CompactOutcome::new("canceled", None),
                            "end_turn" | "" => CompactOutcome::new("ok", None),
                            // The agent REFUSED to run the compaction prompt —
                            // nothing was compacted, so this is not an "ok" with
                            // a footnote. It reaches this arm at all only because
                            // a prompt rejection stopped tearing the connection
                            // down (it used to arrive as the terminal `Error`
                            // below, and settled as `failed`); mapping it
                            // anywhere else would silently promote a failed
                            // compaction to a successful one.
                            "rejected" => CompactOutcome::new(
                                "failed",
                                Some("the agent rejected the compaction prompt".into()),
                            ),
                            // The agent REFUSED to run the compaction prompt —
                            // nothing was compacted, so this is not an "ok" with
                            // a footnote. It reaches this arm at all only because
                            // a prompt rejection stopped tearing the connection
                            // down (it used to arrive as the terminal `Error`
                            // below, and settled as `failed`); mapping it
                            // anywhere else would silently promote a failed
                            // compaction to a successful one.
                            other => CompactOutcome::new("ok", Some(other.to_string())),
                        };
                    }
                    // A terminal error tears the connection down without a
                    // `TurnComplete` (the state is discarded, not settled), so
                    // it has to end the wait itself. Fenced on the way out for
                    // the same reason the liveness exit is: in-order delivery
                    // proves no completion is queued on THIS receiver, but not
                    // that the dying connection emits none after this event.
                    AcpEvent::Error {
                        message,
                        terminal: true,
                        ..
                    } => {
                        self.fence_completed_turns(conn_id).await;
                        return CompactOutcome::new("failed", Some(message.clone()));
                    }
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Disconnected | ConnectionStatus::Error,
                    } => {
                        self.fence_completed_turns(conn_id).await;
                        return CompactOutcome::new("failed", Some("connection lost".into()));
                    }
                    _ => {}
                },
                Ok(_) => {}
                // Whatever ended this turn may be among the events the lag
                // dropped, so ask the connection directly instead of waiting
                // for a message that may never come again. Only OUR receiver
                // dropped them — the engine's own copy may still be pending,
                // which is why the probe fences before ending the wait.
                Err(RecvError::Lagged(_)) => {
                    if let Some(outcome) = self.compaction_still_running(conn_id).await {
                        return outcome;
                    }
                }
                Err(RecvError::Closed) => {
                    return CompactOutcome::new("failed", Some("event bus closed".into()))
                }
            }
        }
    }

    /// Liveness probe for a compaction turn: `None` means "still running, keep
    /// waiting", `Some(outcome)` means the wait is over.
    ///
    /// Reads the same flag the send gate itself owns. `send_prompt_inner` sets
    /// `turn_in_flight` synchronously before the send returns, so by the time
    /// this can run, `false` means this turn has already ended and its event
    /// was missed. A connection the manager no longer knows is gone for good —
    /// its state (and any flag on it) was discarded, not settled.
    ///
    /// Ending the wait WITHOUT having seen the completion still has to fence
    /// that completion off, because it may be sitting in the engine's own
    /// receiver right now: the flag is cleared by `apply_event` BEFORE the
    /// envelope is broadcast, so this can observe a finished turn whose event
    /// nobody has delivered yet. The watermark is the connection's current
    /// `event_seq` — every event already emitted is at or below it, and the
    /// round's own completion, which cannot even be prompted until this
    /// function has returned, is necessarily above it.
    pub(super) async fn compaction_still_running(&self, conn_id: &str) -> Option<CompactOutcome> {
        let Some(state) = self.manager.get_state(conn_id).await else {
            self.fence_completed_turns(conn_id).await;
            return Some(CompactOutcome::new("failed", Some("connection lost".into())));
        };
        let seq = {
            let s = state.read().await;
            if s.turn_in_flight {
                return None;
            }
            s.event_seq
        };
        self.mark_compaction_turn(conn_id, seq).await;
        Some(CompactOutcome::new("ok", Some("turn end not observed".into())))
    }

    /// Fence every `TurnComplete` this connection has emitted so far, for a
    /// wait that is ending without having read one.
    ///
    /// The watermark is the connection's current `event_seq`: everything
    /// already emitted is at or below it, and the round's own completion —
    /// which cannot be prompted until this wait returns — is above it. A
    /// connection the manager has already dropped can report no seq, so its
    /// whole id is fenced; ids are per-launch UUIDs and never reused.
    async fn fence_completed_turns(&self, conn_id: &str) {
        let seq = match self.manager.get_state(conn_id).await {
            Some(state) => state.read().await.event_seq,
            None => u64::MAX,
        };
        self.mark_compaction_turn(conn_id, seq).await;
    }

    /// Fence off every `TurnComplete` up to `seq` on this connection: they
    /// belong to the launch's pre-prompt compaction, not to the round.
    pub(super) async fn mark_compaction_turn(&self, conn_id: &str, seq: u64) {
        self.compaction_turns
            .lock()
            .await
            .insert(conn_id.to_string(), seq);
    }

    /// Whether this `TurnComplete` is fenced off as a launch's own compaction
    /// signal — in which case `on_event` must not treat it as the round's turn.
    ///
    /// A watermark rather than an exact seq: the wait can end without ever
    /// seeing the completion (see `compaction_still_running`), and it must
    /// still be able to fence one it never read. Self-cleaning — a seq ABOVE
    /// the watermark can only be the round's own turn, which both lets it
    /// through and retires the mark. A connection that never completes another
    /// turn is cleared by `retire_connection` / `forget_connection`.
    pub(super) async fn claim_compaction_turn(&self, conn_id: &str, seq: u64) -> bool {
        let mut marks = self.compaction_turns.lock().await;
        match marks.get(conn_id).copied() {
            Some(marked) if marked >= seq => true,
            Some(_) => {
                marks.remove(conn_id);
                false
            }
            None => false,
        }
    }

    /// Drop the in-memory traces of a connection this engine is done with: its
    /// run binding and its compaction fence. Paired with the `disconnect` calls
    /// on the launch's teardown paths, which — unlike `retire_connection` —
    /// unwind a generation that never became live.
    pub(super) async fn forget_connection(&self, conn_id: &str) {
        self.index.lock().await.remove(conn_id);
        self.compaction_turns.lock().await.remove(conn_id);
    }

    /// Abort a pre-prompt compaction this task's launch is waiting on. Called
    /// by `cancel` BEFORE it takes the task lock, for the same reason
    /// [`Self::kill_setup_child`] is: the launch holds that lock across the
    /// whole compaction turn.
    ///
    /// Only cancels the turn — the launch's own teardown (disconnect, and the
    /// conversation when it was not resumed) happens where it always has, on
    /// the `mark_running` CAS it is about to lose.
    pub(super) async fn abort_compaction(&self, task_id: i32, run_seq: i32) {
        let conn_id = {
            let compacting = self.compacting.lock().await;
            match compacting.get(&task_id) {
                Some(run) if run.run_seq == run_seq => run.conn_id.clone(),
                _ => return,
            }
        };
        let _ = self.manager.cancel(&self.db.conn, &conn_id).await;
    }

    /// Drop this generation's compaction slot. Generation-checked so a slow
    /// unwind can never drop a newer launch's slot.
    pub(super) async fn release_compact_slot(&self, task_id: i32, run_seq: i32) {
        let mut compacting = self.compacting.lock().await;
        if matches!(compacting.get(&task_id), Some(run) if run.run_seq == run_seq) {
            compacting.remove(&task_id);
        }
    }

    /// Record one step of a compaction on the timeline, stamped with the
    /// generation it belongs to.
    ///
    /// The stamp is load-bearing, not decoration: the board decides "this card
    /// is compacting right now" from the LAST `context_compact` of the current
    /// generation being a `started` with no outcome after it, and an unstamped
    /// event from an earlier round would answer for a round that has moved on.
    async fn record_compact_event(
        &self,
        task_id: i32,
        run_seq: i32,
        mut payload: serde_json::Value,
    ) {
        if let Some(map) = payload.as_object_mut() {
            map.insert("run_seq".into(), run_seq.into());
        }
        let _ = work_task_service::record_event(
            &self.db.conn,
            task_id,
            "context_compact",
            "engine",
            Some(payload),
        )
        .await;
    }

    /// Context-window occupancy of the session about to be prompted: the live
    /// connection first, the parsed transcript second.
    ///
    /// The live reading is the agent's own accounting of the session it just
    /// loaded, and costs a lock. The transcript is what the Session Details
    /// dialog shows and covers every agent that reports usage in its own files
    /// but publishes no ACP `usage_update` — at the price of a full parse,
    /// which is why it only runs when the threshold is switched on and the live
    /// value is missing.
    async fn context_reading(&self, conn_id: &str, conversation_id: i32) -> Option<ContextReading> {
        if let Some(live) = self.live_context_reading(conn_id).await {
            return Some(live);
        }
        // The transcript belongs to the session the ROW names; the compaction
        // goes to the one the CONNECTION holds. Those are the same session on
        // an ordinary resume — but not when a `session/load` failure the
        // adapter swallowed fell through to `session/new`, which `spawn_agent`
        // still reports as success, so the launch still reads as `resumed`
        // (see the codeg#500 note in `send_prompt_linked_with_message_id`).
        // Measuring the old session and compacting the new one would spend a
        // whole turn shrinking a context that is already empty, so the fallback
        // is only trusted when the two ids match exactly.
        //
        // Exactly, not through `continued_session_ids`: that relation answers
        // "is this the same CONVERSATION", and a continuation is precisely the
        // case where the agent restarted and its context was rebuilt — the old
        // transcript's occupancy would not describe the new session.
        let live_session = self
            .manager
            .get_state(conn_id)
            .await?
            .read()
            .await
            .external_id
            .clone()?;
        let row = conversation_service::get_by_id(&self.db.conn, conversation_id)
            .await
            .ok()?;
        if !compact::transcript_describes_live_session(
            row.external_id.as_deref(),
            Some(live_session.as_str()),
        ) {
            tracing::info!(
                conversation_id,
                row_session = row.external_id.as_deref().unwrap_or("<none>"),
                live_session = %live_session,
                "[work_task] transcript occupancy refused: the connection holds a \
                 different session than the row"
            );
            return None;
        }
        let (detail, _) = get_folder_conversation_core(&self.db.conn, conversation_id)
            .await
            .ok()?;
        let stats = detail.session_stats?;
        let percent = stats
            .context_window_usage_percent
            .filter(|p| p.is_finite() && *p > 0.0)
            .or_else(|| {
                compact::occupancy_percent(
                    stats.context_window_used_tokens,
                    stats.context_window_max_tokens,
                )
            })?;
        Some(ContextReading {
            percent,
            used: stats.context_window_used_tokens,
            size: stats.context_window_max_tokens,
            source: "transcript",
        })
    }

    /// The live connection's own `usage_update` reading, if it has published
    /// one.
    async fn live_context_reading(&self, conn_id: &str) -> Option<ContextReading> {
        let state = self.manager.get_state(conn_id).await?;
        let usage = state.read().await.usage.clone()?;
        let percent = compact::occupancy_percent(Some(usage.used), Some(usage.size))?;
        Some(ContextReading {
            percent,
            used: Some(usage.used),
            size: Some(usage.size),
            source: "live",
        })
    }

    /// Start preflight: the target folder must exist, be live, and be a
    /// project root (not a worktree).
    pub(super) async fn preflight_folder(&self, folder_id: i32) -> Result<(), String> {
        let row = folder::Entity::find_by_id(folder_id)
            .one(&self.db.conn)
            .await
            .map_err(|e| e.to_string())?
            .filter(|f| f.deleted_at.is_none())
            .ok_or_else(|| "folder not found".to_string())?;
        if row.parent_id.is_some() {
            return Err("tasks must run from a project folder, not a worktree".to_string());
        }
        Ok(())
    }

    // ── event settlement ────────────────────────────────────────────────────
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// A launch's in-flight pre-prompt compaction, keyed by task in
/// [`TaskEngine::compacting`]. Holds the connection the compaction turn runs
/// on so a cancel can abort that turn without the task lock, and the generation
/// that owns it so a stale cancel cannot abort a newer run's compaction.
pub(super) struct CompactRun {
    pub(super) run_seq: i32,
    pub(super) conn_id: String,
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Everything [`TaskEngine::compact_before_prompt`] needs about the launch it
/// is running inside. A struct rather than a parameter list because four of
/// the fields are bare `i32`s in a row — named at the call site, a transposed
/// pair is a compile error instead of a task compacting someone else's session.
pub(super) struct CompactRequest<'a> {
    pub(super) task_id: i32,
    pub(super) run_seq: i32,
    pub(super) conn_id: &'a str,
    pub(super) agent_type: AgentType,
    pub(super) settings: &'a WorkTaskFolderSettings,
    /// The WORKTREE folder the conversation belongs to — same value the round's
    /// own prompt is linked with.
    pub(super) folder_id: i32,
    pub(super) conversation_id: i32,
    /// Status this generation holds for the rest of the launch, which the
    /// cancel gates re-check (`preparing`, or `merging` for a merge round).
    pub(super) in_flight: WorkTaskStatus,
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// One context-window occupancy reading, plus where it came from — the source
/// rides the timeline event because "live" and "transcript" answer slightly
/// different questions (what the agent has loaded vs. what its last turn
/// recorded), and a surprising percentage is only debuggable with it.
pub(super) struct ContextReading {
    pub(super) percent: f64,
    pub(super) used: Option<u64>,
    pub(super) size: Option<u64>,
    pub(super) source: &'static str,
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// How a compaction turn ended: `"ok"` / `"canceled"` / `"failed"`, plus the
/// stop reason or error worth showing beside it.
pub(super) struct CompactOutcome {
    pub(super) status: &'static str,
    pub(super) detail: Option<String>,
}

impl CompactOutcome {
    fn new(status: &'static str, detail: Option<String>) -> Self {
        Self { status, detail }
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// One decimal place, for percentages written to the timeline — the same
/// precision the composer's context ring shows, so the two never read as
/// disagreeing about the same session.
pub(super) fn round1(percent: f64) -> f64 {
    (percent * 10.0).round() / 10.0
}
