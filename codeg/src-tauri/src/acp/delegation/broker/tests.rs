    use super::*;
    use crate::acp::delegation::spawner::{mock::MockSpawner, ResumedSpawn, SpawnerError};
    use crate::acp::delegation::types::DelegationSuccess;
    use crate::models::AgentType;

    /// Test-only `ConversationDepthLookup` that resolves against a flat
    /// (id, parent_id) table. Unknown ids return `Ok(None)` to keep test
    /// setup small.
    struct MockDepth(Vec<(i32, Option<i32>)>);

    #[async_trait]
    impl ConversationDepthLookup for MockDepth {
        async fn parent_of(&self, id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(self.0.iter().find(|(c, _)| *c == id).and_then(|(_, p)| *p))
        }
    }

    fn shallow_lookup() -> Arc<dyn ConversationDepthLookup> {
        // parent conversation is the root — depth = 0, no rejection.
        Arc::new(MockDepth(vec![(1, None)])) as Arc<dyn ConversationDepthLookup>
    }

    fn request(parent_conv: i32, tool_use: &str) -> DelegationRequest {
        DelegationRequest {
            parent_connection_id: "parent-conn".into(),
            parent_conversation_id: parent_conv,
            parent_tool_use_id: tool_use.into(),
            agent_type: AgentType::ClaudeCode,
            task: "do x".into(),
            working_dir: None,
            requested_working_dir: None,
            external_handle: None,
        }
    }

    fn request_with_handle(parent_conv: i32, tool_use: &str, handle: &str) -> DelegationRequest {
        let mut r = request(parent_conv, tool_use);
        r.external_handle = Some(handle.to_string());
        r
    }

    /// Bring the broker's `enabled` switch up before driving any test that
    /// hits `handle_request`. Production now defaults to `enabled: false`,
    /// so a bare `DelegationBroker::new(...)` would short-circuit before
    /// parking a pending entry. Tests that assert disabled behavior set
    /// their own config explicitly and skip this helper.
    async fn enable_delegation(broker: &DelegationBroker) {
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
    }

    // -- Task 4.3 -----------------------------------------------------------

    #[tokio::test]
    async fn config_round_trip() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .set_config(DelegationConfig {
                enabled: false,
                depth_limit: 5,
                ..DelegationConfig::default()
            })
            .await;
        let got = broker.config_snapshot().await;
        assert!(!got.enabled);
        assert_eq!(got.depth_limit, 5);
    }

    #[tokio::test]
    async fn disabled_returns_canceled_without_touching_spawner() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        broker
            .set_config(DelegationConfig {
                enabled: false,
                depth_limit: 2,
                ..DelegationConfig::default()
            })
            .await;
        let outcome = broker.handle_request(request(1, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            _ => panic!("expected Err"),
        }
        assert!(mock.disconnects.lock().await.is_empty());
    }

    // -- Task 4.4: happy path ----------------------------------------------

    #[tokio::test]
    async fn happy_path_returns_ok_after_complete_call() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };

        // Spin until the broker has registered the pending call so the test
        // doesn't race the spawn/send awaits.
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };

        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "4".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 50,
                    token_usage: None,
                }),
            )
            .await;

        let outcome = driver.await.unwrap();
        match outcome {
            DelegationOutcome::Ok(s) => {
                assert_eq!(s.text, "4");
                assert_eq!(s.child_conversation_id, 42);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        assert_eq!(broker.pending_count().await, 0);
        // complete_call disconnects the child once.
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["child-conn-1"]);
    }

    /// `StatusWait::Infinite` (the explicit `wait_ms = 0` escape hatch) must
    /// park while the task is still running rather than returning the running
    /// snapshot, then resolve to the terminal report once the task completes —
    /// no matter how long the child takes.
    #[tokio::test]
    async fn infinite_wait_parks_until_terminal() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let ack = broker.start_delegation(request(1, "pt-1")).await;
        assert_eq!(ack.status, TaskStatus::Running);
        let task_id = ack.task_id.clone().expect("running task carries an id");

        // Infinite wait: parks on the completion signal instead of returning
        // the still-running snapshot.
        let waiter = {
            let broker = broker.clone();
            let task_id = task_id.clone();
            tokio::spawn(async move {
                broker
                    .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Infinite)
                    .await
            })
        };

        // Give the waiter a beat — it must still be parked, not finished.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !waiter.is_finished(),
            "infinite wait must park while the task is running"
        );

        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;

        let report = waiter.await.unwrap();
        assert_eq!(report.status, TaskStatus::Completed);
        assert_eq!(report.text.as_deref(), Some("done"));
    }

    /// A running snapshot upgrades its bare `"Running."` message with the child's
    /// latest one-line reply when the live-reply lookup has one.
    #[tokio::test]
    async fn running_status_appends_live_reply_when_available() {
        use crate::acp::delegation::live_reply::mock::MockChildLiveReplyLookup;

        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup())
                .with_live_reply_lookup(Arc::new(MockChildLiveReplyLookup::new(Some(
                    "Reading config.rs".into(),
                ))));
        enable_delegation(&broker).await;

        let ack = broker.start_delegation(request(1, "pt-1")).await;
        assert_eq!(ack.status, TaskStatus::Running);
        let task_id = ack.task_id.clone().expect("running task carries an id");

        let report = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Immediate)
            .await;
        assert_eq!(report.status, TaskStatus::Running);
        // The live hint lands on its own line so a content-only host can anchor
        // "still running" to the standalone first line "Running.".
        assert_eq!(
            report.message.as_deref(),
            Some("Running.\nLatest sub-agent reply: Reading config.rs")
        );
    }

    /// With no live reply (default Noop lookup / child produced nothing yet) the
    /// running snapshot stays the bare `"Running."`.
    #[tokio::test]
    async fn running_status_stays_bare_without_live_reply() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let ack = broker.start_delegation(request(1, "pt-1")).await;
        let task_id = ack.task_id.clone().expect("running task carries an id");

        let report = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Immediate)
            .await;
        assert_eq!(report.status, TaskStatus::Running);
        assert_eq!(report.message.as_deref(), Some("Running."));
    }

    // -- Blocked sub-agents (#447) -----------------------------------------

    fn permission_block(request_id: &str, title: &str) -> BlockedOn {
        BlockedOn {
            kind: BlockedKind::Permission,
            request_id: request_id.into(),
            title: Some(title.into()),
        }
    }

    /// Spin up a broker whose children all report `blocked` (settable later) and
    /// one running task on it. Returns `(broker, lookup, task_id)`.
    async fn broker_with_blockable_child() -> (
        DelegationBroker,
        Arc<crate::acp::delegation::live_reply::mock::MockChildLiveReplyLookup>,
        String,
    ) {
        // The production interval is minutes; every test but the valve one
        // wants "effectively never re-surfaces during this test".
        broker_with_blockable_child_resurfacing(Duration::from_secs(3600)).await
    }

    async fn broker_with_blockable_child_resurfacing(
        resurface: Duration,
    ) -> (
        DelegationBroker,
        Arc<crate::acp::delegation::live_reply::mock::MockChildLiveReplyLookup>,
        String,
    ) {
        use crate::acp::delegation::live_reply::mock::MockChildLiveReplyLookup;

        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let lookup = Arc::new(MockChildLiveReplyLookup::new(Some("Reading x".into())));
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup())
                .with_live_reply_lookup(lookup.clone())
                .with_block_resurface(resurface);
        enable_delegation(&broker).await;
        let task_id = broker
            .start_delegation(request(1, "pt-1"))
            .await
            .task_id
            .expect("running task carries an id");
        (broker, lookup, task_id)
    }

    /// A child parked on a permission reports `Running` + `blocked_on`, and its
    /// message says so INSTEAD of the (now stale) live reply.
    #[tokio::test]
    async fn blocked_child_reports_blocked_on_over_live_reply() {
        let (broker, lookup, task_id) = broker_with_blockable_child().await;
        lookup.set_blocked(Some(permission_block("req-1", "rm -rf /tmp/x")));

        let report = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Immediate)
            .await;

        // Status stays wire-stable `running` — the block rides alongside it, so
        // a consumer that predates the field still resolves the poll correctly.
        assert_eq!(report.status, TaskStatus::Running);
        let blocked = report.blocked_on.expect("blocked_on is set");
        assert_eq!(blocked.kind, BlockedKind::Permission);
        assert_eq!(blocked.request_id, "req-1");
        let message = report.message.expect("blocked report carries a message");
        // First line must stay the bare running marker (content-only hosts
        // anchor on it), with the note on its own second line.
        let mut lines = message.lines();
        assert_eq!(lines.next(), Some("Running."));
        assert_eq!(
            lines.next(),
            Some("Awaiting your decision: rm -rf /tmp/x"),
            "the blocked note replaces the live reply, which is stale by definition"
        );
        assert!(!message.contains("Latest sub-agent reply"));
    }

    /// The whole point of #447: a `wait_ms: 0` (Infinite) wait must NOT park
    /// forever behind a permission prompt. Nothing settles this task — if the
    /// block didn't wake it, this test would hang.
    #[tokio::test]
    async fn infinite_wait_returns_when_the_child_becomes_blocked() {
        let (broker, lookup, task_id) = broker_with_blockable_child().await;
        lookup.set_blocked(Some(permission_block("req-1", "write to /etc/hosts")));

        let report = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Infinite)
            .await;

        assert_eq!(report.status, TaskStatus::Running);
        assert!(report.blocked_on.is_some());
    }

    /// ...but each prompt is surfaced ONCE. A re-poll while the same prompt is
    /// still unanswered goes back to waiting instead of returning instantly,
    /// which is what keeps an LLM's wait loop from spinning. A *second*,
    /// different prompt is new again and returns.
    #[tokio::test]
    async fn a_reported_block_does_not_return_twice() {
        let (broker, lookup, task_id) = broker_with_blockable_child().await;
        lookup.set_blocked(Some(permission_block("req-1", "first")));

        // First observation returns.
        let first = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Infinite)
            .await;
        assert_eq!(first.blocked_on.map(|b| b.request_id).as_deref(), Some("req-1"));

        // Same prompt, still unanswered: a bounded wait now runs to its deadline
        // rather than returning immediately.
        let started = Instant::now();
        let second = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Bounded(120))
            .await;
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "an already-reported block must not short-circuit the wait"
        );
        // It still REPORTS the block on the way out — only the early return is
        // suppressed, never the information.
        assert!(second.blocked_on.is_some());

        // A different prompt is a new event and returns early again.
        lookup.set_blocked(Some(permission_block("req-2", "second")));
        let started = Instant::now();
        let third = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Bounded(5_000))
            .await;
        assert!(
            started.elapsed() < Duration::from_millis(2_000),
            "a NEW blocking prompt must wake the wait"
        );
        assert_eq!(third.blocked_on.map(|b| b.request_id).as_deref(), Some("req-2"));
    }

    /// The delivery-failure valve: marking a block reported is not proof the
    /// parent RECEIVED it (the report still has to cross the listener, the
    /// companion's stdout and the agent CLI, and a cancelled or aborted
    /// `tools/call` discards it silently). So the SAME unanswered prompt
    /// becomes reportable again after the resurface interval — otherwise that
    /// one lost report was the only one ever offered and the caller is stranded
    /// exactly as before #447.
    ///
    /// Uses `Infinite`, which has no deadline of its own: if the park weren't
    /// bounded by the pending resurface, nothing would ever wake it (a blocked
    /// child emits nothing further) and this test would hang.
    #[tokio::test]
    async fn an_undelivered_block_resurfaces_after_the_interval() {
        let (broker, lookup, task_id) =
            broker_with_blockable_child_resurfacing(Duration::from_millis(150)).await;
        lookup.set_blocked(Some(permission_block("req-1", "first")));

        // First report — imagine this one never reaches the LLM.
        let first = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Infinite)
            .await;
        assert!(first.blocked_on.is_some());

        let started = Instant::now();
        let second = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Infinite)
            .await;
        assert_eq!(
            second.blocked_on.map(|b| b.request_id).as_deref(),
            Some("req-1"),
            "the same unanswered prompt must become reportable again"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(120),
            "it must WAIT out the interval, not return instantly (that would be the spin)"
        );
    }

    /// An unblocked child is unchanged: no `blocked_on`, live reply intact, and
    /// an Infinite wait still parks (proven by a Bounded wait running its full
    /// deadline out).
    #[tokio::test]
    async fn unblocked_child_still_parks_and_keeps_its_live_reply() {
        let (broker, _lookup, task_id) = broker_with_blockable_child().await;

        let started = Instant::now();
        let report = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Bounded(120))
            .await;
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(report.blocked_on.is_none());
        assert_eq!(
            report.message.as_deref(),
            Some("Running.\nLatest sub-agent reply: Reading x")
        );
    }

    // -- Batch get_tasks_status --------------------------------------------

    /// Queue one spawn+send pair and start a delegation, returning its task id.
    /// Each call consumes one queued `(spawn, send)` from the mock.
    async fn start_running(
        broker: &DelegationBroker,
        mock: &MockSpawner,
        child_conn: &str,
        child_conv: i32,
        tool_use: &str,
    ) -> String {
        mock.queue_spawn(Ok(child_conn.into())).await;
        mock.queue_send(Ok(child_conv)).await;
        broker
            .start_delegation(request(1, tool_use))
            .await
            .task_id
            .expect("running task carries an id")
    }

    /// The single-id batch agrees with `get_task_status` for a completed task —
    /// the refactor that routes the single path through `get_tasks_status` keeps
    /// the historical contract.
    #[tokio::test]
    async fn get_tasks_status_single_matches_get_task_status() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let t1 = start_running(&broker, &mock, "child-1", 42, "pt-1").await;
        broker
            .complete_call(
                &t1,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 7,
                    token_usage: None,
                }),
            )
            .await;

        let single = broker
            .get_task_status("parent-conn", Some(1), &t1, StatusWait::Immediate)
            .await;
        let batch = broker
            .get_tasks_status(
                "parent-conn",
                Some(1),
                std::slice::from_ref(&t1),
                StatusWait::Immediate,
            )
            .await;
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].status, single.status);
        assert_eq!(batch[0].text, single.text);
        assert_eq!(batch[0].task_id, single.task_id);
    }

    /// An immediate batch poll resolves a mix of completed / running / unknown
    /// tasks in ONE pass, preserving request order.
    #[tokio::test]
    async fn batch_status_immediate_mixed_preserves_order() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let t1 = start_running(&broker, &mock, "child-1", 1, "pt-1").await;
        let t2 = start_running(&broker, &mock, "child-2", 2, "pt-2").await;
        broker
            .complete_call(
                &t1,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "first".into(),
                    child_conversation_id: 1,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 3,
                    token_usage: None,
                }),
            )
            .await;

        let ids = vec![t1.clone(), t2.clone(), "no-such-id".to_string()];
        let reports = broker
            .get_tasks_status("parent-conn", Some(1), &ids, StatusWait::Immediate)
            .await;
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].status, TaskStatus::Completed);
        assert_eq!(reports[0].text.as_deref(), Some("first"));
        assert_eq!(reports[0].task_id.as_deref(), Some(t1.as_str()));
        assert_eq!(reports[1].status, TaskStatus::Running);
        assert_eq!(reports[1].task_id.as_deref(), Some(t2.as_str()));
        assert_eq!(reports[2].status, TaskStatus::Unknown);
    }

    /// A batch `Infinite` wait returns as soon as ANY requested task settles,
    /// leaving the still-running siblings in the snapshot.
    #[tokio::test]
    async fn batch_infinite_returns_when_any_settles() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let t1 = start_running(&broker, &mock, "child-1", 1, "pt-1").await;
        let t2 = start_running(&broker, &mock, "child-2", 2, "pt-2").await;

        let waiter = {
            let broker = broker.clone();
            let ids = vec![t1.clone(), t2.clone()];
            tokio::spawn(async move {
                broker
                    .get_tasks_status("parent-conn", Some(1), &ids, StatusWait::Infinite)
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !waiter.is_finished(),
            "batch infinite wait must park while both tasks run"
        );

        broker
            .complete_call(
                &t1,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "first-done".into(),
                    child_conversation_id: 1,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 4,
                    token_usage: None,
                }),
            )
            .await;

        let reports = waiter.await.unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].status, TaskStatus::Completed);
        assert_eq!(reports[0].text.as_deref(), Some("first-done"));
        assert_eq!(reports[1].status, TaskStatus::Running);
    }

    /// A batch `Infinite` wait must NOT hold an already-terminal result hostage
    /// to a still-running sibling: when a task is terminal at call ENTRY (it
    /// completed before the poll), return immediately with the current snapshot
    /// rather than parking for the runner. This is the mixed-at-entry case the
    /// transition-only wake used to miss — distinct from
    /// [`batch_infinite_returns_when_any_settles`] (both running at entry).
    #[tokio::test]
    async fn batch_infinite_returns_immediately_when_one_already_terminal() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let t1 = start_running(&broker, &mock, "child-1", 1, "pt-1").await;
        let t2 = start_running(&broker, &mock, "child-2", 2, "pt-2").await;
        // t1 completes BEFORE the poll; t2 keeps running.
        broker
            .complete_call(
                &t1,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "first-done".into(),
                    child_conversation_id: 1,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 4,
                    token_usage: None,
                }),
            )
            .await;

        // Infinite wait, but one task is already terminal at entry → must return
        // at once (a bounded timeout guards against the regression: parking here
        // would block until t2 settles, which it never does in this test).
        let ids = vec![t1.clone(), t2.clone()];
        let reports = tokio::time::timeout(
            Duration::from_secs(2),
            broker.get_tasks_status("parent-conn", Some(1), &ids, StatusWait::Infinite),
        )
        .await
        .expect("a batch with an already-terminal task must not park under Infinite");
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].status, TaskStatus::Completed);
        assert_eq!(reports[0].text.as_deref(), Some("first-done"));
        assert_eq!(reports[1].status, TaskStatus::Running);
    }

    /// A batch `Infinite` wait where NOTHING is running (all ids unknown) must
    /// return immediately rather than parking forever.
    #[tokio::test]
    async fn batch_infinite_all_settled_returns_immediately() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let ids = vec!["nope-1".to_string(), "nope-2".to_string()];
        let reports = tokio::time::timeout(
            Duration::from_secs(2),
            broker.get_tasks_status("parent-conn", Some(1), &ids, StatusWait::Infinite),
        )
        .await
        .expect("all-settled infinite batch must not hang");
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|r| r.status == TaskStatus::Unknown));
    }

    /// A bounded batch wait with no completion returns the running snapshot once
    /// the deadline elapses (the child keeps running; the caller re-polls).
    #[tokio::test]
    async fn batch_bounded_deadline_returns_running_snapshot() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let t1 = start_running(&broker, &mock, "child-1", 1, "pt-1").await;
        let reports = broker
            .get_tasks_status("parent-conn", Some(1), &[t1], StatusWait::Bounded(40))
            .await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].status, TaskStatus::Running);
    }

    /// A task owned by a different parent reports `Unknown` in a batch — never
    /// leaking another parent's task, just like the single-task path.
    #[tokio::test]
    async fn batch_status_scopes_to_parent() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let t1 = start_running(&broker, &mock, "child-1", 1, "pt-1").await;
        let reports = broker
            .get_tasks_status("other-parent", Some(2), &[t1], StatusWait::Immediate)
            .await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].status, TaskStatus::Unknown);
    }

    // -- Task 4.5: error paths ---------------------------------------------

    #[tokio::test]
    async fn spawn_failure_maps_to_spawn_failed() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Err(SpawnerError::Spawn("nope".into())))
            .await;
        let broker = DelegationBroker::new(mock as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let outcome = broker.handle_request(request(1, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "spawn_failed"),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn agent_defaults_are_forwarded_to_spawner() {
        // Configure broker with per-agent defaults for ClaudeCode and verify
        // they reach the spawner. Other agent types should still get the
        // empty/None defaults.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-1".into())).await;
        mock.queue_send(Err(SpawnerError::Send("stop after spawn".into())))
            .await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());

        let mut claude_cfg = BTreeMap::new();
        claude_cfg.insert("model".into(), "claude-sonnet-4-5".into());
        let mut agent_defaults = BTreeMap::new();
        agent_defaults.insert(
            AgentType::ClaudeCode,
            AgentDelegationDefaults {
                mode_id: Some("auto".into()),
                config_values: claude_cfg.clone(),
            },
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 8,
                agent_defaults,
                ..DelegationConfig::default()
            })
            .await;

        let _ = broker.handle_request(request(1, "pt-1")).await;

        let args = mock.spawn_args.lock().await;
        assert_eq!(args.len(), 1);
        let call = &args[0];
        assert_eq!(call.agent_type, AgentType::ClaudeCode);
        assert_eq!(call.preferred_mode_id.as_deref(), Some("auto"));
        assert_eq!(call.preferred_config_values, claude_cfg);
    }

    #[tokio::test]
    async fn agent_with_no_defaults_gets_empty_preferred_args() {
        // ClaudeCode is configured in agent_defaults; a Codex request should
        // still receive (None, empty) — no cross-contamination.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-1".into())).await;
        mock.queue_send(Err(SpawnerError::Send("stop after spawn".into())))
            .await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());

        let mut agent_defaults = BTreeMap::new();
        agent_defaults.insert(
            AgentType::ClaudeCode,
            AgentDelegationDefaults {
                mode_id: Some("auto".into()),
                config_values: BTreeMap::new(),
            },
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 8,
                agent_defaults,
                ..DelegationConfig::default()
            })
            .await;

        let mut codex_req = request(1, "pt-1");
        codex_req.agent_type = AgentType::Codex;
        let _ = broker.handle_request(codex_req).await;

        let args = mock.spawn_args.lock().await;
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].agent_type, AgentType::Codex);
        assert!(args[0].preferred_mode_id.is_none());
        assert!(args[0].preferred_config_values.is_empty());
    }

    #[tokio::test]
    async fn send_failure_after_spawn_disconnects_child() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Err(SpawnerError::Send("agent rejected prompt".into())))
            .await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let outcome = broker.handle_request(request(1, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "spawn_failed"),
            other => panic!("expected Err, got {other:?}"),
        }
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c1"]);
    }

    #[tokio::test]
    async fn handle_request_waits_indefinitely_for_completion() {
        // No timeout race anymore: handle_request blocks on `rx.await` until
        // complete_call / cancel_* fires. This test asserts the pending entry
        // sticks around even after a generous idle window.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(99)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };

        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(broker.pending_count().await, 1);
        assert!(mock.cancels.lock().await.is_empty());

        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 99,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 50,
                    token_usage: None,
                }),
            )
            .await;

        let outcome = driver.await.unwrap();
        match outcome {
            DelegationOutcome::Ok(s) => assert_eq!(s.text, "done"),
            other => panic!("expected Ok, got {other:?}"),
        }
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c1"]);
    }

    // -- Task 4.6: parent-cancel cascade -----------------------------------

    #[tokio::test]
    async fn parent_cancel_cancels_all_pending_children() {
        let mock = Arc::new(MockSpawner::new());
        for i in 0..3 {
            mock.queue_spawn(Ok(format!("c{i}"))).await;
            mock.queue_send(Ok(100 + i)).await;
        }
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let mut handles = Vec::new();
        for i in 0..3 {
            let broker = broker.clone();
            handles.push(tokio::spawn(async move {
                broker.handle_request(request(1, &format!("pt-{i}"))).await
            }));
        }

        // Wait until all three are parked.
        while broker.pending_count().await < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        broker.cancel_by_parent("parent-conn").await;
        for h in handles {
            let outcome = h.await.unwrap();
            match outcome {
                DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
                other => panic!("expected canceled, got {other:?}"),
            }
        }
        assert_eq!(mock.cancels.lock().await.len(), 3);
        // Each child disconnects exactly once via cancel_by_parent.
        assert_eq!(mock.disconnects.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn cancel_by_parent_ignores_other_parents() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(200)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        broker.cancel_by_parent("other-parent").await;
        // No effect — pending entry still there.
        assert_eq!(broker.pending_count().await, 1);

        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 200,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 10,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    // -- Task 4.7: depth limit ---------------------------------------------

    #[tokio::test]
    async fn depth_limit_rejects_before_spawn() {
        let mock = Arc::new(MockSpawner::new());
        // No queued spawn results — if the broker tries to spawn, it errors loudly.
        // chain: 1 (root, None) <- 2 (child of 1) <- 3 (grandchild of 2).
        // Parent = grandchild (id 3): parent_depth = 2. With limit = 2, child
        // would sit at depth 3 → reject.
        let lookup = Arc::new(MockDepth(vec![(1, None), (2, Some(1)), (3, Some(2))]))
            as Arc<dyn ConversationDepthLookup>;
        let broker = DelegationBroker::new(mock as Arc<dyn ConnectionSpawner>, lookup);
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 2,
                ..DelegationConfig::default()
            })
            .await;
        let outcome = broker.handle_request(request(3, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "depth_limit"),
            other => panic!("expected depth_limit, got {other:?}"),
        }
    }

    // -- Pending tool_call_id queue (MCP `_meta.tool_use_id` fallback) ----

    #[tokio::test]
    async fn pending_tool_call_register_and_take_is_fifo() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        broker.register_pending_tool_call("p1", "tc-b".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-a")
        );
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-b")
        );
        assert!(broker.take_pending_tool_call("p1").await.is_none());
    }

    #[tokio::test]
    async fn register_dedupes_repeated_tool_call_id() {
        // Regression: some hosts re-emit `sessionUpdate(tool_call)` (not
        // `tool_call_update`) for the same call as raw_input chunks arrive
        // or as the status flips. Without dedupe the second push leaves a
        // stale id in the queue that mis-binds the next delegation.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-a")
        );
        assert!(
            broker.take_pending_tool_call("p1").await.is_none(),
            "duplicate register must not leave a stale id in the queue"
        );
    }

    #[tokio::test]
    async fn register_after_claim_drops_stale_re_emit() {
        // Regression for the post-claim re-emit race: a host re-sends
        // `sessionUpdate(tool_call)` for the same id after the matching
        // MCP round-trip already consumed it (e.g. shipping the
        // `completed` status flip or a settled `raw_input`). The
        // in-queue dedupe alone leaves the queue empty at that moment,
        // so without the recently-consumed memory the re-emit would
        // sneak into the queue and mis-bind the next delegation.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-a")
        );
        // Re-emit of the same id after it was already claimed.
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        assert!(
            broker.take_pending_tool_call("p1").await.is_none(),
            "post-claim re-emit of the same id must not be re-queued"
        );
        // A genuinely new id on the same parent still flows through.
        broker.register_pending_tool_call("p1", "tc-b".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-b")
        );
    }

    #[tokio::test]
    async fn concurrent_take_and_re_register_never_leaks_stale_duplicate() {
        // TOCTOU regression: a host re-emit of the same tool_call_id
        // racing against the matching take must never inject a stale
        // duplicate. Co-locating `pending` and `consumed` under the
        // same mutex guarantees the claim → mark-consumed pair is
        // atomic, so the only two legal interleavings are:
        //
        //   * take wins → pending=[], consumed=[id]; re-register sees
        //     the id in consumed and drops it.
        //   * register wins → pending=[id] (still the original entry,
        //     in-queue dedupe drops the re-emit); take then pops it
        //     and records it in consumed.
        //
        // In neither case may the queue retain a duplicate id once
        // both futures settle. We drive many rounds with `tokio::spawn`
        // to stress the interleaving.
        let broker = std::sync::Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        ));
        for _ in 0..200 {
            broker.register_pending_tool_call("p1", "tc-a".into()).await;
            let b_take = broker.clone();
            let b_reg = broker.clone();
            let h_take = tokio::spawn(async move {
                b_take.take_pending_tool_call("p1").await;
            });
            let h_reg = tokio::spawn(async move {
                b_reg.register_pending_tool_call("p1", "tc-a".into()).await;
            });
            let _ = tokio::join!(h_take, h_reg);
            assert!(
                broker.take_pending_tool_call("p1").await.is_none(),
                "stale duplicate of tc-a leaked after concurrent take + re-register"
            );
        }
    }

    #[tokio::test]
    async fn consumed_memory_outlives_pending_ttl_for_long_running_delegation() {
        // Regression: a delegated child agent can run for
        // minutes-to-hours. When it finishes, the host may re-emit
        // the parent-side `tool_call` (e.g. as a `completed` status
        // flip via the non-update `ToolCall` variant). That re-emit
        // arrives well after PENDING_TOOL_CALL_TTL, so the consumed
        // memory MUST NOT age out under that TTL — otherwise the
        // stale id slips back into pending and mis-binds the next
        // delegation. Consumed entries are scoped to the parent
        // connection's lifetime instead.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-a")
        );
        // Simulate the host re-emitting the same tool_call_id 10×
        // the pending TTL later (i.e. a long-running delegation that
        // finishes after the pending eviction window).
        let long_after = Instant::now() + PENDING_TOOL_CALL_TTL * 10;
        broker
            .register_pending_tool_call_with_key_at("p1", "tc-a".into(), None, false, long_after)
            .await;
        assert!(
            broker
                .take_pending_tool_call_at("p1", long_after)
                .await
                .is_none(),
            "consumed memory must outlast the pending TTL so terminal status re-emits cannot leak through"
        );
    }

    #[tokio::test]
    async fn consumed_memory_unbounded_across_high_fan_out() {
        // Regression for the cap removal: a parent session with many
        // delegations (well past any prior per-bucket cap) must still
        // reject a late re-emit of the very first delegation's id,
        // because the consumed half has no cap. A bounded consumed
        // set with FIFO eviction would silently re-enable the
        // mis-binding bug at high fan-out.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        let first_id = "tc-first".to_string();
        broker
            .register_pending_tool_call("p1", first_id.clone())
            .await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some(first_id.as_str())
        );
        // Issue many more delegations than any prior per-bucket cap. With
        // no cap on consumed, the first id must remain remembered for the
        // lifetime of the parent connection.
        for i in 0..128 {
            let id = format!("tc-{i}");
            broker.register_pending_tool_call("p1", id.clone()).await;
            assert_eq!(
                broker.take_pending_tool_call("p1").await.as_deref(),
                Some(id.as_str())
            );
        }
        // Late re-emit of the very first id (would have been evicted
        // by the prior bounded consumed FIFO).
        broker
            .register_pending_tool_call("p1", first_id.clone())
            .await;
        assert!(
            broker.take_pending_tool_call("p1").await.is_none(),
            "consumed memory must retain the very first id even after high fan-out"
        );
    }

    #[tokio::test]
    async fn consumed_memory_cleared_on_parent_disconnect() {
        // The companion to the long-running invariant above: consumed
        // memory is scoped to the parent connection's lifetime, so
        // `drop_pending_tool_calls_for_parent` (called when the
        // parent disconnects) must clear it. Otherwise a brand-new
        // connection reusing the same id (UUID collision is unlikely
        // but UUIDs are not the only id scheme in play) would be
        // permanently blocked.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-a")
        );
        broker.drop_pending_tool_calls_for_parent("p1").await;
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-a"),
            "parent disconnect must clear consumed memory so id reuse is acceptable"
        );
    }

    #[tokio::test]
    async fn take_skips_entries_older_than_ttl() {
        // Regression: an ACP `tool_call` whose matching MCP round-trip
        // never arrives (host changed its mind, transport dropped, etc.)
        // must not sit in the queue forever and mis-bind a subsequent
        // delegation. TTL eviction is exercised by advancing the
        // injected `as of` instant past PENDING_TOOL_CALL_TTL.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        let t0 = Instant::now();
        broker
            .register_pending_tool_call("p1", "stale".into())
            .await;
        // Fresh id registered "just before" the future `now`.
        broker
            .register_pending_tool_call("p1", "fresh".into())
            .await;
        let future_now = t0 + PENDING_TOOL_CALL_TTL + Duration::from_millis(50);
        // Forge "fresh" so it survives the TTL: rewrite its timestamp to
        // ~now-relative-to-future-now. Direct field access is OK — we're
        // a sibling test in the same module.
        {
            let mut map = broker.tool_calls.inner.lock().await;
            let bucket = map.get_mut("p1").expect("bucket present");
            // Re-stamp the second entry ("fresh") to `future_now`.
            if let Some(entry) = bucket
                .pending
                .iter_mut()
                .find(|p| p.tool_call_id == "fresh")
            {
                entry.registered_at = future_now;
            }
        }
        // First entry ("stale", stamped at ~t0) is past TTL relative to
        // future_now; the second ("fresh") was just re-stamped to
        // future_now and must survive.
        assert_eq!(
            broker
                .take_pending_tool_call_at("p1", future_now)
                .await
                .map(|(id, _)| id)
                .as_deref(),
            Some("fresh")
        );
        assert!(broker.take_pending_tool_call("p1").await.is_none());
    }

    #[tokio::test]
    async fn pending_tool_call_is_isolated_per_parent() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "p1-a".into()).await;
        broker.register_pending_tool_call("p2", "p2-a".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("p1-a")
        );
        assert_eq!(
            broker.take_pending_tool_call("p2").await.as_deref(),
            Some("p2-a")
        );
        assert!(broker.take_pending_tool_call("p1").await.is_none());
        assert!(broker.take_pending_tool_call("p2").await.is_none());
    }

    // -- (agent_type, task) correlation for parallel delegations ----------

    /// Build a match key with a fixed agent and no explicit working_dir for
    /// the common case where the test only varies the task. Use `key_for` to
    /// vary the agent, or `key_with_dir` to vary the directory.
    fn task_key(task: &str) -> DelegationMatchKey {
        key_for(AgentType::Codex, task)
    }

    fn key_for(agent_type: AgentType, task: &str) -> DelegationMatchKey {
        DelegationMatchKey {
            agent_type,
            task: task.to_string(),
            working_dir: None,
        }
    }

    fn key_with_dir(task: &str, working_dir: &str) -> DelegationMatchKey {
        DelegationMatchKey {
            agent_type: AgentType::Codex,
            task: task.to_string(),
            working_dir: Some(working_dir.to_string()),
        }
    }

    #[tokio::test]
    async fn parallel_delegations_bind_by_key_regardless_of_order() {
        // Two `delegate_to_agent` calls fire in parallel; both ACP tool_call
        // events register with their key. The MCP round-trips can claim in
        // EITHER order — each must bind to its own id by key match, never
        // swap. Pure FIFO would hand the first claimer "tc-A" regardless of
        // which call it represented.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-A".into(), Some(task_key("task A")))
            .await;
        broker
            .register_pending_tool_call_with_key("p1", "tc-B".into(), Some(task_key("task B")))
            .await;
        // Claim "task B" first (reverse of registration order).
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task B"))
                .await
                .as_deref(),
            Some("tc-B")
        );
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-A")
        );
        // A re-claim of an already-consumed key finds nothing.
        assert!(broker
            .take_matching_tool_call("p1", &task_key("task A"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn parallel_same_task_different_agent_do_not_swap() {
        // Regression for Codex review: two parallel calls with the SAME task
        // text but DIFFERENT agents must bind by the full key, not by task
        // alone — otherwise the codex card could show the claude_code child
        // and vice versa.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key(
                "p1",
                "tc-codex".into(),
                Some(key_for(AgentType::Codex, "review this")),
            )
            .await;
        broker
            .register_pending_tool_call_with_key(
                "p1",
                "tc-claude".into(),
                Some(key_for(AgentType::ClaudeCode, "review this")),
            )
            .await;
        // The claude_code round-trip must claim the claude_code id even though
        // the codex entry shares the identical task and registered first.
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &key_for(AgentType::ClaudeCode, "review this"))
                .await
                .as_deref(),
            Some("tc-claude")
        );
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &key_for(AgentType::Codex, "review this"))
                .await
                .as_deref(),
            Some("tc-codex")
        );
    }

    #[tokio::test]
    async fn parallel_same_task_same_agent_different_dir_do_not_swap() {
        // Regression for Codex review round 2: two parallel calls with the
        // SAME agent and SAME task text but DIFFERENT explicit working_dir
        // (e.g. "run tests" against /repo-a vs /repo-b) must bind by the full
        // key including working_dir. Claimed in reverse registration order to
        // prove it's not arrival-order FIFO.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key(
                "p1",
                "tc-a".into(),
                Some(key_with_dir("run tests", "/repo-a")),
            )
            .await;
        broker
            .register_pending_tool_call_with_key(
                "p1",
                "tc-b".into(),
                Some(key_with_dir("run tests", "/repo-b")),
            )
            .await;
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &key_with_dir("run tests", "/repo-b"))
                .await
                .as_deref(),
            Some("tc-b")
        );
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &key_with_dir("run tests", "/repo-a"))
                .await
                .as_deref(),
            Some("tc-a")
        );
    }

    #[tokio::test]
    async fn claim_does_not_steal_sibling_and_waits_for_own_registration() {
        // Regression for the reported bug: with only the SIBLING's keyed id
        // registered, a delegation must NOT grab it (which would swap the two
        // cards) — it waits for its own id. The brief-wait loop picks it up
        // once it registers shortly after.
        let broker = std::sync::Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        ));
        broker
            .register_pending_tool_call_with_key("p1", "tc-A".into(), Some(task_key("task A")))
            .await;
        // Immediate claim for "task B" while only tc-A (task A) is pending
        // must refuse to steal tc-A.
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task B"))
                .await
                .is_none(),
            "must not steal a sibling's keyed id"
        );
        // tc-A is still claimable by its own key.
        let broker_bg = broker.clone();
        let register_late = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            broker_bg
                .register_pending_tool_call_with_key("p1", "tc-B".into(), Some(task_key("task B")))
                .await;
        });
        // The brief-wait claim polls until tc-B (task B) registers.
        let claimed = broker
            .claim_pending_tool_call_with_brief_wait("p1", &task_key("task B"))
            .await;
        register_late.await.unwrap();
        assert_eq!(claimed.map(|(id, _)| id).as_deref(), Some("tc-B"));
        // tc-A remains for its own key.
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-A")
        );
    }

    #[tokio::test]
    async fn lone_unkeyed_entry_is_not_claimed_in_loop_only_post_budget() {
        // A host that ships no parseable `raw_input` registers match_key=None.
        // The in-loop path NEVER claims it — not even when it's the only entry,
        // and regardless of how old it gets (10s here). Entry age is no proof a
        // key isn't still coming: a serialized round-trip can register/backfill
        // arbitrarily late, and the entry could belong to a parallel sibling
        // whose owner hasn't registered yet (the staggered-singleton race —
        // Codex review). Arrival-order FIFO is reserved for the post-budget last
        // resort, which only runs once the CALLER has waited its full budget.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        // Even aged 10s (well past any heuristic grace, still < TTL so not
        // evicted), the in-loop claim refuses to hand out the unkeyed id.
        let way_aged = Instant::now() + Duration::from_secs(10);
        assert!(
            broker
                .take_matching_tool_call_at("p1", &task_key("whatever"), way_aged)
                .await
                .is_none(),
            "an unkeyed entry must never be claimed in-loop, regardless of age"
        );
        // The post-budget last resort is where a genuinely keyless entry binds.
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-A")
        );
    }

    #[tokio::test]
    async fn parallel_unkeyed_entries_are_not_claimed_in_loop() {
        // THE Finding 1 regression. Two delegations whose initial ToolCalls
        // registered UNKEYED (args arrive later on a ToolCallUpdate). Before
        // either is keyed, a round-trip arrives. The old `all unkeyed →
        // pop_front` handed it the OLDEST entry (tc-A), mis-binding it to the
        // wrong delegation. The in-loop claim now withholds (None) because no
        // key matches — arrival-order FIFO is left to the post-budget last
        // resort. Age never unlocks an in-loop claim.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        broker.register_pending_tool_call("p1", "tc-B".into()).await;
        // Aged (but < TTL, so not evicted): still withheld in-loop.
        let aged = Instant::now() + Duration::from_secs(5);
        assert!(
            broker
                .take_matching_tool_call_at("p1", &task_key("task B"), aged)
                .await
                .is_none(),
            "unkeyed siblings must not be FIFO-claimed in-loop"
        );
        // Neither entry was consumed.
        let map = broker.tool_calls.inner.lock().await;
        assert_eq!(map.get("p1").expect("bucket present").pending.len(), 2);
    }

    #[tokio::test]
    async fn parallel_unkeyed_resolves_by_backfilled_key_not_fifo() {
        // The pay-off: while the claim is withheld, the args arrive and
        // backfill a key onto the sibling. The round-trip then binds by EXACT
        // MATCH to its own id — never the FIFO-oldest. This is the would-be
        // mis-bind turned into a correct correlation.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        broker.register_pending_tool_call("p1", "tc-B".into()).await;
        // tc-B's args land → backfills its key.
        broker
            .register_pending_tool_call_with_key("p1", "tc-B".into(), Some(task_key("task B")))
            .await;
        // The "task B" round-trip binds to tc-B by key, not to the older tc-A.
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task B"))
                .await
                .as_deref(),
            Some("tc-B")
        );
        // tc-A is untouched, still pending for its own key/round-trip.
        let map = broker.tool_calls.inner.lock().await;
        let pending = &map.get("p1").expect("bucket present").pending;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_call_id, "tc-A");
    }

    #[tokio::test]
    async fn post_budget_fallback_still_fifos_parallel_unkeyed() {
        // A genuinely keyless host (no key ever lands) must still bind both
        // parallel delegations end-to-end. The in-loop claim withholds them,
        // but the post-budget last resort `take_pending_tool_call` claims them
        // oldest-first — the best a keyless host allows, and unchanged from
        // before. Only the premature in-loop FIFO is gone.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        broker.register_pending_tool_call("p1", "tc-B".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-A")
        );
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-B")
        );
    }

    #[tokio::test]
    async fn brief_wait_binds_own_late_registration_not_unkeyed_sibling() {
        // The staggered-singleton timeline Codex flagged, end-to-end: only an
        // UNKEYED sibling (tc-A) is visible when a DIFFERENT delegation's
        // round-trip (task B) starts claiming; B's own keyed `tool_call`
        // registers a little later, still inside the wait budget. The brief-wait
        // loop must bind B to its OWN id (tc-B) by exact match, never FIFO-steal
        // the older unkeyed tc-A. The old in-loop FIFO popped tc-A on the very
        // first poll (all-unkeyed); a grace gate would still steal it once tc-A
        // aged past the grace before tc-B arrived. Deferring all FIFO to the
        // post-budget — i.e. binding by exact match in-loop only — is what makes
        // this correct.
        let broker = std::sync::Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        ));
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        // B's own ACP registration lands ~200ms in — well after any age-based
        // heuristic would have fired, but far inside the ~2s claim budget.
        let broker_bg = broker.clone();
        let register_late = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            broker_bg
                .register_pending_tool_call_with_key("p1", "tc-B".into(), Some(task_key("task B")))
                .await;
        });
        let claimed = broker
            .claim_pending_tool_call_with_brief_wait("p1", &task_key("task B"))
            .await;
        register_late.await.unwrap();
        assert_eq!(
            claimed.map(|(id, _)| id).as_deref(),
            Some("tc-B"),
            "must wait for its own registration, not FIFO-steal the unkeyed sibling"
        );
        // tc-A is untouched, still pending for its own correlation.
        let map = broker.tool_calls.inner.lock().await;
        let pending = &map.get("p1").expect("bucket present").pending;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_call_id, "tc-A");
    }

    #[tokio::test]
    async fn reemit_backfills_key_onto_unkeyed_entry() {
        // A host that re-emits the `session/update(tool_call)` variant: the
        // first ToolCall has no parseable args (registers match_key=None), a
        // later re-emit carries the full args. The re-emit must backfill the
        // key onto the existing entry (not push a duplicate, not be dropped)
        // so key matching works.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        broker
            .register_pending_tool_call_with_key("p1", "tc-A".into(), Some(task_key("task A")))
            .await;
        // Now claimable by the backfilled key.
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-A")
        );
        assert!(broker.take_pending_tool_call("p1").await.is_none());
    }

    #[tokio::test]
    async fn fallback_never_steals_a_keyed_sibling() {
        // A keyed sibling is pending but the requesting round-trip's key never
        // matches (its own tool_call was genuinely lost). The post-budget last
        // resort must NOT hand out the keyed sibling — stealing it would just
        // move the dead card from this delegation to the sibling. It returns
        // None (→ caller mints a synthetic id), and the sibling stays claimable
        // by its own round-trip. (Regression: the old behavior FIFO-popped the
        // keyed entry here, swapping which delegation broke.)
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-A".into(), Some(task_key("task A")))
            .await;
        // No entry matches "task Z", and a keyed entry is present, so the
        // match step refuses to claim.
        assert!(broker
            .take_matching_tool_call("p1", &task_key("task Z"))
            .await
            .is_none());
        // The post-budget last resort steps over the keyed entry → None.
        assert!(
            broker.take_pending_tool_call("p1").await.is_none(),
            "must not steal a keyed sibling via the anonymous fallback"
        );
        // The keyed sibling is untouched — still claimable by its own key.
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-A")
        );
    }

    #[tokio::test]
    async fn keyed_entry_survives_past_ttl_for_serialized_round_trip() {
        // THE headline regression for the reported bug. A 2nd parallel
        // delegation's tool_call registers (keyed), then its MCP round-trip is
        // serialized far behind the 1st delegation — arriving well past
        // PENDING_TOOL_CALL_TTL. The keyed entry must NOT be aged out: an exact
        // key match claims it at any age, so the parent card binds instead of
        // falling to a synthetic id. (Observed live: round-trip landed 77s
        // after registration, past the 60s TTL → evicted → synthetic → dead
        // card stuck on "sub-agent running…".)
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key(
                "p1",
                "tc-late".into(),
                Some(task_key("slow task")),
            )
            .await;
        // Claim "as of" long past the TTL — simulates the round-trip arriving
        // after a many-times-TTL wait behind a serialized sibling.
        let way_past_ttl = Instant::now() + PENDING_TOOL_CALL_TTL * 10;
        assert_eq!(
            broker
                .take_matching_tool_call_at("p1", &task_key("slow task"), way_past_ttl)
                .await
                .as_deref(),
            Some("tc-late"),
            "a keyed entry must remain claimable by exact key match regardless of age"
        );
    }

    #[tokio::test]
    async fn unkeyed_entry_is_still_aged_out() {
        // The flip side: UNKEYED entries (host shipped no parseable raw_input)
        // remain anonymous and arrival-order-correlated, so a stale one MUST
        // still be GC'd by age — otherwise it could mis-bind a much later
        // unkeyed delegation via the FIFO path.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call("p1", "tc-stale".into())
            .await;
        let way_past_ttl = Instant::now() + PENDING_TOOL_CALL_TTL * 10;
        // Unkeyed + stale → evicted by the match path's GC → nothing to claim.
        assert!(broker
            .take_matching_tool_call_at("p1", &task_key("whatever"), way_past_ttl)
            .await
            .is_none());
        // And the anonymous path agrees it's gone.
        assert!(broker
            .take_pending_tool_call_at("p1", way_past_ttl)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn explicit_tool_use_id_consumes_pending_entry_acp_first() {
        // Codex review fix: client supplies the real id via `_meta.tool_use_id`
        // AFTER the dispatcher already registered it (ACP-before-MCP). The
        // explicit-id path must consume the keyed pending entry so it can't
        // linger (keyed entries are retained indefinitely) and be mis-claimed
        // by a later same-key delegation.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-x".into(), Some(task_key("task A")))
            .await;
        broker.consume_explicit_tool_call("p1", "tc-x").await;
        // No longer claimable by its key.
        assert!(broker
            .take_matching_tool_call("p1", &task_key("task A"))
            .await
            .is_none());
        // A late ACP re-registration of the same id is dropped (consumed).
        broker
            .register_pending_tool_call_with_key("p1", "tc-x".into(), Some(task_key("task A")))
            .await;
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .is_none(),
            "a re-registration after explicit consume must stay dropped"
        );
    }

    #[tokio::test]
    async fn explicit_tool_use_id_consumes_pending_entry_mcp_first() {
        // The MCP-before-ACP order: the explicit-id request is handled before
        // the ACP tool_call event registers. consume_explicit_tool_call records
        // the id as consumed up front, so the later registration is dropped.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.consume_explicit_tool_call("p1", "tc-y").await;
        broker
            .register_pending_tool_call_with_key("p1", "tc-y".into(), Some(task_key("task B")))
            .await;
        assert!(broker
            .take_matching_tool_call("p1", &task_key("task B"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn tombstone_removes_stale_keyed_entry() {
        // A `delegate_to_agent` tool call registered a keyed entry but its MCP
        // round-trip never reached the broker (the call failed / the turn was
        // interrupted). A terminal `ToolCallUpdate` tombstones the entry so it
        // can't linger indefinitely (keyed entries are never aged out).
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-stale".into(), Some(task_key("task A")))
            .await;
        assert!(broker.tombstone_pending_tool_call("p1", "tc-stale").await);
        assert!(broker
            .take_matching_tool_call("p1", &task_key("task A"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn tombstone_prevents_same_key_misbind() {
        // The High regression: without the tombstone, a stale keyed entry is
        // retained forever and a LATER identical-key delegation claims its dead
        // id (the exact-key scan returns the oldest match). After tombstoning the
        // stale entry, a fresh registration for the same key binds to the FRESH
        // id instead.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-stale".into(), Some(task_key("task A")))
            .await;
        broker.tombstone_pending_tool_call("p1", "tc-stale").await;
        broker
            .register_pending_tool_call_with_key("p1", "tc-fresh".into(), Some(task_key("task A")))
            .await;
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-fresh"),
            "a later same-key delegation must claim the fresh id, not the tombstoned one"
        );
    }

    #[tokio::test]
    async fn tombstone_leaves_other_entries_intact() {
        // A terminal update for an unrelated (non-delegation) id no-ops and must
        // leave a registered delegation untouched.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-keep".into(), Some(task_key("task A")))
            .await;
        assert!(
            !broker
                .tombstone_pending_tool_call("p1", "tc-bash-123")
                .await
        );
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-keep")
        );
    }

    #[tokio::test]
    async fn tombstone_then_reregister_same_id_stays_dropped() {
        // After tombstoning a real entry, an out-of-order re-registration of the
        // same id is dropped by the Tier-1 consumed check — mirrors
        // `explicit_tool_use_id_consumes_pending_entry_acp_first`.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-stale".into(), Some(task_key("task A")))
            .await;
        assert!(broker.tombstone_pending_tool_call("p1", "tc-stale").await);
        broker
            .register_pending_tool_call_with_key("p1", "tc-stale".into(), Some(task_key("task A")))
            .await;
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .is_none(),
            "a re-registration after tombstone must stay dropped"
        );
    }

    #[tokio::test]
    async fn tombstone_noop_does_not_record_consumed() {
        // The tombstone runs for EVERY terminal tool-call update, most of them
        // non-delegations. A no-op tombstone (id not pending) must NOT record
        // `consumed` — otherwise `consumed` (no TTL/cap) would grow with every
        // completed tool call, and a later legitimate registration of that id
        // would be wrongly dropped.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        assert!(!broker.tombstone_pending_tool_call("p1", "tc-x").await);
        broker
            .register_pending_tool_call_with_key("p1", "tc-x".into(), Some(task_key("task A")))
            .await;
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-x"),
            "a no-op tombstone must not record consumed and drop a later registration"
        );
    }

    #[tokio::test]
    async fn tombstone_removes_only_the_matching_entry_from_a_multi_entry_bucket() {
        // Tombstoning a MIDDLE entry removes only that id (retain is by exact
        // tool_call_id, position-independent) and leaves the siblings claimable
        // by their own keys.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-a".into(), Some(task_key("task A")))
            .await;
        broker
            .register_pending_tool_call_with_key("p1", "tc-b".into(), Some(task_key("task B")))
            .await;
        broker
            .register_pending_tool_call_with_key("p1", "tc-c".into(), Some(task_key("task C")))
            .await;
        assert!(broker.tombstone_pending_tool_call("p1", "tc-b").await);
        assert!(broker
            .take_matching_tool_call("p1", &task_key("task B"))
            .await
            .is_none());
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-a")
        );
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task C"))
                .await
                .as_deref(),
            Some("tc-c")
        );
    }

    #[tokio::test]
    async fn keyed_pending_entries_have_no_count_cap() {
        // Regression for the PENDING_QUEUE_CAP removal: a high-fan-out parent
        // can register hundreds of keyed pending tool_calls — each awaiting its
        // own serialized MCP round-trip — and EVERY one is retained. The old
        // hard cap evicted the oldest keyed entry past 32, orphaning its card to
        // a synthetic id. Keyed entries are now bounded only by claim, terminal
        // tombstoning, and per-parent teardown.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        const N: usize = 256;
        for i in 0..N {
            broker
                .register_pending_tool_call_with_key(
                    "p1",
                    format!("tc-{i}"),
                    Some(task_key(&format!("task {i}"))),
                )
                .await;
        }
        {
            let map = broker.tool_calls.inner.lock().await;
            let bucket = map.get("p1").expect("bucket present");
            assert_eq!(
                bucket.pending.len(),
                N,
                "all keyed pending entries must be retained — no count cap"
            );
        }
        // Each entry stays individually claimable by its exact key, in any
        // order — proving none were dropped or mis-bound by fan-out.
        for i in [0usize, N / 2, N - 1] {
            let claimed = broker
                .take_matching_tool_call("p1", &task_key(&format!("task {i}")))
                .await;
            assert_eq!(claimed.as_deref(), Some(format!("tc-{i}").as_str()));
        }
    }

    #[tokio::test]
    async fn keyed_pending_entry_drains_via_tombstone() {
        // The drain path the no-cap design relies on: when the parent-side ACP
        // tool_call goes terminal before its MCP round-trip ever claims it,
        // `tombstone_pending_tool_call` removes the keyed entry (so it can't
        // linger) AND records it consumed (so a late re-emit can't mis-bind a
        // later delegation sharing the same key).
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-x".into(), Some(task_key("task x")))
            .await;
        assert!(
            broker.tombstone_pending_tool_call("p1", "tc-x").await,
            "tombstone must report it removed the pending entry"
        );
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task x"))
                .await
                .is_none(),
            "a tombstoned entry must be drained from pending"
        );
        // Re-register of the same id after tombstoning is dropped by the
        // Tier-1 consumed check, so it can never be claimed.
        broker
            .register_pending_tool_call_with_key("p1", "tc-x".into(), Some(task_key("task x")))
            .await;
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task x"))
                .await
                .is_none(),
            "consumed memory must reject a re-emit of a tombstoned id"
        );
    }

    #[tokio::test]
    async fn reregistration_refines_key_with_late_working_dir() {
        // Codex re-review fix: the same tool_call_id first registers with a key
        // LACKING working_dir (an early parseable raw_input), then a later
        // ToolCallUpdate completes it with the explicit working_dir. The stored
        // key must be REPLACED with the fuller one — otherwise the MCP claim
        // keying on Some(dir) can't match the stale None and orphans to a
        // synthetic id (dead card for explicit-working-dir delegations).
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key(
                "p1",
                "tc-d".into(),
                Some(key_for(AgentType::Codex, "build")),
            )
            .await;
        // Later update adds the explicit working_dir → key is refined in place.
        broker
            .register_pending_tool_call_with_key(
                "p1",
                "tc-d".into(),
                Some(key_with_dir("build", "/repo")),
            )
            .await;
        // The stale `working_dir: None` key no longer matches (it was replaced)…
        assert!(broker
            .take_matching_tool_call("p1", &key_for(AgentType::Codex, "build"))
            .await
            .is_none());
        // …and the refined `Some("/repo")` key claims the real id.
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &key_with_dir("build", "/repo"))
                .await
                .as_deref(),
            Some("tc-d"),
            "the MCP claim with the explicit working_dir must match the refined key"
        );
    }

    #[tokio::test]
    async fn empty_parent_tool_use_id_claims_pending_then_completes() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(7)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        broker
            .register_pending_tool_call("parent-conn", "tu-from-acp".into())
            .await;
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The captured ACP id was consumed.
        assert!(broker.take_pending_tool_call("parent-conn").await.is_none());
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    #[tokio::test]
    async fn empty_parent_tool_use_id_claims_pending_arriving_late() {
        // Regression: when the parent's ACP `session/update(tool_call)`
        // lands at the lifecycle dispatcher AFTER `broker.handle_request`
        // already entered the claim phase, the brief poll loop must still
        // pick it up rather than falling back to the synthetic UUID.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-late".into())).await;
        mock.queue_send(Ok(13)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };

        // Give the driver time to enter the claim wait loop on an empty
        // queue, then register the ACP id (simulates the dispatcher's
        // ToolCall handling landing late).
        tokio::time::sleep(Duration::from_millis(30)).await;
        broker
            .register_pending_tool_call("parent-conn", "tu-late".into())
            .await;

        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The late-arriving ACP id was consumed by the broker — no leftover
        // entry.
        assert!(broker.take_pending_tool_call("parent-conn").await.is_none());
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "late ok".into(),
                    child_conversation_id: 13,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    #[tokio::test]
    async fn empty_parent_tool_use_id_with_no_pending_falls_back_to_uuid() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(11)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "fallback ok".into(),
                    child_conversation_id: 11,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    #[tokio::test]
    async fn cancel_by_parent_also_drops_pending_tool_calls() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call("parent-conn", "tu-1".into())
            .await;
        broker.cancel_by_parent("parent-conn").await;
        assert!(broker.take_pending_tool_call("parent-conn").await.is_none());
    }

    #[tokio::test]
    async fn turn_cancel_keeps_consumed_rejects_reemit() {
        // A turn/prompt cancel (parent connection STAYS ALIVE) must NOT drop the
        // `consumed` tool_call memory. Otherwise a host re-emit of an
        // already-claimed id (e.g. a terminal status-flip) re-registers as fresh
        // `pending` and the next same-key delegation mis-binds to it — the
        // dead-card/wrong-child class this correlation machinery exists to
        // prevent. `cancel_by_parent_turn` retains `consumed`, so the re-emit
        // stays rejected by the Tier-1 consumed check.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        // Register + claim a keyed id (the delegation that just ran).
        broker
            .register_pending_tool_call_with_key("p1", "tc-A".into(), Some(task_key("task A")))
            .await;
        assert_eq!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .as_deref(),
            Some("tc-A"),
        );
        // Turn cancel — parent still alive.
        broker.cancel_by_parent_turn("p1").await;
        // Host re-emits the now-consumed id with the same key.
        broker
            .register_pending_tool_call_with_key("p1", "tc-A".into(), Some(task_key("task A")))
            .await;
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task A"))
                .await
                .is_none(),
            "re-emit of a consumed id must stay rejected across a turn cancel"
        );
    }

    #[tokio::test]
    async fn turn_cancel_drops_unclaimed_pending() {
        // The unclaimed `pending` half is cleared by a turn cancel (tombstoned
        // into `consumed`): the cancelled turn's serial round-trip won't arrive,
        // so the stale keyed entry must not remain claimable by a later same-key
        // delegation. `take_matching` scans only `pending`, so it returns None.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-B".into(), Some(task_key("task B")))
            .await;
        broker.cancel_by_parent_turn("p1").await;
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task B"))
                .await
                .is_none(),
            "unclaimed pending must not stay claimable after a turn cancel"
        );
    }

    #[tokio::test]
    async fn turn_cancel_tombstones_pending_rejects_late_reemit() {
        // Stronger than the clear test: after a turn cancel clears an UNCLAIMED
        // keyed pending id, a late host re-emit of that SAME id must not
        // resurrect it as a claimable entry — otherwise the next same-key
        // delegation would mis-bind to the stale id. The cancel tombstones the
        // cleared id into `consumed`, so the re-emit is dropped by the Tier-1
        // consumed check and never re-enters `pending`.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call_with_key("p1", "tc-X".into(), Some(task_key("task X")))
            .await;
        broker.cancel_by_parent_turn("p1").await;
        // Late re-emit of the cancelled turn's unclaimed id (same key).
        broker
            .register_pending_tool_call_with_key("p1", "tc-X".into(), Some(task_key("task X")))
            .await;
        assert!(
            broker
                .take_matching_tool_call("p1", &task_key("task X"))
                .await
                .is_none(),
            "a re-emit of a tombstoned (cleared-on-cancel) pending id must not be claimable"
        );
    }

    #[tokio::test]
    async fn teardown_cancel_clears_consumed() {
        // The teardown variant (`cancel_by_parent`) DOES drop consumed — the
        // connection is going away, so a reused connection_id must start clean.
        // Contrast with `turn_cancel_keeps_consumed_rejects_reemit`.
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-A"),
        );
        broker.cancel_by_parent("p1").await;
        // consumed cleared → the same id re-registers and is claimable again.
        broker.register_pending_tool_call("p1", "tc-A".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-A"),
            "teardown cancel must clear consumed so id reuse is acceptable"
        );
    }

    #[tokio::test]
    async fn cancel_by_parent_turn_drains_synchronously_then_tears_down_child() {
        // The turn cancel must (a) drop the tracker + remove parked calls
        // SYNCHRONOUSLY — before the connection loop could accept the next
        // prompt — so a delayed cancel can't tombstone/cancel a NEXT turn's
        // entries (the invariant `drop_tool_calls_for_parent` relies on); and
        // (b) still fully tear the child down (backgrounded), resolving the
        // awaiting `handle_request` as canceled exactly once.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-1".into())).await;
        mock.queue_send(Ok(7)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        // Park a delegation for "parent-conn"...
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // ...plus a separate unclaimed keyed tracker entry on the same parent.
        broker
            .register_pending_tool_call_with_key(
                "parent-conn",
                "tc-Z".into(),
                Some(task_key("task Z")),
            )
            .await;

        broker.cancel_by_parent_turn("parent-conn").await;

        // (a) Synchronously — no sleep: the parked call is removed and the
        // tracker entry is dropped (tombstoned), so neither can leak into a
        // next-turn registration that the backgrounded teardown might clobber.
        assert_eq!(
            broker.pending_count().await,
            0,
            "parked call must be drained synchronously by the turn cancel"
        );
        assert!(
            broker
                .take_matching_tool_call("parent-conn", &task_key("task Z"))
                .await
                .is_none(),
            "tracker pending must be dropped synchronously by the turn cancel"
        );

        // (b) The backgrounded child teardown still resolves the driver as
        // canceled and tears the child down exactly once.
        match driver.await.unwrap() {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            other => panic!("expected canceled, got {other:?}"),
        }
        assert_eq!(mock.cancels.lock().await.as_slice(), &["child-1"]);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["child-1"]);
    }

    #[tokio::test]
    async fn depth_limit_allows_root() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(7)).await;
        let lookup = Arc::new(MockDepth(vec![(1, None)])) as Arc<dyn ConversationDepthLookup>;
        let broker = DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, lookup);
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 2,
                ..DelegationConfig::default()
            })
            .await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    // -- Meta writer lifecycle --------------------------------------------

    use crate::acp::delegation::meta_writer::mock::MockMetaWriter;
    use crate::acp::delegation::meta_writer::DelegationMetaWriter;

    async fn broker_with_meta(
        mock: Arc<MockSpawner>,
        writer: Arc<MockMetaWriter>,
    ) -> DelegationBroker {
        let broker = DelegationBroker::with_meta_writer(
            mock as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            writer as Arc<dyn DelegationMetaWriter>,
        );
        enable_delegation(&broker).await;
        broker
    }

    #[tokio::test]
    async fn meta_writes_carry_task_preview_and_task_id_on_every_write() {
        // Meta is replace-wholesale on the ToolCallState, so BOTH the running
        // and the terminal writes must carry the task label + broker task id —
        // they are the persisted card's only label source on hosts whose
        // raw_input never carries the arguments (Cursor).
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-t".into())).await;
        mock.queue_send(Ok(42)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-task")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        for call in &calls {
            let inner = call
                .meta
                .get("codeg.delegation")
                .unwrap()
                .as_object()
                .unwrap();
            assert_eq!(
                inner.get("task_preview").unwrap().as_str().unwrap(),
                "do x",
                "every write must keep the task label (status={})",
                inner.get("status").unwrap()
            );
            assert_eq!(
                inner.get("task_id").unwrap().as_str().unwrap(),
                &call_id,
                "task_id must be the broker call id on every write"
            );
        }
    }

    #[tokio::test]
    async fn identityless_fifo_claim_restores_delegate_identity() {
        // Cursor path: the ACP announcement registered an identity-less
        // candidate ("MCP: tool" + "{}"), the MCP round-trip arrives with no
        // explicit tool_use id. The post-budget FIFO claim must land on the
        // candidate AND restore the call's identity (canonical delegate title
        // + reconstructed arguments) on the live tool call.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-i".into())).await;
        mock.queue_send(Ok(43)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;
        broker
            .register_identityless_tool_call("parent-conn", "call-cursor-7\nfc_x_0".into())
            .await;

        let driver = {
            let broker = broker.clone();
            // Empty tool_use id → claim path (2s budget, then FIFO).
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 43,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let identities = writer.identity_snapshot().await;
        assert_eq!(identities.len(), 1, "exactly one identity restoration");
        let id_write = &identities[0];
        assert_eq!(id_write.parent_connection_id, "parent-conn");
        assert_eq!(id_write.tool_call_id, "call-cursor-7\nfc_x_0");
        assert_eq!(
            id_write.title,
            crate::acp::delegation::DELEGATE_TOOL_REWRITE_TITLE
        );
        assert_eq!(
            id_write.raw_input.get("task").unwrap().as_str().unwrap(),
            "do x"
        );
        assert_eq!(
            id_write
                .raw_input
                .get("agent_type")
                .unwrap()
                .as_str()
                .unwrap(),
            "claude_code"
        );
        // The meta writes bound to the REAL claimed id, not a synthetic one.
        let metas = writer.snapshot().await;
        assert!(metas
            .iter()
            .all(|m| m.parent_tool_use_id == "call-cursor-7\nfc_x_0"));
    }

    #[tokio::test]
    async fn keyed_claim_does_not_rewrite_host_identity() {
        // A host that shipped real raw_input (keyed registration) keeps its
        // own wire identity — reconstructing raw_input would clobber
        // host-specific fields.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-k".into())).await;
        mock.queue_send(Ok(44)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;
        broker
            .register_pending_tool_call_with_key(
                "parent-conn",
                "tc-keyed".into(),
                Some(DelegationMatchKey {
                    agent_type: AgentType::ClaudeCode,
                    task: "do x".into(),
                    working_dir: None,
                }),
            )
            .await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 44,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        assert!(
            writer.identity_snapshot().await.is_empty(),
            "keyed (host-identified) claims must not be rewritten"
        );
        let metas = writer.snapshot().await;
        assert!(metas.iter().all(|m| m.parent_tool_use_id == "tc-keyed"));
    }

    #[tokio::test]
    async fn rewrite_identityless_tool_call_claims_and_writes_status_identity() {
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(Arc::new(MockSpawner::new()), writer.clone()).await;
        broker
            .register_identityless_tool_call("parent-conn", "call-status-1".into())
            .await;

        let claimed = broker
            .rewrite_identityless_tool_call(
                "parent-conn",
                crate::acp::delegation::STATUS_TOOL_REWRITE_TITLE,
                serde_json::json!({"task_ids": ["t-1"], "wait_ms": 0}),
            )
            .await;
        assert_eq!(claimed.as_deref(), Some("call-status-1"));
        let identities = writer.identity_snapshot().await;
        assert_eq!(identities.len(), 1);
        assert_eq!(
            identities[0].title,
            crate::acp::delegation::STATUS_TOOL_REWRITE_TITLE
        );
        assert_eq!(
            identities[0].raw_input.get("task_ids").unwrap()[0]
                .as_str()
                .unwrap(),
            "t-1"
        );
        // Consumed: the candidate can't be claimed again by the delegate FIFO.
        assert!(broker.take_pending_tool_call("parent-conn").await.is_none());
    }

    #[tokio::test]
    async fn rewrite_skips_connections_that_never_announced_identityless() {
        // Plain unkeyed registration (a keyless delegation invocation) must
        // NOT be renamed — and the sticky gate must return instantly (no
        // 300 ms poll) for such connections. Also: the entry stays claimable
        // by the delegate FIFO afterwards.
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(Arc::new(MockSpawner::new()), writer.clone()).await;
        broker
            .register_pending_tool_call("parent-conn", "tc-plain".into())
            .await;

        let started = std::time::Instant::now();
        let claimed = broker
            .rewrite_identityless_tool_call(
                "parent-conn",
                crate::acp::delegation::STATUS_TOOL_REWRITE_TITLE,
                serde_json::json!({"task_ids": ["t-1"]}),
            )
            .await;
        assert!(claimed.is_none());
        // The poll path sleeps ≥300 ms (30 × 10 ms lower bounds); the gate
        // path sleeps zero. 280 ms discriminates the two while leaving slack
        // for full-suite parallel-load scheduling jitter.
        assert!(
            started.elapsed() < Duration::from_millis(280),
            "sticky gate must skip the poll for hosts without identity-less announcements"
        );
        assert!(writer.identity_snapshot().await.is_empty());
        assert_eq!(
            broker
                .take_pending_tool_call("parent-conn")
                .await
                .as_deref(),
            Some("tc-plain"),
            "the plain unkeyed entry must remain for the delegate FIFO"
        );
    }

    #[tokio::test]
    async fn meta_writer_records_running_then_completed_on_happy_path() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-real")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        // First write: running, with child connection + conversation ids.
        let first = &calls[0];
        assert_eq!(first.parent_tool_use_id, "pt-real");
        let inner_first = first
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            inner_first.get("status").unwrap().as_str().unwrap(),
            "running"
        );
        assert_eq!(
            inner_first
                .get("child_connection_id")
                .unwrap()
                .as_str()
                .unwrap(),
            "child-conn-1"
        );
        assert_eq!(
            inner_first
                .get("child_conversation_id")
                .unwrap()
                .as_i64()
                .unwrap(),
            42
        );
        // Second write: completed.
        let second = &calls[1];
        let inner_second = second
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            inner_second.get("status").unwrap().as_str().unwrap(),
            "completed"
        );
    }

    #[tokio::test]
    async fn meta_writer_records_failed_on_err_outcome() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-2".into())).await;
        mock.queue_send(Ok(7)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-err")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::from_err(
                    DelegationError::SubagentRuntimeError("agent died".into()),
                    Some(7),
                ),
            )
            .await;
        driver.await.unwrap();

        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        let inner = calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(inner.get("status").unwrap().as_str().unwrap(), "failed");
        assert_eq!(
            inner.get("error_code").unwrap().as_str().unwrap(),
            "subagent_error"
        );
    }

    // -- Registration-race: child terminal failure before the entry is parked --

    /// Headline regression: a child terminal failure (auth error / immediate
    /// process death) that fires AFTER the broker reserved the child but BEFORE
    /// it parked the pending entry must still resolve the parked request — not
    /// no-op and strand it on `rx.await` forever. The `send_gate` pins
    /// `handle_request` in exactly that window; we fire the failure, release the
    /// gate, and assert the request resolves as canceled (carrying the
    /// terminal-error detail) with a single child disconnect and a clean
    /// running→failed meta trail.
    #[tokio::test]
    async fn child_failure_before_park_resolves_instead_of_hanging() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-fast-fail".into())).await;
        mock.queue_send(Ok(55)).await;
        let release = mock.install_send_gate().await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-fast")).await })
        };

        // Wait until handle_request has spawned + reserved the child and is
        // held inside send_prompt by the gate — entry NOT yet parked.
        loop {
            if broker.reserved_child_count().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(broker.pending_count().await, 0, "entry not parked yet");

        // Child dies before the entry is parked. With the reservation in place
        // this buffers (rather than no-oping on a not-yet-existent entry).
        broker
            .cancel_by_child_connection("c-fast-fail", Some("Authentication required"))
            .await;
        assert_eq!(broker.early_cancel_count().await, 1, "failure buffered");

        // Release send_prompt → handle_request parks, drains the buffered
        // failure, and resolves inline instead of hanging.
        let _ = release.send(());
        let outcome = driver.await.unwrap();
        match outcome {
            DelegationOutcome::Err {
                code,
                message,
                child_conversation_id,
            } => {
                assert_eq!(code, "canceled");
                assert!(
                    message.contains("Authentication required"),
                    "reason should carry the terminal-error detail, got: {message}"
                );
                assert_eq!(child_conversation_id, Some(55));
            }
            other => panic!("expected canceled Err, got {other:?}"),
        }

        // Reservation + buffer drained; child torn down exactly once.
        assert_eq!(broker.pending_count().await, 0);
        assert_eq!(broker.reserved_child_count().await, 0);
        assert_eq!(broker.early_cancel_count().await, 0);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c-fast-fail"]);

        // Meta trail: running (written pre-park) then failed/canceled (pickup).
        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        let running = calls[0]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(running.get("status").unwrap().as_str().unwrap(), "running");
        let failed = calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(failed.get("status").unwrap().as_str().unwrap(), "failed");
        assert_eq!(
            failed.get("error_code").unwrap().as_str().unwrap(),
            "canceled"
        );
    }

    /// The SAME race on the SUCCESS path: a `TurnComplete` whose `complete_call`
    /// fires AFTER the delegation reserved but BEFORE `handle_request` parked (a
    /// fast/empty turn whose completion propagates while the broker is still
    /// awaiting the parent `write_meta`) must still resolve the request. The
    /// prompt is only *enqueued* by `send_prompt`, so the child loop can emit
    /// `TurnComplete` before the park. The `send_gate` pins `handle_request` in
    /// the reserve→park window; we resolve via the reserved `call_id` (the entry
    /// isn't parked yet) and assert the request returns Ok instead of hanging.
    #[tokio::test]
    async fn completion_before_park_resolves_instead_of_hanging() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-fast-ok".into())).await;
        mock.queue_send(Ok(70)).await;
        let release = mock.install_send_gate().await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-ok")).await })
        };

        // Wait until reserved (spawned + id minted, held in send_prompt by the
        // gate); the entry is NOT parked yet, so grab the call_id from the
        // reservation rather than the parked-calls map.
        let call_id = loop {
            if let Some(id) = broker.peek_reserved_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(broker.pending_count().await, 0, "entry not parked yet");

        // TurnComplete beats the park. With the reservation in place this
        // buffers (rather than no-oping on a not-yet-existent entry).
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "fast done".into(),
                    child_conversation_id: 70,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        assert_eq!(
            broker.early_complete_count().await,
            1,
            "completion buffered"
        );

        // Release send_prompt → handle_request parks, drains the buffered
        // completion, and resolves inline instead of hanging.
        let _ = release.send(());
        let outcome = driver.await.unwrap();
        match outcome {
            DelegationOutcome::Ok(s) => {
                assert_eq!(s.text, "fast done");
                assert_eq!(s.child_conversation_id, 70);
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        assert_eq!(broker.pending_count().await, 0);
        assert_eq!(broker.reserved_call_count().await, 0);
        assert_eq!(broker.reserved_child_count().await, 0);
        assert_eq!(broker.early_complete_count().await, 0);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c-fast-ok"]);

        // Meta trail: running (written pre-park) then completed (pickup).
        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        let running = calls[0]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(running.get("status").unwrap().as_str().unwrap(), "running");
        let completed = calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            completed.get("status").unwrap().as_str().unwrap(),
            "completed"
        );
    }

    /// The reservation is released at park, and a SUCCESSFUL completion buffers
    /// nothing. The child's post-completion disconnect (normal v1 one-shot
    /// teardown) finds the child un-reserved and must NOT buffer a spurious
    /// cancel — otherwise every completed delegation would leak a buffer entry.
    #[tokio::test]
    async fn normal_completion_leaves_no_reservation_or_buffer() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-clean".into())).await;
        mock.queue_send(Ok(60)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-clean")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        // Parked → reservation already released.
        assert_eq!(
            broker.reserved_child_count().await,
            0,
            "park releases the reservation"
        );

        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 60,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        assert!(matches!(driver.await.unwrap(), DelegationOutcome::Ok(_)));

        // The child's post-completion disconnect arrives. Child is no longer
        // reserved → must NOT buffer a spurious cancel.
        broker.cancel_by_child_connection("c-clean", None).await;
        assert_eq!(
            broker.early_cancel_count().await,
            0,
            "a post-resolution teardown must not buffer a spurious cancel"
        );
        assert_eq!(broker.pending_count().await, 0);
    }

    // -- Item 1: parent-cancel coverage of the `handle_request` setup window --

    /// A parent cancel that lands while `handle_request` is INSIDE `spawn` (the
    /// child exists but no prompt has been sent) must disconnect the child and
    /// bail — never send it a prompt — instead of no-oping and letting it run
    /// orphaned. Pinned with the spawn gate.
    #[tokio::test]
    async fn parent_cancel_in_spawn_window_disconnects_child_without_sending() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c2".into())).await;
        mock.queue_send(Ok(99)).await; // staged but must NOT be consumed
        let release = mock.install_spawn_gate().await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-2")).await })
        };
        // Inside spawn (call recorded, held by the gate): registered in-flight,
        // not yet reserved.
        loop {
            if !mock.spawn_args.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(broker.inflight_count().await, 1);
        assert_eq!(broker.reserved_child_count().await, 0, "not reserved yet");

        broker.cancel_by_parent_turn("parent-conn").await;
        let _ = release.send(());

        match driver.await.unwrap() {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            other => panic!("expected canceled, got {other:?}"),
        }
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c2"]);
        assert!(
            mock.cancels.lock().await.is_empty(),
            "no prompt was sent, so no cancel — disconnect only"
        );
        assert_eq!(
            mock.send_results.lock().await.len(),
            1,
            "send must not be consumed — no prompt sent to an abandoned child"
        );
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(broker.reserved_child_count().await, 0);
    }

    /// A parent cancel that lands in the reserve→park window (prompt already
    /// sent, entry not yet parked) must cancel AND disconnect the child and
    /// resolve the request as canceled. Pinned with the send gate; also asserts
    /// the running→failed/canceled meta trail.
    #[tokio::test]
    async fn parent_cancel_in_reserve_park_window_tears_down_child() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c3".into())).await;
        mock.queue_send(Ok(33)).await;
        let release = mock.install_send_gate().await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-3")).await })
        };
        // Spawned + reserved, held inside send_prompt.
        loop {
            if broker.reserved_child_count().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(broker.inflight_count().await, 1);
        assert_eq!(broker.pending_count().await, 0, "not parked yet");

        broker.cancel_by_parent_turn("parent-conn").await;
        let _ = release.send(());

        match driver.await.unwrap() {
            DelegationOutcome::Err {
                code,
                child_conversation_id,
                ..
            } => {
                assert_eq!(code, "canceled");
                assert_eq!(child_conversation_id, Some(33));
            }
            other => panic!("expected canceled, got {other:?}"),
        }
        // Prompt was sent → child cancel()'d AND disconnected.
        assert_eq!(mock.cancels.lock().await.as_slice(), &["c3"]);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c3"]);
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(broker.reserved_child_count().await, 0);
        assert_eq!(broker.early_cancel_count().await, 0);
        assert_eq!(broker.pending_count().await, 0);

        // Meta trail: running (pre-park) then failed/canceled (ParentCanceled).
        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        let running = calls[0]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(running.get("status").unwrap().as_str().unwrap(), "running");
        let failed = calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(failed.get("status").unwrap().as_str().unwrap(), "failed");
        assert_eq!(
            failed.get("error_code").unwrap().as_str().unwrap(),
            "canceled"
        );
    }

    /// Strict first-terminal-wins: when a child completion buffers FIRST and a
    /// parent cancel lands afterward, the child's earlier arrival stamp wins and
    /// its real result is preserved (the cancel is moot — the child already
    /// finished before it).
    #[tokio::test]
    async fn child_terminal_wins_over_later_parent_cancel() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c4".into())).await;
        mock.queue_send(Ok(44)).await;
        let release = mock.install_send_gate().await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-4")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_reserved_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(broker.inflight_count().await, 1);

        // Child completes FIRST, then the parent cancels — child result wins.
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 44,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        broker.cancel_by_parent_turn("parent-conn").await;
        let _ = release.send(());

        assert!(matches!(driver.await.unwrap(), DelegationOutcome::Ok(_)));
        assert!(
            mock.cancels.lock().await.is_empty(),
            "child completed — the moot parent cancel must not cancel it"
        );
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(broker.early_complete_count().await, 0);
    }

    /// Strict first-terminal-wins (Item 3): when the parent cancel is recorded
    /// BEFORE the child completion buffers, the cancel wins — the late
    /// completion is discarded and the child is torn down, because the parent
    /// had already abandoned the turn by the time the completion landed.
    #[tokio::test]
    async fn parent_cancel_wins_when_it_arrives_before_child_terminal() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c5".into())).await;
        mock.queue_send(Ok(55)).await;
        let release = mock.install_send_gate().await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-5")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_reserved_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };

        // Parent cancels FIRST (earlier arrival stamp); the child completes
        // afterward (later stamp) — first-terminal-wins judges the cancel the
        // winner and discards the late completion.
        broker.cancel_by_parent_turn("parent-conn").await;
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "late".into(),
                    child_conversation_id: 55,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let _ = release.send(());

        match driver.await.unwrap() {
            DelegationOutcome::Err {
                code,
                child_conversation_id,
                ..
            } => {
                assert_eq!(code, "canceled");
                assert_eq!(child_conversation_id, Some(55));
            }
            other => panic!(
                "first-terminal-wins: an earlier parent cancel must beat a later completion, got {other:?}"
            ),
        }
        // The abandoned child is torn down (prompt was sent → cancel + disconnect).
        assert_eq!(mock.cancels.lock().await.as_slice(), &["c5"]);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c5"]);
        assert_eq!(broker.inflight_count().await, 0);
        // The buffered completion was drained (and discarded), leaving no leak.
        assert_eq!(broker.early_complete_count().await, 0);
    }

    /// Strict first-terminal-wins through the child-FAILURE buffer: a child
    /// failure that buffers BEFORE a parent cancel keeps its (earlier) arrival
    /// stamp and wins, so the request resolves with the child's failure detail
    /// and the child is torn down once (disconnect only — the child already
    /// failed, so there's no in-flight prompt to cancel). Exercises the
    /// `early_cancels` stamp path that mirrors the completion case above.
    #[tokio::test]
    async fn child_failure_wins_over_later_parent_cancel() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("cF".into())).await;
        mock.queue_send(Ok(66)).await;
        let release = mock.install_send_gate().await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-f")).await })
        };
        // Spawned + reserved, held inside send_prompt by the gate.
        loop {
            if broker.reserved_child_count().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Child fails FIRST (earlier stamp), then the parent cancels (later
        // stamp) — the child terminal wins and carries its failure detail.
        broker
            .cancel_by_child_connection("cF", Some("boom detail"))
            .await;
        broker.cancel_by_parent_turn("parent-conn").await;
        let _ = release.send(());

        match driver.await.unwrap() {
            DelegationOutcome::Err {
                code,
                message,
                child_conversation_id,
            } => {
                assert_eq!(code, "canceled");
                assert!(
                    message.contains("boom detail"),
                    "child failure detail must survive, got: {message}"
                );
                assert_eq!(child_conversation_id, Some(66));
            }
            other => panic!("expected child failure Err, got {other:?}"),
        }
        // Child-terminal path tears down via disconnect only (no cancel).
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["cF"]);
        assert!(
            mock.cancels.lock().await.is_empty(),
            "child already failed — the moot parent cancel must not cancel it"
        );
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(broker.early_cancel_count().await, 0);
    }

    /// The teardown variant `cancel_by_parent` covers the same reserve→park
    /// window as the turn variant — both funnel through `drain_for_parent_cancel`
    /// where the in-flight mark is applied.
    #[tokio::test]
    async fn parent_teardown_in_reserve_park_window_tears_down_child() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c7".into())).await;
        mock.queue_send(Ok(77)).await;
        let release = mock.install_send_gate().await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-7")).await })
        };
        loop {
            if broker.reserved_child_count().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        let _ = release.send(());

        match driver.await.unwrap() {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            other => panic!("expected canceled, got {other:?}"),
        }
        assert_eq!(mock.cancels.lock().await.as_slice(), &["c7"]);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c7"]);
        assert_eq!(broker.inflight_count().await, 0);
    }

    /// A cancel targeting a DIFFERENT parent must not flag this setup: it parks
    /// normally and resolves via its own child terminal.
    #[tokio::test]
    async fn parent_cancel_for_other_parent_leaves_setup_intact() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c8".into())).await;
        mock.queue_send(Ok(88)).await;
        let release = mock.install_send_gate().await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-8")).await })
        };
        loop {
            if broker.reserved_child_count().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Wrong-parent cancel — a no-op for this setup.
        broker.cancel_by_parent_turn("some-other-parent").await;
        let _ = release.send(());

        // It must park normally; resolve it via its child completion.
        let parked = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &parked,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "fine".into(),
                    child_conversation_id: 88,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        assert!(matches!(driver.await.unwrap(), DelegationOutcome::Ok(_)));
        assert!(
            mock.cancels.lock().await.is_empty(),
            "a wrong-parent cancel must not tear this child down"
        );
        assert_eq!(broker.inflight_count().await, 0);
    }

    /// The in-flight record is deregistered on every exit path: the normal park
    /// hand-off, and each early-return (disabled / spawn-fail / send-fail).
    #[tokio::test]
    async fn inflight_drained_on_normal_park() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-ok".into())).await;
        mock.queue_send(Ok(70)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-ok")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        // Parked → the in-flight record was handed off (deregistered) at park.
        assert_eq!(
            broker.inflight_count().await,
            0,
            "park deregisters in-flight"
        );
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 70,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        assert!(matches!(driver.await.unwrap(), DelegationOutcome::Ok(_)));
        assert_eq!(broker.inflight_count().await, 0);
    }

    #[tokio::test]
    async fn inflight_drained_on_disabled() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        // `enabled` defaults to false → short-circuits at the disabled check.
        let outcome = broker.handle_request(request(1, "pt-d")).await;
        assert!(matches!(outcome, DelegationOutcome::Err { .. }));
        assert_eq!(broker.inflight_count().await, 0);
    }

    #[tokio::test]
    async fn inflight_drained_on_spawn_failure() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Err(SpawnerError::Spawn("nope".into())))
            .await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        match broker.handle_request(request(1, "pt-sf")).await {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "spawn_failed"),
            other => panic!("expected spawn_failed, got {other:?}"),
        }
        assert_eq!(broker.inflight_count().await, 0);
        assert!(mock.disconnects.lock().await.is_empty());
    }

    #[tokio::test]
    async fn inflight_drained_on_send_failure() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c6".into())).await;
        mock.queue_send(Err(SpawnerError::Send("boom".into())))
            .await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;
        match broker.handle_request(request(1, "pt-sendf")).await {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "spawn_failed"),
            other => panic!("expected spawn_failed, got {other:?}"),
        }
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c6"]);
        assert!(mock.cancels.lock().await.is_empty());
    }

    /// A terminal failure for a child the broker never reserved (unknown id, or
    /// one whose delegation already fully resolved) is a clean no-op — it must
    /// not buffer, so the buffer can only ever hold genuine pre-registration
    /// races.
    #[tokio::test]
    async fn cancel_for_unreserved_child_never_buffers() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .cancel_by_child_connection("never-reserved", Some("boom"))
            .await;
        assert_eq!(broker.early_cancel_count().await, 0);
        assert_eq!(broker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn meta_writer_records_failed_on_parent_cancel() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-cancel".into())).await;
        mock.queue_send(Ok(33)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-pcancel")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Err { .. }));

        let calls = writer.snapshot().await;
        // running + canceled
        assert_eq!(calls.len(), 2);
        let inner = calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(inner.get("status").unwrap().as_str().unwrap(), "failed");
        assert_eq!(
            inner.get("error_code").unwrap().as_str().unwrap(),
            "canceled"
        );
    }

    #[tokio::test]
    async fn meta_writer_skipped_for_synthetic_parent_tool_use_id() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-synth".into())).await;
        mock.queue_send(Ok(8)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone()).await;

        // Empty `parent_tool_use_id` triggers the broker's UUID fallback —
        // `"delegation-<uuid>"` — which the writer must skip because no
        // matching ACP tool_call_id exists.
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 8,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = writer.snapshot().await;
        assert!(
            calls.is_empty(),
            "writer should be skipped for synthetic parent_tool_use_id, got {:?}",
            calls
        );
    }

    // -- Event emitter lifecycle ------------------------------------------
    //
    // Issue: `.docs/issues/2026-05-24-delegation-termination-cascade.md`.
    // The broker must emit `AcpEvent::DelegationCompleted` once per drained
    // pending entry, regardless of which terminal path drained it (happy
    // `complete_call`, MCP `cancel_by_external_handle`, child-disconnect
    // cleanup, or parent-cancel cascade). Without these emits the frontend's live
    // delegation binding stays at "running" forever — see the issue doc
    // for the full path matrix.

    use crate::acp::delegation::event_emitter::mock::MockEventEmitter;
    use crate::acp::delegation::event_emitter::DelegationEventEmitter;
    use crate::acp::types::DelegationResultSummary;

    async fn broker_with_emitter(
        mock: Arc<MockSpawner>,
        writer: Arc<MockMetaWriter>,
        emitter: Arc<MockEventEmitter>,
    ) -> DelegationBroker {
        let broker = DelegationBroker::with_writers(
            mock as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            writer as Arc<dyn DelegationMetaWriter>,
            emitter as Arc<dyn DelegationEventEmitter>,
        );
        enable_delegation(&broker).await;
        broker
    }

    #[tokio::test]
    async fn emitter_records_ok_on_complete_call_happy_path() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-ok")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 73,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.parent_tool_use_id, "pt-ok");
        assert_eq!(call.child_connection_id, "child-conn-1");
        assert_eq!(call.child_conversation_id, 42);
        // The completed event carries the child agent_type (from the running
        // task), so a frontend that missed `DelegationStarted` still binds the
        // correct agent. `request()` delegates to ClaudeCode.
        assert_eq!(call.agent_type, AgentType::ClaudeCode);
        // duration_ms is now broker-measured (not the outcome's value); assert
        // the Ok variant + the enriched text_preview instead.
        assert!(
            matches!(
                &call.result,
                DelegationResultSummary::Ok { text_preview, .. }
                    if text_preview.as_deref() == Some("done")
            ),
            "expected Ok with preview, got {:?}",
            call.result
        );
    }

    #[tokio::test]
    async fn emitter_records_started_on_start_delegation_happy_path() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-start".into())).await;
        mock.queue_send(Ok(55)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-start")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };

        // `started` fires during setup, BEFORE the task parks — so it is already
        // recorded by the time a pending entry is visible, and it must not be
        // conflated with the (not-yet-emitted) terminal.
        let started = emitter.started_snapshot().await;
        assert_eq!(started.len(), 1, "exactly one DelegationStarted per task");
        let s = &started[0];
        assert_eq!(s.parent_connection_id, "parent-conn");
        assert_eq!(s.parent_tool_use_id, "pt-start");
        assert_eq!(s.child_connection_id, "child-conn-start");
        assert_eq!(s.child_conversation_id, 55);
        assert_eq!(s.agent_type, AgentType::ClaudeCode);
        assert_eq!(
            emitter.count().await,
            0,
            "no terminal emit before completion"
        );

        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 55,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 10,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        // Completion adds exactly one terminal emit; started stays at 1.
        assert_eq!(emitter.started_count().await, 1);
        assert_eq!(emitter.count().await, 1);
    }

    #[tokio::test]
    async fn emitter_skips_started_for_synthetic_parent_tool_use_id() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-synth-start".into())).await;
        mock.queue_send(Ok(9)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        // Empty parent_tool_use_id → broker falls back to a synthetic
        // `delegation-<uuid>` id (no ACP tool_call to claim in a mock harness).
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 9,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        assert_eq!(
            emitter.started_count().await,
            0,
            "started emit must skip synthetic parent_tool_use_id (same rule as the meta writer / completed emit)"
        );
    }

    #[tokio::test]
    async fn emitter_records_err_on_complete_call_err_outcome() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-err".into())).await;
        mock.queue_send(Ok(11)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-err")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::from_err(
                    DelegationError::SubagentRuntimeError("agent died".into()),
                    Some(11),
                ),
            )
            .await;
        driver.await.unwrap();

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1);
        match &calls[0].result {
            DelegationResultSummary::Err { error_code } => {
                assert_eq!(error_code, "subagent_error")
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emitter_records_canceled_on_cancel_by_external_handle() {
        // MCP-driven cancel path: companion received notifications/cancelled
        // and the listener forwarded it to broker.cancel_by_external_handle.
        // The broker must drain the pending entry, cancel + disconnect the
        // child, and emit DelegationCompleted with error_code = "canceled".
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-h".into())).await;
        mock.queue_send(Ok(91)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .handle_request(request_with_handle(1, "pt-mcp-cancel", "h-1"))
                    .await
            })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker
            .cancel_by_external_handle("h-1", "user requested".into())
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(
            outcome,
            DelegationOutcome::Err { ref code, .. } if code == "canceled"
        ));

        assert_eq!(mock.cancels.lock().await.as_slice(), &["child-conn-h"]);
        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1, "expected exactly one emit, got {calls:?}");
        let call = &calls[0];
        assert_eq!(call.parent_tool_use_id, "pt-mcp-cancel");
        assert_eq!(call.child_connection_id, "child-conn-h");
        assert_eq!(call.child_conversation_id, 91);
        match &call.result {
            DelegationResultSummary::Err { error_code } => {
                assert_eq!(error_code, "canceled")
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_by_external_handle_no_match_buffers_pre_cancel() {
        // Cancel arrives before handle_request reaches pending registration.
        // The broker must buffer the handle in pre_canceled_handles so the
        // in-flight call drains itself on its post-registration checkpoint.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-pre".into())).await;
        mock.queue_send(Ok(13)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        // Pre-cancel before spawning the driver — handle is unknown to the
        // broker right now, but a buffered entry should make the next
        // handle_request with the same handle bail out canceled.
        broker
            .cancel_by_external_handle("h-pre", "early cancel".into())
            .await;
        // Pre-cancel set is single-shot: a second call with the same handle
        // and no pending entry just buffers it again (idempotent in practice).
        let outcome = broker
            .handle_request(request_with_handle(1, "pt-pre", "h-pre"))
            .await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            other => panic!("expected canceled, got {other:?}"),
        }
        // Since the cancel won pre-spawn, no child connection should have
        // been opened.
        assert!(mock.cancels.lock().await.is_empty());
        assert!(mock.disconnects.lock().await.is_empty());
        // The pre-cancel early-return must also drop the in-flight record
        // (registered as handle_request's first statement, before this check).
        assert_eq!(broker.inflight_count().await, 0);
    }

    /// The real MCP-shaped path carries an `external_handle`. Registration now
    /// happens as `handle_request`'s FIRST statement — before the pre-cancel
    /// `.await` — so a parent cancel in the setup window reaches these requests
    /// too, not just the synthetic-id path. Guards the regression Codex flagged
    /// (registration ordered after the pre-cancel await left a miss window).
    #[tokio::test]
    async fn parent_cancel_covers_external_handle_setup_window() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-eh".into())).await;
        mock.queue_send(Ok(21)).await;
        let release = mock.install_send_gate().await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .handle_request(request_with_handle(1, "pt-eh", "h-eh"))
                    .await
            })
        };
        loop {
            if broker.reserved_child_count().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(broker.inflight_count().await, 1);

        broker.cancel_by_parent_turn("parent-conn").await;
        let _ = release.send(());

        match driver.await.unwrap() {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            other => panic!("expected canceled, got {other:?}"),
        }
        assert_eq!(mock.cancels.lock().await.as_slice(), &["c-eh"]);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c-eh"]);
        assert_eq!(broker.inflight_count().await, 0);
    }

    #[tokio::test]
    async fn emitter_records_canceled_on_cancel_by_child_connection() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-dropped".into())).await;
        mock.queue_send(Ok(55)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-cbc")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_child_connection("c-dropped", None).await;
        let outcome = driver.await.unwrap();
        match &outcome {
            DelegationOutcome::Err { code, message, .. } => {
                assert_eq!(code, "canceled");
                // No terminal_error supplied → falls back to default reason.
                assert_eq!(
                    message,
                    "canceled: child session ended without TurnComplete"
                );
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1);
        match &calls[0].result {
            DelegationResultSummary::Err { error_code } => {
                assert_eq!(error_code, "canceled")
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_by_child_connection_threads_terminal_error_into_reason() {
        // The lifecycle worker forwards the child's last AcpEvent::Error
        // detail through `cancel_by_child_connection`. The broker stitches it
        // into the `Canceled { reason }` message so the parent's
        // `delegate_to_agent` tool-call result surfaces the real failure
        // cause (e.g. Gemini OAuth expired) instead of the opaque default.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-auth".into())).await;
        mock.queue_send(Ok(77)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-auth")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker
            .cancel_by_child_connection("c-auth", Some("[auth_required] Authentication required"))
            .await;
        let outcome = driver.await.unwrap();
        match &outcome {
            DelegationOutcome::Err { code, message, .. } => {
                assert_eq!(code, "canceled");
                assert_eq!(
                    message,
                    "canceled: child session ended without TurnComplete: \
                     [auth_required] Authentication required"
                );
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_by_child_connection_ignores_empty_terminal_error() {
        // Whitespace-only or empty detail strings shouldn't produce a
        // dangling "...:" suffix on the reason — fall back to the default.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-empty".into())).await;
        mock.queue_send(Ok(78)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-empty")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker
            .cancel_by_child_connection("c-empty", Some("   "))
            .await;
        let outcome = driver.await.unwrap();
        match &outcome {
            DelegationOutcome::Err { message, .. } => {
                assert_eq!(
                    message,
                    "canceled: child session ended without TurnComplete"
                );
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emitter_records_one_event_per_drained_entry_on_cancel_by_parent() {
        let mock = Arc::new(MockSpawner::new());
        for i in 0..3 {
            mock.queue_spawn(Ok(format!("c{i}"))).await;
            mock.queue_send(Ok(100 + i)).await;
        }
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let mut handles = Vec::new();
        for i in 0..3 {
            let broker = broker.clone();
            handles.push(tokio::spawn(async move {
                broker.handle_request(request(1, &format!("pt-{i}"))).await
            }));
        }
        while broker.pending_count().await < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        for h in handles {
            let _ = h.await.unwrap();
        }

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 3, "expected 3 emits, got {calls:?}");
        let mut parent_tool_use_ids: Vec<String> =
            calls.iter().map(|c| c.parent_tool_use_id.clone()).collect();
        parent_tool_use_ids.sort();
        assert_eq!(
            parent_tool_use_ids,
            vec!["pt-0".to_string(), "pt-1".to_string(), "pt-2".to_string()]
        );
        for call in &calls {
            match &call.result {
                DelegationResultSummary::Err { error_code } => {
                    assert_eq!(error_code, "canceled")
                }
                other => panic!("expected Err{{canceled}}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn emitter_does_not_double_emit_on_repeat_cancel_by_parent() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-once".into())).await;
        mock.queue_send(Ok(42)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-idem")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // First call drains the entry + emits one.
        broker.cancel_by_parent("parent-conn").await;
        // Second call finds the pending map empty — no extra emit.
        broker.cancel_by_parent("parent-conn").await;
        // Cleanup-guard-style triple call also stays bounded.
        broker.cancel_by_parent("parent-conn").await;
        let _ = driver.await.unwrap();

        assert_eq!(emitter.count().await, 1);
    }

    #[tokio::test]
    async fn emitter_skipped_for_synthetic_parent_tool_use_id() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-synth".into())).await;
        mock.queue_send(Ok(8)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 8,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = emitter.snapshot().await;
        assert!(
            calls.is_empty(),
            "emitter must skip synthetic parent_tool_use_id (same rule as meta writer); got {calls:?}"
        );
    }

    #[tokio::test]
    async fn emitter_records_after_meta_write_on_complete_call() {
        // Frontend's snapshot-recovery path reads `meta["codeg.delegation"]`
        // first and the live event second; if the emit lands before the
        // meta write, a snapshot taken between them would see "running"
        // meta paired with a "completed" event. Enforce meta-before-emit
        // by checking the MockMetaWriter has at least one call before the
        // emitter records.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-order".into())).await;
        mock.queue_send(Ok(7)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone()).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-order")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let meta_calls = writer.snapshot().await;
        let event_calls = emitter.snapshot().await;
        // running (from handle_request) + completed (from complete_call) =
        // 2 meta writes. The single event must be the "completed" one,
        // and it must land AFTER the running meta — guaranteed structurally
        // by complete_call's order (write_meta_if_real then emit).
        assert_eq!(meta_calls.len(), 2);
        assert_eq!(event_calls.len(), 1);
        let inner_second = meta_calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            inner_second.get("status").unwrap().as_str().unwrap(),
            "completed"
        );
    }

    // -- Production-path fanout coverage ----------------------------------
    //
    // Every other emitter test in this module uses `MockEventEmitter`. The
    // production wiring goes through `ConnectionManagerEventEmitter`, which
    // resolves `(state, emitter)` against the live `ConnectionManager` and
    // hands the event to `emit_with_state` so it fans out to (1) the parent
    // connection's `ConnectionEventStream` (the WS attach path) and (2) the
    // `InternalEventBus` (the lifecycle/pet/chat-channel subscriber path).
    // These tests exercise that real fanout end-to-end so a regression in
    // `get_state_and_emitter` lookup, `emit_with_state` routing, or the
    // `EventEmitter::WebOnly { bus, .. }` wiring is caught here even when
    // every mock-backed test stays green.

    #[tokio::test]
    async fn real_emitter_fans_out_delegation_completed_to_parent_stream_and_bus() {
        use crate::acp::delegation::event_emitter::ConnectionManagerEventEmitter;
        use crate::acp::manager::ConnectionManager;
        use crate::acp::types::AcpEvent;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

        // Real ConnectionManager + fake parent wired to a WebOnly emitter so
        // the InternalEventBus gets typed envelopes and we can subscribe to
        // verify the lifecycle-path delivery alongside the per-connection
        // stream delivery.
        let manager = ConnectionManager::new();
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let parent_emitter = EventEmitter::test_web_only(broadcaster);
        let bus = parent_emitter
            .acp_event_bus()
            .expect("WebOnly emitter must expose an InternalEventBus");
        manager
            .insert_test_connection("parent-conn", AgentType::ClaudeCode, None, parent_emitter)
            .await;

        // Subscribe BEFORE triggering events — broadcast channels drop
        // sends that happen with no receivers registered.
        let mut bus_rx = bus.subscribe();
        let (parent_state, _) = manager
            .get_state_and_emitter("parent-conn")
            .await
            .expect("parent just inserted");
        let mut stream_rx = parent_state.read().await.event_stream().subscribe();

        // Build the broker with the PRODUCTION emitter; meta writer can stay
        // noop because this test is asserting the event-fanout invariant.
        let mock_spawner = Arc::new(MockSpawner::new());
        mock_spawner.queue_spawn(Ok("child-conn-real".into())).await;
        mock_spawner.queue_send(Ok(77)).await;
        let real_emitter = Arc::new(ConnectionManagerEventEmitter {
            manager: Arc::new(manager.clone_ref()),
        });
        let broker = DelegationBroker::with_writers(
            mock_spawner.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            Arc::new(crate::acp::delegation::meta_writer::NoopMetaWriter)
                as Arc<dyn crate::acp::delegation::meta_writer::DelegationMetaWriter>,
            real_emitter as Arc<dyn crate::acp::delegation::event_emitter::DelegationEventEmitter>,
        );
        enable_delegation(&broker).await;

        // Park a pending entry then trigger cancel_by_parent to drive the
        // production emit path. `request()` hard-codes parent_connection_id
        // = "parent-conn" which matches the insert above.
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-fanout")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        let _ = driver.await.unwrap();

        // Per-connection stream (WS attach delivery path) must receive the
        // envelope tagged with the right connection + payload shape.
        // The parent stream now also carries the setup-time DelegationStarted;
        // skip past it to the terminal DelegationCompleted.
        let envelope = loop {
            let env = tokio::time::timeout(Duration::from_millis(500), stream_rx.recv())
                .await
                .expect("per-connection stream should receive DelegationCompleted within 500ms")
                .expect("envelope recv must not error");
            if matches!(env.payload, AcpEvent::DelegationStarted { .. }) {
                continue;
            }
            break env;
        };
        assert_eq!(envelope.connection_id, "parent-conn");
        match &envelope.payload {
            AcpEvent::DelegationCompleted {
                parent_tool_use_id,
                child_connection_id,
                child_conversation_id,
                result,
                ..
            } => {
                assert_eq!(parent_tool_use_id, "pt-fanout");
                assert_eq!(child_connection_id, "child-conn-real");
                assert_eq!(*child_conversation_id, 77);
                match result {
                    DelegationResultSummary::Err { error_code } => {
                        assert_eq!(error_code, "canceled");
                    }
                    other => panic!("expected Err{{canceled}}, got {other:?}"),
                }
            }
            other => panic!("expected DelegationCompleted, got {other:?}"),
        }

        // InternalEventBus (lifecycle/pet/chat-channel subscriber path) must
        // also receive the same envelope — proves the WebOnly emitter's bus
        // arm in `emit_with_state` is reached.
        let bus_envelope = loop {
            let env = tokio::time::timeout(Duration::from_millis(500), bus_rx.recv())
                .await
                .expect("InternalEventBus should receive DelegationCompleted within 500ms")
                .expect("bus recv must not error");
            if matches!(env.payload, AcpEvent::DelegationStarted { .. }) {
                continue;
            }
            break env;
        };
        assert_eq!(bus_envelope.connection_id, "parent-conn");
        assert!(matches!(
            bus_envelope.payload,
            AcpEvent::DelegationCompleted { .. }
        ));
    }

    #[tokio::test]
    async fn real_emitter_fans_out_delegation_started_to_parent_stream_and_bus() {
        use crate::acp::delegation::event_emitter::ConnectionManagerEventEmitter;
        use crate::acp::manager::ConnectionManager;
        use crate::acp::types::AcpEvent;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

        // Web/server delivery shape: a real ConnectionManager + a fake parent on
        // a WebOnly emitter. `DelegationStarted` must land on the PARENT's
        // per-connection stream (the WS attach path the frontend subscribes to
        // in web/server mode) AND the InternalEventBus — mirroring the
        // completed-path invariant. This is the regression lock for the
        // web-mode live-delegation gap: before moving the emit to the parent
        // stream, started rode the (un-attached) child stream and was lost here.
        let manager = ConnectionManager::new();
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let parent_emitter = EventEmitter::test_web_only(broadcaster);
        let bus = parent_emitter
            .acp_event_bus()
            .expect("WebOnly emitter must expose an InternalEventBus");
        manager
            .insert_test_connection("parent-conn", AgentType::ClaudeCode, None, parent_emitter)
            .await;

        // Subscribe BEFORE triggering events — broadcast channels drop sends
        // that happen with no receivers registered.
        let mut bus_rx = bus.subscribe();
        let (parent_state, _) = manager
            .get_state_and_emitter("parent-conn")
            .await
            .expect("parent just inserted");
        let mut stream_rx = parent_state.read().await.event_stream().subscribe();

        let mock_spawner = Arc::new(MockSpawner::new());
        mock_spawner
            .queue_spawn(Ok("child-conn-started".into()))
            .await;
        mock_spawner.queue_send(Ok(88)).await;
        let real_emitter = Arc::new(ConnectionManagerEventEmitter {
            manager: Arc::new(manager.clone_ref()),
        });
        let broker = DelegationBroker::with_writers(
            mock_spawner.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            Arc::new(crate::acp::delegation::meta_writer::NoopMetaWriter)
                as Arc<dyn crate::acp::delegation::meta_writer::DelegationMetaWriter>,
            real_emitter as Arc<dyn crate::acp::delegation::event_emitter::DelegationEventEmitter>,
        );
        enable_delegation(&broker).await;

        // `started` fires during setup, before park — drive the request, wait
        // for it to park, then assert the envelope already arrived.
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-started")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let envelope = tokio::time::timeout(Duration::from_millis(500), stream_rx.recv())
            .await
            .expect("per-connection stream should receive DelegationStarted within 500ms")
            .expect("envelope recv must not error");
        assert_eq!(envelope.connection_id, "parent-conn");
        match &envelope.payload {
            AcpEvent::DelegationStarted {
                parent_connection_id,
                parent_tool_use_id,
                child_connection_id,
                child_conversation_id,
                agent_type,
                task_preview,
                task_id,
            } => {
                assert_eq!(parent_connection_id, "parent-conn");
                assert_eq!(parent_tool_use_id, "pt-started");
                assert_eq!(child_connection_id, "child-conn-started");
                assert_eq!(*child_conversation_id, 88);
                assert_eq!(*agent_type, AgentType::ClaudeCode);
                // The event labels the card even when the parent tool call's
                // raw_input never carried the arguments (identity-less hosts).
                assert_eq!(task_preview, "do x");
                assert!(!task_id.is_empty(), "broker-minted task id must ride the event");
            }
            other => panic!("expected DelegationStarted, got {other:?}"),
        }

        let bus_envelope = tokio::time::timeout(Duration::from_millis(500), bus_rx.recv())
            .await
            .expect("InternalEventBus should receive DelegationStarted within 500ms")
            .expect("bus recv must not error");
        assert_eq!(bus_envelope.connection_id, "parent-conn");
        assert!(matches!(
            bus_envelope.payload,
            AcpEvent::DelegationStarted { .. }
        ));

        // Drain the parked driver so the test doesn't leak the spawned task.
        broker.cancel_by_parent("parent-conn").await;
        let _ = driver.await.unwrap();
    }

    #[tokio::test]
    async fn real_emitter_is_silent_no_op_when_parent_already_detached() {
        // Parent torn down mid-delegation: `get_state_and_emitter` returns
        // None, the emit silently drops, BUT the broker still drains its
        // pending table and surfaces the outcome to the awaiting caller.
        // This is the "parent disappeared before terminal" path that the
        // mock-backed tests can't observe.
        use crate::acp::delegation::event_emitter::ConnectionManagerEventEmitter;
        use crate::acp::manager::ConnectionManager;

        let manager = ConnectionManager::new();
        // Intentionally no insert_test_connection — parent is absent.
        let real_emitter = Arc::new(ConnectionManagerEventEmitter {
            manager: Arc::new(manager.clone_ref()),
        });
        let mock_spawner = Arc::new(MockSpawner::new());
        mock_spawner.queue_spawn(Ok("c-orphan".into())).await;
        mock_spawner.queue_send(Ok(1)).await;
        let broker = DelegationBroker::with_writers(
            mock_spawner.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            Arc::new(crate::acp::delegation::meta_writer::NoopMetaWriter)
                as Arc<dyn crate::acp::delegation::meta_writer::DelegationMetaWriter>,
            real_emitter as Arc<dyn crate::acp::delegation::event_emitter::DelegationEventEmitter>,
        );
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-orphan")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        let outcome = driver.await.unwrap();

        assert!(matches!(
            outcome,
            DelegationOutcome::Err { ref code, .. } if code == "canceled"
        ));
        assert_eq!(
            broker.pending_count().await,
            0,
            "broker must drain pending even when no parent exists to receive the emit"
        );
    }

    // -- Async-cutover review regressions -----------------------------------

    /// A pre-cancel that bails `start_delegation` before the claim path must
    /// still drain the keyed ACP tool_call, so a later same-key delegation
    /// can't claim the canceled call's id and mis-bind to the wrong card.
    #[tokio::test]
    async fn pre_cancel_drains_keyed_tool_call_to_avoid_misbinding() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        enable_delegation(&broker).await;
        let key = DelegationMatchKey {
            agent_type: AgentType::ClaudeCode,
            task: "do x".into(),
            working_dir: None,
        };
        // The lifecycle registered the keyed tool_call for this delegation.
        broker
            .register_pending_tool_call_with_key("parent-conn", "tc-1".into(), Some(key.clone()))
            .await;
        // notifications/cancelled lands before the round-trip → buffered (no
        // running task yet).
        broker.cancel_by_external_handle("h-1", "user".into()).await;
        // The MCP round-trip arrives (empty parent_tool_use_id, same key) and
        // bails at the first pre-cancel check.
        let report = broker
            .start_delegation(request_with_handle(1, "", "h-1"))
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        // The keyed entry must have been drained — not claimable afterward.
        assert_eq!(
            broker.take_matching_tool_call("parent-conn", &key).await,
            None
        );
    }

    /// The running ack must carry the literal task_id in its message, so a
    /// client that only surfaces MCP `content` text (not `structuredContent`)
    /// can still call get_delegation_status / cancel_delegation.
    #[test]
    fn running_ack_message_embeds_task_id() {
        let report = running_ack("task-xyz".into(), 42, AgentType::Codex);
        assert_eq!(report.task_id.as_deref(), Some("task-xyz"));
        assert!(
            report.message.as_deref().unwrap().contains("task-xyz"),
            "ack message must embed the literal task_id, got {:?}",
            report.message
        );
    }

    /// Previews and cached text must stay within their advertised BYTE caps
    /// (including the appended ellipsis), and truncate on UTF-8 boundaries.
    #[test]
    fn previews_and_cached_text_respect_byte_caps() {
        let preview = build_text_preview(&"x".repeat(STATUS_PREVIEW_CAP * 2)).unwrap();
        assert!(
            preview.len() <= STATUS_PREVIEW_CAP,
            "preview {} > cap {STATUS_PREVIEW_CAP}",
            preview.len()
        );
        let cached = cap_completed_text(&"y".repeat(COMPLETED_TEXT_CAP * 2));
        assert!(cached.len() <= COMPLETED_TEXT_CAP);
        // Multibyte safety: 3-byte chars must not be split, and the cap holds.
        let multibyte = build_text_preview(&"€".repeat(STATUS_PREVIEW_CAP)).unwrap();
        assert!(multibyte.len() <= STATUS_PREVIEW_CAP);
        assert!(std::str::from_utf8(multibyte.as_bytes()).is_ok());
    }

    // -- completed-cache byte valve ----------------------------------------

    fn completed_with_text(parent: &str, text_len: usize) -> CompletedTask {
        CompletedTask {
            parent_connection_id: parent.to_string(),
            child_conversation_id: 1,
            agent_type: AgentType::ClaudeCode,
            status: TaskStatus::Completed,
            text: Some("x".repeat(text_len)),
            error_code: None,
            message: None,
            duration_ms: 0,
        }
    }

    #[test]
    fn completed_cache_valve_evicts_oldest_over_byte_budget() {
        let mut inner = PendingInner {
            completed_cap_bytes: 1000,
            ..Default::default()
        };
        // Three 400-byte results = 1200 bytes > 1000 cap. Oldest must evict.
        inner.insert_completed("a", completed_with_text("p1", 400));
        inner.insert_completed("b", completed_with_text("p1", 400));
        inner.insert_completed("c", completed_with_text("p1", 400));
        assert!(!inner.completed.contains_key("a"), "oldest must be evicted");
        assert!(inner.completed.contains_key("b"));
        assert!(inner.completed.contains_key("c"), "newest must be retained");
        // Counter + order reflect only the two retained entries.
        assert_eq!(inner.completed_bytes.get("p1").copied(), Some(800));
        assert_eq!(inner.completed_order.get("p1").map(|o| o.len()), Some(2));
        // Survivors keep their FULL text — the valve drops whole entries, it
        // never truncates a survivor.
        assert_eq!(
            inner
                .completed
                .get("c")
                .unwrap()
                .text
                .as_deref()
                .map(str::len),
            Some(400)
        );
    }

    #[test]
    fn completed_cache_valve_keeps_newest_even_if_alone_over_budget() {
        // A single result larger than the whole budget is still retained — the
        // valve never evicts the entry just inserted (the LLM's immediate
        // get_delegation_status must hit). Per-result text is independently
        // bounded by COMPLETED_TEXT_CAP.
        let mut inner = PendingInner {
            completed_cap_bytes: 100,
            ..Default::default()
        };
        inner.insert_completed("solo", completed_with_text("p1", 500));
        assert!(inner.completed.contains_key("solo"));
        assert_eq!(inner.completed_bytes.get("p1").copied(), Some(500));
    }

    #[test]
    fn completed_cache_unlimited_when_cap_zero() {
        let mut inner = PendingInner::default(); // completed_cap_bytes == 0
        for i in 0..50 {
            inner.insert_completed(&format!("t{i}"), completed_with_text("p1", 10_000));
        }
        assert_eq!(inner.completed.len(), 50, "cap 0 disables eviction");
        assert_eq!(inner.completed_bytes.get("p1").copied(), Some(500_000));
    }

    #[test]
    fn completed_cache_valve_is_per_parent() {
        let mut inner = PendingInner {
            completed_cap_bytes: 1000,
            ..Default::default()
        };
        // p1 overflows; p2 stays under its own independent budget.
        inner.insert_completed("a1", completed_with_text("p1", 600));
        inner.insert_completed("a2", completed_with_text("p1", 600)); // evicts a1
        inner.insert_completed("b1", completed_with_text("p2", 600));
        assert!(!inner.completed.contains_key("a1"));
        assert!(inner.completed.contains_key("a2"));
        assert!(
            inner.completed.contains_key("b1"),
            "p2 must be untouched by p1 overflow"
        );
        assert_eq!(inner.completed_bytes.get("p1").copied(), Some(600));
        assert_eq!(inner.completed_bytes.get("p2").copied(), Some(600));
    }

    #[test]
    fn drop_completed_for_parent_clears_byte_counter() {
        let mut inner = PendingInner::default(); // unlimited; teardown still clears
        inner.insert_completed("a", completed_with_text("p1", 100));
        inner.insert_completed("b", completed_with_text("p2", 100));
        inner.drop_completed_for_parent("p1");
        assert!(!inner.completed.contains_key("a"));
        assert!(inner.completed.contains_key("b"));
        assert_eq!(
            inner.completed_bytes.get("p1"),
            None,
            "byte counter must be cleared on teardown"
        );
        assert_eq!(inner.completed_bytes.get("p2").copied(), Some(100));
    }

    #[tokio::test]
    async fn lowering_cap_prunes_existing_completed_results() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        // Start unlimited and retain several results for one parent.
        broker
            .set_config(DelegationConfig {
                completed_cache_cap_bytes: 0,
                ..DelegationConfig::default()
            })
            .await;
        {
            let mut inner = broker.pending.inner.lock().await;
            for i in 0..5 {
                inner.insert_completed(&format!("t{i}"), completed_with_text("p1", 400));
            }
            assert_eq!(inner.completed.len(), 5);
            assert_eq!(inner.completed_bytes.get("p1").copied(), Some(2000));
        }
        // Lower the cap to 1000 bytes — existing results must be pruned NOW,
        // not only on the next completion (which may never arrive).
        broker
            .set_config(DelegationConfig {
                completed_cache_cap_bytes: 1000,
                ..DelegationConfig::default()
            })
            .await;
        let inner = broker.pending.inner.lock().await;
        assert!(
            inner.completed_bytes.get("p1").copied().unwrap_or(0) <= 1000,
            "retained bytes must fit the lowered cap"
        );
        assert!(
            inner.completed.contains_key("t4"),
            "newest result must survive pruning"
        );
        assert!(
            !inner.completed.contains_key("t0"),
            "oldest result must be pruned"
        );
    }

    // -- resume_delegation ---------------------------------------------------

    /// Test-only `ChildStatusLookup` whose resume context is seeded per test.
    /// Answers ANY call_id with the seeded context (broker tests mint task ids
    /// as UUIDs, so keying by exact id would force awkward post-hoc setup).
    #[derive(Default)]
    struct MockResumeLookup {
        ctx: Mutex<Option<ChildResumeContext>>,
    }

    #[async_trait]
    impl ChildStatusLookup for MockResumeLookup {
        async fn find_by_call_id(&self, _call_id: &str) -> Option<ChildStatusRecord> {
            self.ctx.lock().await.as_ref().map(|c| ChildStatusRecord {
                child_conversation_id: c.child_conversation_id,
                status: c.status,
                agent_type: c.agent_type,
                parent_id: c.parent_id,
            })
        }
        async fn find_resume_context_by_call_id(
            &self,
            _call_id: &str,
        ) -> Option<ChildResumeContext> {
            self.ctx.lock().await.clone()
        }
    }

    fn resume_ctx(status: TaskStatus) -> ChildResumeContext {
        ChildResumeContext {
            child_conversation_id: 42,
            status,
            agent_type: AgentType::ClaudeCode,
            parent_id: Some(1),
            parent_tool_use_id: Some("pt-orig".into()),
            external_id: Some("ext-session-1".into()),
            folder_id: 7,
            working_dir: Some("/work".into()),
            title: Some("do x".into()),
        }
    }

    fn resume_request(task_id: &str) -> ResumeDelegationRequest {
        ResumeDelegationRequest {
            parent_connection_id: "parent-conn".into(),
            parent_conversation_id: 1,
            task_id: task_id.into(),
            reason: None,
            external_handle: None,
        }
    }

    /// Broker wired with a MockSpawner + seedable resume lookup, enabled.
    async fn resume_harness(
        ctx: Option<ChildResumeContext>,
    ) -> (Arc<MockSpawner>, Arc<MockResumeLookup>, DelegationBroker) {
        let mock = Arc::new(MockSpawner::new());
        let lookup = Arc::new(MockResumeLookup::default());
        *lookup.ctx.lock().await = ctx;
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        )
        .with_status_lookup(lookup.clone() as Arc<dyn ChildStatusLookup>);
        enable_delegation(&broker).await;
        (mock, lookup, broker)
    }

    #[test]
    fn resume_prompt_carries_bounded_reason_only() {
        let bare = build_resume_prompt(None);
        assert!(bare.contains("interrupted before completion"));
        assert!(!bare.contains("Context on why"));
        // Whitespace-only reason collapses to the bare prompt.
        assert_eq!(build_resume_prompt(Some("   ")), bare);
        let with_reason = build_resume_prompt(Some("app crashed"));
        assert!(with_reason.contains("Context on why the run was interrupted"));
        assert!(with_reason.contains("app crashed"));
        // An oversized reason is truncated, not passed through.
        let huge = "x".repeat(10 * 1024);
        let capped = build_resume_prompt(Some(&huge));
        assert!(capped.len() < bare.len() + RESUME_REASON_CAP + 128);
    }

    /// A crash-interrupted task (nothing in memory, DB row `cancelled`) resumes:
    /// Running ack under the SAME id, the child re-spawned by loading the
    /// recorded agent session in the recorded working dir, the continuation
    /// prompt sent into the SAME conversation row — and a later `complete_call`
    /// on the unchanged id finishes it normally.
    #[tokio::test]
    async fn resume_after_interruption_runs_under_same_id_then_completes() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_resume_spawn(Ok(ResumedSpawn::fresh("child-conn-2"))).await;
        mock.queue_resume_send(Ok(())).await;

        let mut req = resume_request("task-1");
        req.reason = Some("machine rebooted".into());
        let ack = broker.resume_delegation(req).await;
        assert_eq!(ack.status, TaskStatus::Running);
        assert_eq!(ack.task_id.as_deref(), Some("task-1"));
        assert_eq!(ack.child_conversation_id, Some(42));
        assert!(ack.message.unwrap().starts_with("Delegation resumed. task_id=task-1"));

        // The spawn resumed the recorded agent session in the recorded dir.
        let spawns = mock.resume_spawn_args.lock().await;
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].external_session_id, "ext-session-1");
        assert_eq!(spawns[0].working_dir.as_deref(), Some("/work"));
        drop(spawns);
        // The continuation prompt targeted the SAME child row and carried the
        // reason as bounded context.
        let sends = mock.resume_send_args.lock().await;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].child_conversation_id, 42);
        assert_eq!(sends[0].folder_id, 7);
        assert!(sends[0].prompt.contains("machine rebooted"));
        drop(sends);

        // Status now reports Running for the same id, scoped to the caller.
        let report = broker
            .get_task_status("parent-conn", Some(1), "task-1", StatusWait::Immediate)
            .await;
        assert_eq!(report.status, TaskStatus::Running);

        // TurnComplete routes through the unchanged delegation_call_id.
        broker
            .complete_call(
                "task-1",
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "finished".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let report = broker
            .get_task_status("parent-conn", Some(1), "task-1", StatusWait::Immediate)
            .await;
        assert_eq!(report.status, TaskStatus::Completed);
        assert_eq!(report.text.as_deref(), Some("finished"));
    }

    /// An in-session cancel leaves a Canceled completed-cache entry; resuming
    /// must atomically clear it (with its byte/order bookkeeping) so the task
    /// classifies as Running again — and a later re-terminal re-inserts exactly
    /// one record.
    #[tokio::test]
    async fn resume_clears_canceled_cache_entry_and_reregisters() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        // Seed a real canceled task via the live path: delegate → cancel.
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let ack = broker.start_delegation(request(1, "pt-orig")).await;
        let task_id = ack.task_id.unwrap();
        assert_eq!(ack.status, TaskStatus::Running);
        let canceled = broker
            .cancel_task_by_id("parent-conn", Some(1), &task_id)
            .await;
        assert_eq!(canceled.status, TaskStatus::Canceled);
        assert_eq!(broker.completed_count().await, 1);

        mock.queue_resume_spawn(Ok(ResumedSpawn::fresh("child-conn-2"))).await;
        mock.queue_resume_send(Ok(())).await;
        let ack = broker.resume_delegation(resume_request(&task_id)).await;
        assert_eq!(ack.status, TaskStatus::Running);
        // The stale canceled record is gone; the task is running again.
        assert_eq!(broker.completed_count().await, 0);
        assert_eq!(broker.pending_count().await, 1);

        // Re-terminal lands exactly one completed record under the same id.
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done after resume".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        assert_eq!(broker.completed_count().await, 1);
        let report = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Immediate)
            .await;
        assert_eq!(report.status, TaskStatus::Completed);
    }

    /// A still-running task is NOT resumable — and stays untouched.
    #[tokio::test]
    async fn resume_refuses_running_task() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let ack = broker.start_delegation(request(1, "pt-orig")).await;
        let task_id = ack.task_id.unwrap();

        let report = broker.resume_delegation(resume_request(&task_id)).await;
        assert_eq!(report.status, TaskStatus::Running);
        assert_eq!(report.error_code.as_deref(), Some(NOT_RESUMABLE_CODE));
        assert!(report.message.unwrap().starts_with("Not resumed:"));
        // No resume spawn happened; the running task is still there.
        assert!(mock.resume_spawn_args.lock().await.is_empty());
        assert_eq!(broker.pending_count().await, 1);
    }

    /// A task that completed on its own is NOT resumable (cache-fresh variant).
    #[tokio::test]
    async fn resume_refuses_completed_task() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let ack = broker.start_delegation(request(1, "pt-orig")).await;
        let task_id = ack.task_id.unwrap();
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;

        let report = broker.resume_delegation(resume_request(&task_id)).await;
        assert_eq!(report.status, TaskStatus::Completed);
        assert_eq!(report.error_code.as_deref(), Some(NOT_RESUMABLE_CODE));
        assert!(mock.resume_spawn_args.lock().await.is_empty());
        // The completed record is untouched — its result stays collectable.
        let status = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Immediate)
            .await;
        assert_eq!(status.status, TaskStatus::Completed);
        assert_eq!(status.text.as_deref(), Some("done"));
    }

    /// A failure the child produced itself (refusal etc.) is NOT an
    /// interruption — refused with the task's actual Failed status.
    #[tokio::test]
    async fn resume_refuses_failed_task() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let ack = broker.start_delegation(request(1, "pt-orig")).await;
        let task_id = ack.task_id.unwrap();
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::from_err(DelegationError::ChildRefusal, Some(42)),
            )
            .await;

        let report = broker.resume_delegation(resume_request(&task_id)).await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some(NOT_RESUMABLE_CODE));
        assert!(mock.resume_spawn_args.lock().await.is_empty());
    }

    /// DB says the child row is still `in_progress`: with NO live connection it
    /// is a crash leftover (resumable); with one it is genuinely active
    /// (refused).
    #[tokio::test]
    async fn resume_stale_in_progress_row_depends_on_live_connection() {
        // Live connection bound to the child conversation → refuse.
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Running))).await;
        mock.mark_conversation_live(42).await;
        let report = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(report.status, TaskStatus::Running);
        assert_eq!(report.error_code.as_deref(), Some(NOT_RESUMABLE_CODE));
        assert!(mock.resume_spawn_args.lock().await.is_empty());

        // No live connection → the stranded row resumes.
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Running))).await;
        mock.queue_resume_spawn(Ok(ResumedSpawn::fresh("child-conn-2"))).await;
        mock.queue_resume_send(Ok(())).await;
        let ack = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(ack.status, TaskStatus::Running);
        assert_eq!(ack.error_code, None);
        assert_eq!(broker.pending_count().await, 1);
    }

    /// Unknown ids, cross-parent ids, and rows owned by another conversation
    /// all stay opaque — exactly the status-tool scoping rules.
    #[tokio::test]
    async fn resume_scoping_unknown_and_cross_parent() {
        // No DB row at all.
        let (mock, _lookup, broker) = resume_harness(None).await;
        let report = broker.resume_delegation(resume_request("nope")).await;
        assert_eq!(report.status, TaskStatus::Unknown);
        assert!(mock.resume_spawn_args.lock().await.is_empty());

        // Row owned by a different parent conversation.
        let mut foreign = resume_ctx(TaskStatus::Canceled);
        foreign.parent_id = Some(99);
        let (mock, _lookup, broker) = resume_harness(Some(foreign)).await;
        let report = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(report.status, TaskStatus::Unknown);
        assert!(mock.resume_spawn_args.lock().await.is_empty());

        // Canceled cache entry owned by ANOTHER parent connection: opaque.
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let ack = broker.start_delegation(request(1, "pt-orig")).await;
        let task_id = ack.task_id.unwrap();
        broker
            .cancel_task_by_id("parent-conn", Some(1), &task_id)
            .await;
        let mut req = resume_request(&task_id);
        req.parent_connection_id = "other-parent".into();
        let report = broker.resume_delegation(req).await;
        assert_eq!(report.status, TaskStatus::Unknown);
    }

    /// A child row without a recorded agent session id has nothing to load.
    #[tokio::test]
    async fn resume_refuses_when_no_external_session_id() {
        let mut ctx = resume_ctx(TaskStatus::Canceled);
        ctx.external_id = None;
        let (mock, _lookup, broker) = resume_harness(Some(ctx)).await;
        let report = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(report.error_code.as_deref(), Some(NOT_RESUMABLE_CODE));
        assert!(report.message.unwrap().contains("no agent session id"));
        assert!(mock.resume_spawn_args.lock().await.is_empty());
    }

    /// Resume honors the global delegation switch like every other entry point.
    #[tokio::test]
    async fn resume_refuses_when_delegation_disabled() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        broker
            .set_config(DelegationConfig {
                enabled: false,
                ..DelegationConfig::default()
            })
            .await;
        let report = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert!(report.message.unwrap().contains("delegation disabled"));
        assert!(mock.resume_spawn_args.lock().await.is_empty());
    }

    /// A resume whose spawn fails reports the failure but leaves the task's
    /// canceled record intact — the task is still canceled, still resumable.
    #[tokio::test]
    async fn resume_spawn_failure_preserves_canceled_record() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let ack = broker.start_delegation(request(1, "pt-orig")).await;
        let task_id = ack.task_id.unwrap();
        broker
            .cancel_task_by_id("parent-conn", Some(1), &task_id)
            .await;

        mock.queue_resume_spawn(Err(SpawnerError::Spawn("agent not installed".into())))
            .await;
        let report = broker.resume_delegation(resume_request(&task_id)).await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("spawn_failed"));
        // The canceled record still answers status queries.
        let status = broker
            .get_task_status("parent-conn", Some(1), &task_id, StatusWait::Immediate)
            .await;
        assert_eq!(status.status, TaskStatus::Canceled);
        // In-flight bookkeeping is clean.
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(broker.reserved_call_count().await, 0);
    }

    /// A resume whose SEND fails (after spawn) disconnects the fresh child and
    /// releases the reservation.
    #[tokio::test]
    async fn resume_send_failure_disconnects_and_unreserves() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_resume_spawn(Ok(ResumedSpawn::fresh("child-conn-2"))).await;
        mock.queue_resume_send(Err(SpawnerError::Send("stdin closed".into())))
            .await;
        let report = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("spawn_failed"));
        assert_eq!(
            mock.disconnects.lock().await.as_slice(),
            &["child-conn-2".to_string()]
        );
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(broker.reserved_call_count().await, 0);
        assert_eq!(broker.pending_count().await, 0);
    }

    /// While one resume of a task is mid-setup, a second resume of the SAME id
    /// is refused (owner-visible "already in progress") instead of spawning a
    /// second child onto the same agent session — in both gate branches: the
    /// caller-owned canceled cache entry, and the crash case where ownership is
    /// only proven by the DB row.
    #[tokio::test]
    async fn concurrent_resume_of_same_task_is_refused() {
        // Cache-owned branch.
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let ack = broker.start_delegation(request(1, "pt-orig")).await;
        let task_id = ack.task_id.unwrap();
        broker
            .cancel_task_by_id("parent-conn", Some(1), &task_id)
            .await;
        broker
            .pending
            .inner
            .lock()
            .await
            .resuming
            .insert(task_id.clone());
        let report = broker.resume_delegation(resume_request(&task_id)).await;
        assert_eq!(report.error_code.as_deref(), Some(NOT_RESUMABLE_CODE));
        assert!(report.message.unwrap().contains("already in progress"));
        assert!(mock.resume_spawn_args.lock().await.is_empty());

        // Not-in-memory branch: the refusal is deferred until the DB proves
        // ownership, and the contended claim is left in place (it belongs to
        // the resume that holds it).
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        broker
            .pending
            .inner
            .lock()
            .await
            .resuming
            .insert("task-1".into());
        let report = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(report.error_code.as_deref(), Some(NOT_RESUMABLE_CODE));
        assert!(report.message.unwrap().contains("already in progress"));
        assert!(mock.resume_spawn_args.lock().await.is_empty());
        assert!(broker.pending.inner.lock().await.resuming.contains("task-1"));

        // A successful resume releases its own claim.
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_resume_spawn(Ok(ResumedSpawn::fresh("child-conn-2"))).await;
        mock.queue_resume_send(Ok(())).await;
        let ack = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(ack.status, TaskStatus::Running);
        assert!(broker.pending.inner.lock().await.resuming.is_empty());
    }

    /// A failed send on a DEDUP-REUSED connection must NOT disconnect it — it
    /// belongs to whoever was already driving it (the user's open tab that
    /// raced past the live-connection gate). Only broker-spawned connections
    /// are torn down; bookkeeping still unwinds either way.
    #[tokio::test]
    async fn resume_send_failure_on_reused_connection_skips_disconnect() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_resume_spawn(Ok(ResumedSpawn::reused("user-conn")))
            .await;
        mock.queue_resume_send(Err(SpawnerError::Send("turn in flight".into())))
            .await;
        let report = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("spawn_failed"));
        assert!(
            mock.disconnects.lock().await.is_empty(),
            "a reused connection must be left alone"
        );
        assert_eq!(broker.inflight_count().await, 0);
        assert_eq!(broker.reserved_call_count().await, 0);
        assert!(broker.pending.inner.lock().await.resuming.is_empty());
    }

    /// A resumed task is cancelable again through the ordinary path — the
    /// running entry belongs to the CALLING parent connection.
    #[tokio::test]
    async fn resumed_task_cancels_like_any_running_task() {
        let (mock, _lookup, broker) = resume_harness(Some(resume_ctx(TaskStatus::Canceled))).await;
        mock.queue_resume_spawn(Ok(ResumedSpawn::fresh("child-conn-2"))).await;
        mock.queue_resume_send(Ok(())).await;
        let ack = broker.resume_delegation(resume_request("task-1")).await;
        assert_eq!(ack.status, TaskStatus::Running);

        let report = broker
            .cancel_task_by_id("parent-conn", Some(1), "task-1")
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert!(mock
            .cancels
            .lock()
            .await
            .contains(&"child-conn-2".to_string()));
        assert!(mock
            .disconnects
            .lock()
            .await
            .contains(&"child-conn-2".to_string()));
    }

    // -- [crewchat] team scope gate (Phase 1.3) -----------------------------

    /// `TeamLookup` stub: every conversation resolves to this fixed member
    /// list, so the gate can be driven without a DB or roster file.
    struct StubTeamLookup(Vec<crate::acp::delegation::team::TeamMember>);

    #[async_trait]
    impl crate::acp::delegation::team::TeamLookup for StubTeamLookup {
        async fn members_of(
            &self,
            _conversation_id: i32,
        ) -> Vec<crate::acp::delegation::team::TeamMember> {
            self.0.clone()
        }

        async fn conversation_role(
            &self,
            _conversation_id: i32,
        ) -> Option<crate::models::agent_role::AgentRole> {
            None
        }
    }

    fn team_member(name: &str, agent_type: &str) -> crate::acp::delegation::team::TeamMember {
        crate::acp::delegation::team::TeamMember {
            name: name.into(),
            agent_type: agent_type.into(),
            duty: None,
            role_id: format!("role-{name}"),
        }
    }

    #[tokio::test]
    async fn out_of_team_delegation_is_refused_before_any_spawn() {
        let mock = Arc::new(MockSpawner::new());
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup())
                .with_team_lookup(Arc::new(StubTeamLookup(vec![
                    team_member("前端工程师", "codex"),
                    team_member("后端开发工程师", "open_code"),
                ])));
        enable_delegation(&broker).await;

        // `request()` delegates to ClaudeCode — not a member of this team.
        let outcome = broker.handle_request(request(1, "pt-outside")).await;
        let DelegationOutcome::Err {
            code,
            message,
            child_conversation_id,
        } = outcome
        else {
            panic!("expected a terminal refusal, got {outcome:?}");
        };
        assert_eq!(code, "not_in_team");
        assert!(message.contains("前端工程师 (codex)"), "{message}");
        assert!(message.contains("后端开发工程师 (open_code)"), "{message}");
        assert_eq!(child_conversation_id, None);
        assert!(
            mock.spawn_args.lock().await.is_empty(),
            "a refused delegation must not reserve a child"
        );
    }

    #[tokio::test]
    async fn an_in_team_delegation_passes_the_scope_gate() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(7)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup())
                .with_team_lookup(Arc::new(StubTeamLookup(vec![team_member(
                    "运维工程师",
                    "claude_code",
                )])));
        enable_delegation(&broker).await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-inside")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)), "got {outcome:?}");
        assert_eq!(mock.spawn_args.lock().await.len(), 1);
        // [crewchat] Phase 2.2: the child row is stamped with the member's
        // roster role, which is how the child session resolves its persona.
        let links = mock.delegation_links.lock().await;
        assert_eq!(
            links.last().and_then(|link| link.member_role_id.as_deref()),
            Some("role-运维工程师")
        );
    }
