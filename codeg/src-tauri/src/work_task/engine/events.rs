use super::*;

// [crewchat] 从 engine.rs 拆出（批次 B 目录化）：events
impl TaskEngine {
    pub(super) async fn on_event(self: &Arc<Self>, env: &EventEnvelope) {
        match &env.payload {
            AcpEvent::TurnComplete { stop_reason, .. } => {
                // A pre-prompt compaction's completion is the LAUNCH's signal,
                // never the round's — see `compaction_turns` for why the index
                // alone cannot tell them apart from in here.
                if self.claim_compaction_turn(&env.connection_id, env.seq).await {
                    return;
                }
                self.on_turn_complete(&env.connection_id, stop_reason).await;
            }
            AcpEvent::QuestionRequest { question_id, .. } => {
                self.track_request(&env.connection_id, format!("q:{question_id}"), true)
                    .await;
            }
            AcpEvent::QuestionResolved { question_id } => {
                self.track_request(&env.connection_id, format!("q:{question_id}"), false)
                    .await;
            }
            AcpEvent::PermissionRequest { request_id, .. } => {
                self.track_request(&env.connection_id, format!("p:{request_id}"), true)
                    .await;
            }
            AcpEvent::PermissionResolved { request_id } => {
                self.track_request(&env.connection_id, format!("p:{request_id}"), false)
                    .await;
            }
            AcpEvent::PlanApprovalRequest { approval_id, .. } => {
                self.track_request(&env.connection_id, format!("a:{approval_id}"), true)
                    .await;
            }
            AcpEvent::PlanApprovalResolved { approval_id } => {
                self.track_request(&env.connection_id, format!("a:{approval_id}"), false)
                    .await;
            }
            // Both delegation events ride the PARENT's stream, so
            // `env.connection_id` is the (possibly task-owning) parent and the
            // child id is in the payload.
            AcpEvent::DelegationStarted {
                child_connection_id,
                ..
            } => {
                // Scoped to task runs: a delegation from an ordinary chat tab
                // has no board row to flip, and mapping it would grow this map
                // for the life of the process. Resolved through
                // `task_for_connection` rather than `index` alone so a nested
                // delegation (a sub-agent delegating further) maps to the same
                // run — its prompts block the task just as much.
                if self.task_for_connection(&env.connection_id).await.is_some() {
                    self.delegation_parents
                        .lock()
                        .await
                        .insert(child_connection_id.clone(), env.connection_id.clone());
                    // The broker starts the child's turn BEFORE announcing it
                    // (`send_prompt_linked_for_delegation` precedes
                    // `emit_started_if_real`), so a child that blocks
                    // immediately raised its prompt while we had no mapping and
                    // we dropped it. Recover from live state now that we do —
                    // otherwise the board sits at `running` for a run that is
                    // already parked on the user.
                    self.backfill_child_blocking_prompts(child_connection_id)
                        .await;
                }
            }
            AcpEvent::DelegationCompleted {
                child_connection_id,
                ..
            } => {
                self.forget_delegation_child(child_connection_id).await;
            }
            _ => {}
        }
    }

    /// Resolve the task generation an event belongs to, and whether `conn_id` is
    /// the run's OWN connection. A connection that isn't itself a task run may
    /// still be a delegation child of one, in which case the run is its
    /// parent's and the caller must namespace anything it records by
    /// `conn_id` — see [`Self::track_request`].
    ///
    /// Walks the delegation chain rather than one hop: with `depth_limit > 1` a
    /// sub-agent can delegate further, and a grandchild parked on a permission
    /// blocks the run exactly as much as a direct child does. Bounded by
    /// [`MAX_DELEGATION_CHAIN_HOPS`] so a malformed map can never spin.
    ///
    /// Never holds two of the engine's maps at once (each guard is a statement
    /// temporary), matching the discipline the rest of these helpers keep.
    pub(super) async fn task_for_connection(&self, conn_id: &str) -> Option<((i32, i32), bool)> {
        if let Some(entry) = self.index.lock().await.get(conn_id).copied() {
            return Some((entry, true));
        }
        let mut cursor = conn_id.to_string();
        for _ in 0..MAX_DELEGATION_CHAIN_HOPS {
            let parent = self.delegation_parents.lock().await.get(&cursor).cloned()?;
            if let Some(entry) = self.index.lock().await.get(&parent).copied() {
                return Some((entry, false));
            }
            cursor = parent;
        }
        None
    }

    /// Detach `root`'s delegation-child mappings, transitively, and return every
    /// connection id removed (`root` itself only when `include_root`).
    ///
    /// Transitive because a sub-agent may have delegated further: dropping only
    /// the direct children would strand the deeper links, and — since
    /// [`Self::task_for_connection`] deliberately doesn't prune as it walks —
    /// nothing else would ever collect them.
    ///
    /// Terminates unconditionally: each iteration removes the entries it
    /// enqueues, so the map strictly shrinks.
    async fn detach_delegation_subtree(&self, root: &str, include_root: bool) -> Vec<String> {
        let mut parents = self.delegation_parents.lock().await;
        let mut detached = Vec::new();
        let mut frontier = vec![root.to_string()];
        while let Some(node) = frontier.pop() {
            let children: Vec<String> = parents
                .iter()
                .filter(|(_, parent)| parent.as_str() == node)
                .map(|(child, _)| child.clone())
                .collect();
            for child in children {
                parents.remove(&child);
                frontier.push(child.clone());
                detached.push(child);
            }
        }
        if include_root && parents.remove(root).is_some() {
            detached.push(root.to_string());
        }
        detached
    }

    /// Track any blocking prompt the child is ALREADY parked on, reading its
    /// live `SessionState` (authoritative, and updated independently of the
    /// event bus). Idempotent: re-tracking a key already in the set is a
    /// no-op insert, so a prompt we did see plus this backfill can't
    /// double-count.
    ///
    /// Reads all three prompt kinds rather than the single top-precedence one,
    /// so resolving whichever comes first can't empty the set while another is
    /// still outstanding.
    async fn backfill_child_blocking_prompts(&self, child_conn_id: &str) {
        let Some(state) = self.manager.get_state(child_conn_id).await else {
            return;
        };
        // Collect under the read lock, track after releasing it — `track_request`
        // takes the engine's own maps and must not nest inside a session lock.
        let keys: Vec<String> = {
            let s = state.read().await;
            let mut keys = Vec::new();
            if let Some(p) = s.pending_permission.as_ref() {
                keys.push(format!("p:{}", p.request_id));
            }
            if let Some(q) = s.pending_question.as_ref() {
                keys.push(format!("q:{}", q.question_id));
            }
            if let Some(a) = s.pending_plan_approval.as_ref() {
                keys.push(format!("a:{}", a.approval_id));
            }
            keys
        };
        for key in keys {
            self.track_request(child_conn_id, key, true).await;
        }
    }

    /// Drop every delegation-child mapping belonging to a retired run, and
    /// return the connection ids that were detached — their outstanding
    /// requests are now unanswerable, so whoever does not clear the task's set
    /// wholesale has to retract those keys by hand (see
    /// [`Self::retire_connection`]).
    pub(super) async fn forget_delegation_children_of(&self, parent_conn_id: &str) -> Vec<String> {
        // `parent_conn_id` is a run connection, never a key in this map.
        self.detach_delegation_subtree(parent_conn_id, false).await
    }

    /// Drop every engine-side trace of a run connection: its `index` entry, the
    /// task's outstanding-request set, and its delegation children.
    ///
    /// `index` is what attributes an incoming event to a task, so an entry that
    /// outlives its connection keeps answering for a row that has moved on —
    /// and `remove_worktree_locked` reads the same map as "a live agent is
    /// working in there". Whoever takes over from a settle that will never
    /// arrive calls this; the entry has no other way out (`reconcile_once`
    /// only ever looks at `running` / `awaiting_input` rows).
    pub(super) async fn retire_connection(&self, conn_id: &str, task_id: i32) {
        // A connection that is being retired will never complete another turn,
        // so an unspent compaction marker on it would linger for the life of
        // the process. (`claim_compaction_turn` clears every marker whose
        // connection does complete one.)
        self.compaction_turns.lock().await.remove(conn_id);

        // Unmap this run's delegation children FIRST: the purge below needs
        // their ids, and a child that is already detached can no longer publish
        // a fresh key (`track_request` resolves a child through
        // `delegation_parents`, and an unmapped one resolves to nothing).
        let orphaned: Vec<String> = self
            .forget_delegation_children_of(conn_id)
            .await
            .iter()
            .map(|child| format!("{child}#"))
            .collect();

        // The outstanding-request set is keyed by TASK, not by connection, so
        // clearing it while another generation still owns the row would drop
        // ITS pending permissions — resolving one of the survivors then empties
        // an already-empty set and flips the card back to `running` with the
        // agent still parked. Hence "was this the last connection on the task",
        // and hence the `index` guard held ACROSS the clear rather than just
        // the test: a launch registers its generation in `index` before it can
        // publish any request (and a delegation child's keys only exist while
        // its parent is indexed), so holding it is what keeps the answer true
        // until the set is gone. Lock order is index → awaiting; every other
        // `awaiting` critical section is a leaf, so it cannot invert.
        let emptied_for = {
            let mut index = self.index.lock().await;
            index.remove(conn_id);
            let survivor = index
                .values()
                .find(|(tid, _)| *tid == task_id)
                .map(|(_, run_seq)| *run_seq);
            let mut awaiting = self.awaiting.lock().await;
            match survivor {
                // Last one out, so the whole set goes — orphans included. A set
                // inherited by the next generation would never flip the row to
                // `awaiting_input` again.
                None => {
                    awaiting.remove(&task_id);
                    None
                }
                // Someone else still owns the row: keep THEIR requests, but
                // retract this run's children's. Sparing those is the opposite
                // error and the worse one — the chain that named them is gone,
                // so neither `track_request` nor `forget_delegation_child` can
                // resolve them any more, and one key stuck in a shared set pins
                // the survivor on the wrong side of the `running` ⇄
                // `awaiting_input` edge until something clears the set
                // wholesale. Only a LATER retirement that is the last one out
                // (or a cancel) does that, so it outlasts every generation the
                // orphan is actually lying about.
                Some(run_seq) => Self::retract_keys(&mut awaiting, task_id, &orphaned)
                    .then_some(run_seq),
            }
        };

        // Emptied by the retraction alone: the row is parked on requests that
        // just became unanswerable, so return it to the survivor's `running`
        // through the same flip `track_request` would have used.
        if let Some(run_seq) = emptied_for {
            let flipped = work_task_service::flip_awaiting(&self.db.conn, task_id, run_seq, false)
                .await
                .unwrap_or(false);
            if flipped {
                self.emit_upsert(task_id);
            }
        }
    }

    /// Drop every key under `prefixes` from the task's outstanding set,
    /// reporting whether that is what emptied it (and so owes a flip back to
    /// `running`). An untouched set never reports `true`, however empty.
    fn retract_keys(
        awaiting: &mut HashMap<i32, HashSet<String>>,
        task_id: i32,
        prefixes: &[String],
    ) -> bool {
        if prefixes.is_empty() {
            return false;
        }
        let Some(set) = awaiting.get_mut(&task_id) else {
            return false;
        };
        let before = set.len();
        set.retain(|k| !prefixes.iter().any(|p| k.starts_with(p.as_str())));
        if set.len() == before {
            return false; // nothing of this subtree's was outstanding
        }
        if set.is_empty() {
            awaiting.remove(&task_id);
            return true;
        }
        false
    }

    /// Drop a finished delegation child (and anything it delegated in turn) plus
    /// any blocking requests they left unanswered.
    ///
    /// The cleanup is the load-bearing half: a child torn down while a
    /// permission was still pending (cancel, crash, parent teardown) would
    /// otherwise leave its key in the task's outstanding set forever, pinning
    /// the row at `awaiting_input` with nothing left that could ever resolve it.
    /// Empties the set through the same flip path as `track_request` so the row
    /// returns to `running`.
    async fn forget_delegation_child(&self, child_conn_id: &str) {
        // Resolve the run BEFORE detaching — afterwards the chain is gone.
        let entry = self.task_for_connection(child_conn_id).await;
        let detached = self.detach_delegation_subtree(child_conn_id, true).await;
        let Some(((task_id, run_seq), _)) = entry else {
            return;
        };
        let prefixes: Vec<String> = detached.iter().map(|c| format!("{c}#")).collect();
        let emptied = {
            let mut awaiting = self.awaiting.lock().await;
            Self::retract_keys(&mut awaiting, task_id, &prefixes)
        };
        if emptied {
            let flipped = work_task_service::flip_awaiting(&self.db.conn, task_id, run_seq, false)
                .await
                .unwrap_or(false);
            if flipped {
                self.emit_upsert(task_id);
            }
        }
    }
}
