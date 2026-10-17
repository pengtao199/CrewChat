    use super::*;
    use crate::models::WorkTaskMergeOp;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};

    fn draft(folder_id: i32, title: &str) -> WorkTaskDraft {
        WorkTaskDraft {
            folder_id,
            title: title.to_string(),
            config: serde_json::json!({
                "display_text": "do the thing",
                "prompt_blocks": [{ "type": "text", "text": "do the thing" }],
            }),
        }
    }

    #[tokio::test]
    async fn create_list_get_roundtrip_and_validation() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-test").await;

        assert!(create(&db.conn, draft(folder_id, "")).await.is_err());
        let mut no_prompt = draft(folder_id, "x");
        no_prompt.config = serde_json::json!({ "display_text": "", "prompt_blocks": [] });
        assert!(create(&db.conn, no_prompt).await.is_err());

        let t = create(&db.conn, draft(folder_id, "fix login")).await.unwrap();
        assert_eq!(t.status, WorkTaskStatus::Todo);
        assert_eq!(t.run_seq, 0);

        let listed = list(&db.conn, Some(folder_id)).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(get(&db.conn, t.id).await.unwrap().id, t.id);

        // The created event landed in the same transaction.
        let events = list_events(&db.conn, t.id, 100).await.unwrap();
        assert_eq!(events[0].kind, "created");
    }

    /// The card's live note line: whose news it is, and what it says while a
    /// round is parked on its pre-prompt compaction.
    #[tokio::test]
    async fn the_live_note_belongs_to_the_current_generation() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-live-note").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let first = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, t.id, first).await.unwrap());
        assert!(mark_running(&db.conn, t.id, first, 7, "conn-1").await.unwrap());
        record_event(
            &db.conn,
            t.id,
            "agent_progress",
            "agent",
            Some(serde_json::json!({ "message": "installing deps", "run_seq": first })),
        )
        .await
        .unwrap();
        let one = |v: &Vec<WorkTaskInfo>| v.iter().find(|i| i.id == t.id).cloned().unwrap();
        let row = one(&list(&db.conn, Some(folder_id)).await.unwrap());
        assert_eq!(row.latest_progress.as_deref(), Some("installing deps"));
        assert!(!row.compacting);

        // A merge is a NEW generation. Its card used to inherit the work
        // round's last milestone and narrate work it is not doing.
        assert!(settle_review(&db.conn, t.id, first, None, None).await.unwrap());
        let merge = begin_merge(
            &db.conn,
            t.id,
            &WorkTaskMergeState::default(),
            first,
            false,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_ne!(merge, first);
        let row = one(&list(&db.conn, Some(folder_id)).await.unwrap());
        assert_eq!(row.latest_progress, None, "the previous round's news is not this one's");

        // The compaction announces itself when the turn goes out…
        let compact = |status: &str| {
            record_event(
                &db.conn,
                t.id,
                "context_compact",
                "engine",
                Some(serde_json::json!({ "status": status, "run_seq": merge })),
            )
        };
        compact("started").await.unwrap();
        assert!(one(&list(&db.conn, Some(folder_id)).await.unwrap()).compacting);
        // …and the outcome retires it, whatever the outcome was.
        compact("canceled").await.unwrap();
        assert!(!one(&list(&db.conn, Some(folder_id)).await.unwrap()).compacting);
    }

    /// `preparing` is a live status for the note line: a resumed round spends
    /// it on the compaction turn, which is the whole reason the line exists.
    /// A generation that never compacted must not inherit the last one's
    /// dangling `started` (a launch killed mid-compaction leaves one).
    #[tokio::test]
    async fn a_preparing_round_reports_its_own_compaction_only() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-preparing-note").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let first = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, t.id, first).await.unwrap());
        record_event(
            &db.conn,
            t.id,
            "context_compact",
            "engine",
            Some(serde_json::json!({ "status": "started", "run_seq": first })),
        )
        .await
        .unwrap();
        let one = |v: &Vec<WorkTaskInfo>| v.iter().find(|i| i.id == t.id).cloned().unwrap();
        let row = one(&list(&db.conn, Some(folder_id)).await.unwrap());
        assert_eq!(row.status, WorkTaskStatus::Preparing);
        assert!(row.compacting, "preparing is where a resumed round compacts");

        // Killed mid-compaction; the sweep hands it back and the next run
        // starts clean.
        assert!(abandon_setup(&db.conn, t.id, first).await.unwrap());
        let next = claim_for_run(&db.conn, t.id, WorkTaskStatus::Queued, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, t.id, next).await.unwrap());
        assert!(
            !one(&list(&db.conn, Some(folder_id)).await.unwrap()).compacting,
            "a dangling `started` answers only for the generation that wrote it"
        );
    }

    #[tokio::test]
    async fn claim_cas_is_exclusive_and_bumps_run_seq() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-claim").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap();
        assert_eq!(seq, Some(1));
        // Second claim of the same todo loses (status is now queued).
        assert_eq!(
            claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
                .await
                .unwrap(),
            None
        );
        assert_eq!(get(&db.conn, t.id).await.unwrap().status, WorkTaskStatus::Queued);
    }

    #[tokio::test]
    async fn setup_transition_is_generation_guarded_and_reversible() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-setup").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();

        // A stale generation loses; the live one wins exactly once.
        assert!(!begin_setup(&db.conn, t.id, seq + 1).await.unwrap());
        assert!(begin_setup(&db.conn, t.id, seq).await.unwrap());
        assert_eq!(
            get(&db.conn, t.id).await.unwrap().status,
            WorkTaskStatus::Preparing
        );
        assert!(!begin_setup(&db.conn, t.id, seq).await.unwrap());
        // Setup is the SAME run as the queue wait — no new generation.
        assert_eq!(get_model(&db.conn, t.id).await.unwrap().run_seq, seq);

        // The orphan sweep hands it back for a clean relaunch.
        assert!(!abandon_setup(&db.conn, t.id, seq + 1).await.unwrap());
        assert!(abandon_setup(&db.conn, t.id, seq).await.unwrap());
        assert_eq!(
            get(&db.conn, t.id).await.unwrap().status,
            WorkTaskStatus::Queued
        );
        // todo→queued, queued→preparing, preparing→queued — all on the timeline.
        let events = list_events(&db.conn, t.id, 100).await.unwrap();
        assert_eq!(
            events.iter().filter(|e| e.kind == "status_changed").count(),
            3
        );
    }

    /// What a `preparing` row's `connection_id` is allowed to mean: the LIVE
    /// connection of the setup in progress, or nothing at all. Never the
    /// previous generation's, which is dead by the time a new one is claimed —
    /// a viewer that streams this status (the pre-prompt compaction is a real
    /// agent turn) would otherwise open onto a connection that is gone.
    #[tokio::test]
    async fn a_preparing_row_names_the_live_connection_or_none() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-preparing-live").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, t.id, seq).await.unwrap());
        assert!(mark_preparing_live(&db.conn, t.id, seq, 42, "conn-1").await.unwrap());
        let row = get_model(&db.conn, t.id).await.unwrap();
        assert_eq!(row.connection_id.as_deref(), Some("conn-1"));
        assert_eq!(row.conversation_id, Some(42));
        // The coordinates move; the status does not — `running` is the round's
        // own turn, and every settle path keys off it.
        assert_eq!(row.status, WorkTaskStatus::Preparing);
        assert!(row.started_at.is_none());

        // A stale generation cannot repoint a live row.
        assert!(!mark_preparing_live(&db.conn, t.id, seq + 1, 99, "conn-stale")
            .await
            .unwrap());
        assert_eq!(
            get_model(&db.conn, t.id).await.unwrap().connection_id.as_deref(),
            Some("conn-1")
        );

        // Requeued and relaunched: the next setup starts with the dead
        // connection cleared, and publishes its own when it has one.
        let next = claim_for_run(&db.conn, t.id, WorkTaskStatus::Preparing, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, t.id, next).await.unwrap());
        assert!(get_model(&db.conn, t.id).await.unwrap().connection_id.is_none());
    }

    #[tokio::test]
    async fn preparing_is_cancelable_and_interrupted_by_a_restart() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-preparing").await;

        // Canceled mid-setup (the init command is still installing).
        let canceled = create(&db.conn, draft(folder_id, "canceled")).await.unwrap();
        let seq = claim_for_run(&db.conn, canceled.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, canceled.id, seq).await.unwrap());
        assert!(cancel(&db.conn, canceled.id, None).await.unwrap());
        assert_eq!(
            get(&db.conn, canceled.id).await.unwrap().status,
            WorkTaskStatus::Canceled
        );

        // Still setting up when the process died → interrupted, like queued.
        let cut = create(&db.conn, draft(folder_id, "cut")).await.unwrap();
        let seq = claim_for_run(&db.conn, cut.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, cut.id, seq).await.unwrap());
        assert_eq!(boot_reconcile_interrupted(&db.conn).await.unwrap(), 1);
        let row = get_model(&db.conn, cut.id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Failed);
        assert_eq!(row.failure_reason.as_deref(), Some("interrupted"));
    }

    #[tokio::test]
    async fn auto_claim_counts_preparing_against_the_budget() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-auto-preparing").await;
        // The one slot is spent by a task that already left the queue and is
        // setting up — the scheduler cannot see the engine's in-memory map, so
        // `preparing` has to count in SQL or the folder over-launches.
        let busy = create(&db.conn, draft(folder_id, "busy")).await.unwrap();
        let seq = claim_for_run(&db.conn, busy.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(begin_setup(&db.conn, busy.id, seq).await.unwrap());
        let next = create(&db.conn, draft(folder_id, "next")).await.unwrap();

        assert_eq!(auto_claim_next(&db.conn, folder_id, 1).await.unwrap(), None);
        assert_eq!(
            get(&db.conn, next.id).await.unwrap().status,
            WorkTaskStatus::Todo
        );
    }

    #[tokio::test]
    async fn auto_claim_counts_queued_against_the_budget() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-auto-budget").await;

        // Budget 2, spent by one running + one manually queued task.
        let running = create(&db.conn, draft(folder_id, "running")).await.unwrap();
        let seq = claim_for_run(&db.conn, running.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, running.id, seq, 1, "c1").await.unwrap());
        let queued = create(&db.conn, draft(folder_id, "queued")).await.unwrap();
        claim_for_run(&db.conn, queued.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        let todo = create(&db.conn, draft(folder_id, "todo")).await.unwrap();

        assert_eq!(auto_claim_next(&db.conn, folder_id, 2).await.unwrap(), None);
        assert_eq!(
            get(&db.conn, todo.id).await.unwrap().status,
            WorkTaskStatus::Todo
        );

        // The running task settles → a slot frees → the head is claimed with a
        // fresh generation and an engine-actor event.
        assert!(settle_review(&db.conn, running.id, seq, None, None).await.unwrap());
        assert_eq!(
            auto_claim_next(&db.conn, folder_id, 2).await.unwrap(),
            Some(todo.id)
        );
        let claimed = get(&db.conn, todo.id).await.unwrap();
        assert_eq!(claimed.status, WorkTaskStatus::Queued);
        assert_eq!(claimed.run_seq, 1);
        let events = list_events(&db.conn, todo.id, 10).await.unwrap();
        assert!(events
            .iter()
            .any(|e| e.kind == "status_changed" && e.actor == "engine"));

        // Budget full again; nothing left to claim either way.
        assert_eq!(auto_claim_next(&db.conn, folder_id, 2).await.unwrap(), None);
    }

    #[tokio::test]
    async fn auto_claim_drains_in_board_order_and_zero_is_unlimited() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-auto-order").await;
        let a = create(&db.conn, draft(folder_id, "a")).await.unwrap();
        let b = create(&db.conn, draft(folder_id, "b")).await.unwrap();
        let c = create(&db.conn, draft(folder_id, "c")).await.unwrap();

        assert_eq!(auto_claim_next(&db.conn, folder_id, 2).await.unwrap(), Some(a.id));
        assert_eq!(auto_claim_next(&db.conn, folder_id, 2).await.unwrap(), Some(b.id));
        // Two queued spend the budget; c stays visible in todo.
        assert_eq!(auto_claim_next(&db.conn, folder_id, 2).await.unwrap(), None);
        assert_eq!(get(&db.conn, c.id).await.unwrap().status, WorkTaskStatus::Todo);
        // 0 = unlimited.
        assert_eq!(auto_claim_next(&db.conn, folder_id, 0).await.unwrap(), Some(c.id));
        assert_eq!(auto_claim_next(&db.conn, folder_id, 0).await.unwrap(), None);
    }

    #[tokio::test]
    async fn schedule_is_todo_only_and_fires_exactly_once() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-schedule").await;
        let due = create(&db.conn, draft(folder_id, "due")).await.unwrap();
        let later = create(&db.conn, draft(folder_id, "later")).await.unwrap();

        let now = Utc::now();
        assert!(set_schedule(&db.conn, due.id, Some(now - chrono::Duration::minutes(1)))
            .await
            .unwrap());
        assert!(set_schedule(&db.conn, later.id, Some(now + chrono::Duration::hours(2)))
            .await
            .unwrap());
        assert!(get(&db.conn, due.id).await.unwrap().scheduled_at.is_some());

        // Only what is due is claimed, and the plan is consumed by the claim.
        let claimed = claim_due_scheduled(&db.conn, now).await.unwrap();
        assert_eq!(claimed, vec![(due.id, folder_id)]);
        let row = get(&db.conn, due.id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Queued);
        assert_eq!(row.run_seq, 1);
        assert!(row.scheduled_at.is_none());
        assert_eq!(
            get(&db.conn, later.id).await.unwrap().status,
            WorkTaskStatus::Todo
        );
        // Exactly once: a second sweep (or a restart's catch-up pass) finds
        // nothing, because the row no longer carries a plan.
        assert!(claim_due_scheduled(&db.conn, now).await.unwrap().is_empty());

        // Planning is a to-do-only affair — the queued task above refuses.
        assert!(!set_schedule(&db.conn, due.id, Some(now)).await.unwrap());

        // Clearing puts the task back under manual/auto control.
        assert!(set_schedule(&db.conn, later.id, None).await.unwrap());
        assert!(get(&db.conn, later.id).await.unwrap().scheduled_at.is_none());
        let events = list_events(&db.conn, later.id, 50).await.unwrap();
        let actions: Vec<&str> = events
            .iter()
            .filter(|e| e.kind == "user_action")
            .filter_map(|e| e.payload.as_ref()?.get("action")?.as_str())
            .collect();
        assert_eq!(actions, vec!["schedule", "unschedule"]);
    }

    /// #649: start, cancel, requeue, start again used to land the task in
    /// `canceled`. The requeue kept the conversation of the run the user had
    /// just stopped, and `launch_mode_for` reads exactly that column to pick
    /// `Retry` over `Fresh`, so the second start resumed the killed session
    /// instead of running the task, on a conversation row still recorded as
    /// `cancelled`, which the reconcile sweep then read as this generation's
    /// verdict. A requeue puts the task back on the board, so it keeps the
    /// worktree and drops the session.
    #[tokio::test]
    async fn a_requeue_drops_the_canceled_run_s_conversation() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-requeue-conversation").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        attach_worktree(&db.conn, t.id, folder_id, "main", "abc123", "task/t")
            .await
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 41, "c1").await.unwrap());
        assert!(cancel(&db.conn, t.id, None).await.unwrap());
        assert_eq!(
            get_model(&db.conn, t.id).await.unwrap().conversation_id,
            Some(41),
            "cancel keeps the link so the stopped run stays reachable"
        );

        assert!(requeue_canceled(&db.conn, t.id, None, &[], false)
            .await
            .unwrap());
        let row = get_model(&db.conn, t.id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Todo);
        assert_eq!(
            row.conversation_id, None,
            "a requeued task must start over, not resume the canceled session"
        );
        assert_eq!(
            row.worktree_folder_id,
            Some(folder_id),
            "the worktree is still reused"
        );
        // …and `started_at` outlives the requeue, unlike `finished_at` beside
        // it. `compose_prompt`'s fresh arm reads exactly this column to decide
        // whether to warn the new session that the worktree already holds an
        // earlier run's work: clearing it here for symmetry would silently send
        // a re-queued task back in reading like a first run.
        assert!(
            row.started_at.is_some(),
            "the fresh arm's 'this worktree is not empty' warning keys off it"
        );
        assert!(row.finished_at.is_none());

        // Retry (failed -> queued) is the path that DOES continue the same
        // session, and it is untouched.
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 42, "c2").await.unwrap());
        assert!(fail(
            &db.conn,
            t.id,
            &[WorkTaskStatus::Running],
            Some(seq),
            "agent_error",
            Some("boom".to_string()),
        )
        .await
        .unwrap());
        assert!(
            claim_for_run(&db.conn, t.id, WorkTaskStatus::Failed, "user")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            get_model(&db.conn, t.id).await.unwrap().conversation_id,
            Some(42),
            "retry continues the same session"
        );
    }

    /// A plan must not outlive the task's stay in `todo`: cancel drops it, so
    /// a requeue weeks later cannot launch an agent at a time nobody remembers
    /// setting. And a claim — from any arm — invalidates the previous run's
    /// self-reported verdict, which the settle path reads as this generation's.
    #[tokio::test]
    async fn a_claim_or_a_cancel_consumes_the_plan_and_the_stale_verdict() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-schedule-stale").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        // Run once and let the agent report, then abandon it back to the board.
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "c1").await.unwrap());
        assert!(set_verdict(&db.conn, t.id, seq, "blocked", Some("gave up"))
            .await
            .unwrap());
        assert!(cancel(&db.conn, t.id, None).await.unwrap());
        assert!(requeue_canceled(&db.conn, t.id, None, &[], false).await.unwrap());
        assert_eq!(
            get_model(&db.conn, t.id).await.unwrap().verdict.as_deref(),
            Some("blocked"),
            "requeue keeps the old report visible — the claim is what clears it"
        );

        // Plan it, then stop it: the plan goes with the cancel, and requeuing
        // must not bring it back.
        let now = Utc::now();
        assert!(set_schedule(&db.conn, t.id, Some(now + chrono::Duration::hours(1)))
            .await
            .unwrap());
        assert!(cancel(&db.conn, t.id, None).await.unwrap());
        assert!(get_model(&db.conn, t.id).await.unwrap().scheduled_at.is_none());
        assert!(requeue_canceled(&db.conn, t.id, None, &[], false).await.unwrap());
        assert!(get_model(&db.conn, t.id).await.unwrap().scheduled_at.is_none());

        // A due plan claims the task and clears the stale verdict with it.
        assert!(set_schedule(&db.conn, t.id, Some(now - chrono::Duration::minutes(1)))
            .await
            .unwrap());
        assert_eq!(
            claim_due_scheduled(&db.conn, now).await.unwrap(),
            vec![(t.id, folder_id)]
        );
        assert_eq!(get_model(&db.conn, t.id).await.unwrap().verdict, None);

        // The auto-process arm holds the same invariant.
        assert!(cancel(&db.conn, t.id, None).await.unwrap());
        assert!(requeue_canceled(&db.conn, t.id, None, &[], false).await.unwrap());
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "c2").await.unwrap());
        assert!(set_verdict(&db.conn, t.id, seq, "blocked", None).await.unwrap());
        assert!(cancel(&db.conn, t.id, None).await.unwrap());
        assert!(requeue_canceled(&db.conn, t.id, None, &[], false).await.unwrap());
        assert_eq!(
            auto_claim_next(&db.conn, folder_id, 0).await.unwrap(),
            Some(t.id)
        );
        assert_eq!(get_model(&db.conn, t.id).await.unwrap().verdict, None);
    }

    #[tokio::test]
    async fn a_planned_task_is_skipped_by_bulk_starts_and_freed_by_an_explicit_one() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-schedule-bulk").await;
        let planned = create(&db.conn, draft(folder_id, "planned")).await.unwrap();
        let plain = create(&db.conn, draft(folder_id, "plain")).await.unwrap();
        assert!(
            set_schedule(&db.conn, planned.id, Some(Utc::now() + chrono::Duration::hours(3)))
                .await
                .unwrap()
        );

        // "Start all" and the auto-process arm both leave the plan alone —
        // `planned` is the board head, so this also proves the head lookup
        // skips it rather than stopping there.
        assert_eq!(list_todo_ids(&db.conn, folder_id).await.unwrap(), vec![plain.id]);
        assert_eq!(
            auto_claim_next(&db.conn, folder_id, 0).await.unwrap(),
            Some(plain.id)
        );
        assert_eq!(auto_claim_next(&db.conn, folder_id, 0).await.unwrap(), None);
        assert_eq!(
            get(&db.conn, planned.id).await.unwrap().status,
            WorkTaskStatus::Todo
        );

        // "Start all" claims through the unplanned-only CAS, so a plan set
        // between its list query and its claim is still honoured — the list is
        // only a snapshot, and a bulk button must not override an individual
        // plan just because it read the row a moment earlier.
        assert_eq!(
            claim_unplanned_for_run(&db.conn, planned.id, WorkTaskStatus::Todo, "user")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            get(&db.conn, planned.id).await.unwrap().status,
            WorkTaskStatus::Todo
        );

        // The task's own start button overrides the plan and consumes it.
        assert!(claim_for_run(&db.conn, planned.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .is_some());
        assert!(get(&db.conn, planned.id).await.unwrap().scheduled_at.is_none());
    }

    #[tokio::test]
    async fn agent_verdict_is_generation_guarded_and_cleared_on_claim() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-verdict").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "c1").await.unwrap());

        // Wrong generation → rejected; current one records verdict + summary
        // (+ the agent_verdict event).
        assert!(!set_verdict(&db.conn, t.id, seq + 1, "success", None).await.unwrap());
        assert!(
            set_verdict(&db.conn, t.id, seq, "needs_review", Some("check the tests"))
                .await
                .unwrap()
        );
        let m = get_model(&db.conn, t.id).await.unwrap();
        assert_eq!(m.verdict.as_deref(), Some("needs_review"));
        assert_eq!(m.result_summary.as_deref(), Some("check the tests"));
        let events = list_events(&db.conn, t.id, 20).await.unwrap();
        assert!(events.iter().any(|e| e.kind == "agent_verdict"));

        // Settled tasks reject further reports; the next claim (a return from
        // review) invalidates the stale verdict.
        assert!(settle_review(&db.conn, t.id, seq, None, None).await.unwrap());
        assert!(!set_verdict(&db.conn, t.id, seq, "success", None).await.unwrap());
        claim_for_run(&db.conn, t.id, WorkTaskStatus::Review, "user")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_model(&db.conn, t.id).await.unwrap().verdict, None);
    }

    #[tokio::test]
    async fn stale_generation_events_are_no_ops() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-stale").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "conn-1").await.unwrap());

        // User cancels; a late TurnComplete for the old generation must be a
        // zero-side-effect no-op (the cancel-late-TurnComplete race).
        assert!(cancel(&db.conn, t.id, None).await.unwrap());
        assert!(!settle_review(&db.conn, t.id, seq, None, None).await.unwrap());
        assert!(!flip_awaiting(&db.conn, t.id, seq, true).await.unwrap());
        assert!(!fail(
            &db.conn,
            t.id,
            &[WorkTaskStatus::Running],
            Some(seq),
            "agent_error",
            None
        )
        .await
        .unwrap());
        assert_eq!(
            get(&db.conn, t.id).await.unwrap().status,
            WorkTaskStatus::Canceled
        );

        // Requeue resurrects it; the next claim bumps the generation.
        assert!(requeue_canceled(&db.conn, t.id, None, &[], false).await.unwrap());
        let seq2 = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seq2, seq + 1);
    }

    /// The reason the user gave when stopping a task rides the `canceled`
    /// status event, which is what the drawer's timeline renders under the
    /// phase header. Blank input must leave the payload alone — an empty
    /// `reason` key would render as a stray empty line.
    #[tokio::test]
    async fn a_cancel_reason_rides_the_status_event() {
        async fn cancel_reason(conn: &DatabaseConnection, id: i32) -> Option<String> {
            list_events(conn, id, 100)
                .await
                .unwrap()
                .into_iter()
                .find(|e| {
                    e.kind == "status_changed"
                        && e.payload.as_ref().and_then(|p| p.get("to")).and_then(|v| v.as_str())
                            == Some("canceled")
                })
                .and_then(|e| e.payload)
                .and_then(|p| p.get("reason").and_then(|v| v.as_str()).map(str::to_string))
        }

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-cancel-reason").await;

        let told = create(&db.conn, draft(folder_id, "told")).await.unwrap();
        assert!(cancel(&db.conn, told.id, Some("  wrong approach  ")).await.unwrap());
        assert_eq!(
            cancel_reason(&db.conn, told.id).await.as_deref(),
            Some("wrong approach")
        );

        let blank = create(&db.conn, draft(folder_id, "blank")).await.unwrap();
        assert!(cancel(&db.conn, blank.id, Some("   ")).await.unwrap());
        assert_eq!(cancel_reason(&db.conn, blank.id).await, None);

        let silent = create(&db.conn, draft(folder_id, "silent")).await.unwrap();
        assert!(cancel(&db.conn, silent.id, None).await.unwrap());
        assert_eq!(cancel_reason(&db.conn, silent.id).await, None);
    }

    #[tokio::test]
    async fn merge_lifecycle_is_cas_guarded_and_done_never_rolls_back() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-merge").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "conn-1").await.unwrap());
        assert!(settle_review(&db.conn, t.id, seq, Some("done".into()), Some((2, 10, 3)))
            .await
            .unwrap());

        let state = WorkTaskMergeState {
            pre_merge_head: "abc123".into(),
            message: "feat: t".into(),
            strategy: "squash".into(),
            delete_worktree: true,
            auto_message: false,
            ..Default::default()
        };
        // The merge is a fresh agent generation: begin bumps run_seq and
        // clears the run-scoped fields.
        let merge_seq = begin_merge(&db.conn, t.id, &state, seq, false, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(merge_seq, seq + 1);
        let row = get_model(&db.conn, t.id).await.unwrap();
        assert!(row.connection_id.is_none());
        assert!(row.verdict.is_none());
        // Double begin loses (already merging) — merge idempotency.
        assert!(begin_merge(&db.conn, t.id, &state, seq, false, None)
            .await
            .unwrap()
            .is_none());
        // Cancel is refused while merging.
        assert!(!cancel(&db.conn, t.id, None).await.unwrap());

        assert!(merge_landed(&db.conn, t.id, "def456").await.unwrap());
        // A second landing (event vs recovery race) is a no-op.
        assert!(!merge_landed(&db.conn, t.id, "zzz").await.unwrap());
        // And nothing can pull a done task back to review.
        assert!(!merge_back_to_review(&db.conn, t.id, None, None, None).await.unwrap());

        let got = get(&db.conn, t.id).await.unwrap();
        assert_eq!(got.status, WorkTaskStatus::Done);
        assert_eq!(got.merge_commit.as_deref(), Some("def456"));

        // Cleanup failure keeps done, only flags cleanup_state.
        set_cleanup_state(&db.conn, t.id, true, Some("worktree remove failed".into()))
            .await
            .unwrap();
        let got = get(&db.conn, t.id).await.unwrap();
        assert_eq!(got.status, WorkTaskStatus::Done);
        assert_eq!(got.cleanup_state.as_deref(), Some("failed"));
    }

    /// `done` no longer means one thing, so every writer of it has to say
    /// which ending this was — a NULL `completion_kind` on a fresh row would
    /// make the board guess again.
    #[tokio::test]
    async fn each_ending_records_how_the_task_finished() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-endings").await;

        let landed = reviewed(&db, folder_id, "merged one").await;
        let state = WorkTaskMergeState {
            pre_merge_head: "abc".into(),
            strategy: "squash".into(),
            ..Default::default()
        };
        begin_merge(&db.conn, landed.0, &state, landed.1, false, None)
            .await
            .unwrap()
            .unwrap();
        assert!(merge_landed(&db.conn, landed.0, "def456").await.unwrap());
        assert_eq!(
            get(&db.conn, landed.0).await.unwrap().completion_kind.as_deref(),
            Some(COMPLETION_MERGED)
        );

        let accepted = reviewed(&db, folder_id, "nothing to land").await;
        assert!(complete_without_merge(&db.conn, accepted.0, "no changes")
            .await
            .unwrap());
        assert_eq!(
            get(&db.conn, accepted.0).await.unwrap().completion_kind.as_deref(),
            Some(COMPLETION_ACCEPTED_WITHOUT_MERGE)
        );
    }

    /// A delivery's whole lifecycle at the service layer: the CAS that starts
    /// it, what it keeps and clears, and the settle that records the pull
    /// request on the row the issue list reads back.
    #[tokio::test]
    async fn a_delivery_starts_settles_and_binds_to_its_own_generation() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-deliver").await;
        let (id, seq) = reviewed(&db, folder_id, "deliver me").await;

        let state = WorkTaskMergeState {
            op: WorkTaskMergeOp::DeliverPr,
            remote_branch: Some("task/1".into()),
            expected_head: Some("abc123".into()),
            ..Default::default()
        };
        let deliver_seq = begin_delivery(&db.conn, id, &state, seq)
            .await
            .unwrap()
            .expect("CAS");
        assert_eq!(deliver_seq, seq + 1, "a delivery is its own generation");
        let row = get_model(&db.conn, id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Merging);
        assert!(row.connection_id.is_none(), "recovery must not see a live session");
        assert_eq!(
            row.verdict.as_deref(),
            Some("success"),
            "no new agent runs, so the review badge stays"
        );
        // Cancel is refused while merging — a delivery inherits that.
        assert!(!cancel(&db.conn, id, None).await.unwrap());
        // Double begin loses: the row is no longer in review.
        assert!(begin_delivery(&db.conn, id, &state, seq).await.unwrap().is_none());

        let meta = r#"{"provider":"github","result_pr":"https://x/pull/9"}"#;
        // A settle bound to the WRONG generation must miss, or a recovery pass
        // racing a live delivery could settle a run that already moved on.
        assert!(!complete_delivered(&db.conn, id, seq, "https://x/pull/9", meta)
            .await
            .unwrap());
        assert!(complete_delivered(&db.conn, id, deliver_seq, "https://x/pull/9", meta)
            .await
            .unwrap());

        let done = get_model(&db.conn, id).await.unwrap();
        assert_eq!(done.status, WorkTaskStatus::Done);
        assert_eq!(done.completion_kind.as_deref(), Some(COMPLETION_DELIVERED_PR));
        assert_eq!(done.source_meta.as_deref(), Some(meta));
        assert!(done.merge_state.is_none(), "the intent is spent");
        assert!(done.finished_at.is_some());
        // A second settle (event vs recovery race) is a no-op, and nothing
        // pulls a delivered task back to review.
        assert!(!complete_delivered(&db.conn, id, deliver_seq, "https://x/pull/9", meta)
            .await
            .unwrap());
        assert!(!merge_back_to_review(&db.conn, id, None, None, None).await.unwrap());
    }

    /// Run a fresh task all the way to `review` — the state every acceptance
    /// path starts from.
    async fn reviewed(
        db: &crate::db::AppDatabase,
        folder_id: i32,
        title: &str,
    ) -> (i32, i32) {
        let t = create(&db.conn, draft(folder_id, title)).await.unwrap();
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "c").await.unwrap());
        // Stamped while the run is still live (that is the only window
        // `set_verdict` accepts), so the review badge is present for the
        // acceptance paths to preserve or clear.
        assert!(set_verdict(&db.conn, t.id, seq, "success", Some("done"))
            .await
            .unwrap());
        assert!(settle_review(&db.conn, t.id, seq, None, None).await.unwrap());
        (t.id, seq)
    }

    #[tokio::test]
    async fn merge_conflict_returns_to_review() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-conflict").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "c").await.unwrap());
        assert!(settle_review(&db.conn, t.id, seq, None, None).await.unwrap());
        let state = WorkTaskMergeState {
            pre_merge_head: "abc".into(),
            message: "m".into(),
            strategy: "squash".into(),
            delete_worktree: false,
            auto_message: false,
            ..Default::default()
        };
        assert!(begin_merge(&db.conn, t.id, &state, seq, false, None)
            .await
            .unwrap()
            .is_some());
        assert!(merge_back_to_review(&db.conn, t.id, None, Some("conflict".into()),
            Some(vec!["a.rs".into()])
        )
        .await
        .unwrap());
        let got = get(&db.conn, t.id).await.unwrap();
        assert_eq!(got.status, WorkTaskStatus::Review);
        assert_eq!(got.last_error.as_deref(), Some("conflict"));
        let events = list_events(&db.conn, t.id, 100).await.unwrap();
        assert!(events.iter().any(|e| e.kind == "merge_conflict"));
    }

    /// The auto-merge bookkeeping: a refused dispatch leaves its banner on the
    /// review row (generation-guarded, and the stop that keeps auto-merge from
    /// retrying a hopeless dispatch forever), and a dispatched auto merge
    /// records "auto" as the actor who pulled the trigger.
    #[tokio::test]
    async fn auto_merge_marks_actor_and_banners_refused_dispatches() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-auto").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        // Not in review yet — nothing to banner.
        assert!(!set_review_error(&db.conn, t.id, 0, "early").await.unwrap());

        let seq = to_review(&db, t.id).await;
        // Stale generation writes nothing; the current one lands on the row.
        assert!(!set_review_error(&db.conn, t.id, seq + 1, "stale").await.unwrap());
        assert!(set_review_error(&db.conn, t.id, seq, "wrong branch").await.unwrap());
        assert_eq!(
            get(&db.conn, t.id).await.unwrap().last_error.as_deref(),
            Some("wrong branch")
        );

        let state = WorkTaskMergeState {
            pre_merge_head: "abc".into(),
            message: String::new(),
            strategy: "squash".into(),
            delete_worktree: true,
            auto_message: true,
            ..Default::default()
        };
        // An unattended dispatch never clears a banner — the failed row waits
        // for a human (the no-auto-retry latch) …
        assert!(begin_merge(&db.conn, t.id, &state, seq, true, None)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            get(&db.conn, t.id).await.unwrap().last_error.as_deref(),
            Some("wrong branch")
        );
        // … while a user dispatch is exactly that human: retry allowed, and
        // the fresh merge starts with a fresh slate.
        assert!(begin_merge(&db.conn, t.id, &state, seq, false, None)
            .await
            .unwrap()
            .is_some());
        assert!(get(&db.conn, t.id).await.unwrap().last_error.is_none());

        // Back in clean review for the unattended path proper.
        assert!(merge_back_to_review(&db.conn, t.id, None, None, None).await.unwrap());
        assert!(begin_merge(&db.conn, t.id, &state, seq + 1, true, None)
            .await
            .unwrap()
            .is_some());
        // The timeline knows the engine, not the user, started this one.
        let events = list_events(&db.conn, t.id, 100).await.unwrap();
        let auto_attempt = events
            .iter()
            .find(|e| e.kind == "merge_attempt" && e.actor == "auto")
            .expect("auto merge attempt event");
        assert_eq!(
            auto_attempt
                .payload
                .as_ref()
                .and_then(|p| p.get("auto"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(events.iter().any(|e| {
            e.kind == "status_changed"
                && e.payload.as_ref().and_then(|p| p.get("to")).and_then(|v| v.as_str())
                    == Some("merging")
                && e.payload
                    .as_ref()
                    .and_then(|p| p.get("reason"))
                    .and_then(|v| v.as_str())
                    == Some("started by auto-merge")
        }));
        // A banner cannot land on a row that already left review.
        assert!(!set_review_error(&db.conn, t.id, seq, "late").await.unwrap());
    }

    /// The lock-wait race of the no-auto-retry invariant: two unattended
    /// sweeps pick the same review generation, the first dispatch fails after
    /// bumping it, and the second — queued on the folder lock with the
    /// pre-failure snapshot — must not redispatch. Its stale generation
    /// misses the CAS, and even a fresh re-listing is stopped by the banner;
    /// only a user dispatch reopens the row.
    #[tokio::test]
    async fn auto_merge_never_retries_a_failed_generation() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-auto-retry").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = to_review(&db, t.id).await;
        let state = WorkTaskMergeState {
            pre_merge_head: "abc".into(),
            message: String::new(),
            strategy: "squash".into(),
            delete_worktree: true,
            auto_message: true,
            ..Default::default()
        };

        // Sweep A dispatches generation `seq` and its launch fails: back to
        // review with the banner on, generation now `seq + 1`.
        assert!(begin_merge(&db.conn, t.id, &state, seq, true, None)
            .await
            .unwrap()
            .is_some());
        assert!(
            merge_back_to_review(&db.conn, t.id, None, Some("launch failed".into()), None)
                .await
                .unwrap()
        );

        // Sweep B, dispatched against the pre-failure snapshot: stale
        // generation, CAS miss.
        assert!(begin_merge(&db.conn, t.id, &state, seq, true, None)
            .await
            .unwrap()
            .is_none());
        // A later sweep re-lists and sees the current generation — the banner
        // still blocks any unattended dispatch.
        assert!(begin_merge(&db.conn, t.id, &state, seq + 1, true, None)
            .await
            .unwrap()
            .is_none());
        let row = get_model(&db.conn, t.id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Review);
        assert_eq!(row.last_error.as_deref(), Some("launch failed"));
        // Exactly one merge attempt made the timeline.
        let events = list_events(&db.conn, t.id, 100).await.unwrap();
        assert_eq!(
            events.iter().filter(|e| e.kind == "merge_attempt").count(),
            1
        );

        // The human path stays open: a user dispatch on the current
        // generation retries and clears the banner.
        assert!(begin_merge(&db.conn, t.id, &state, seq + 1, false, None)
            .await
            .unwrap()
            .is_some());
        assert!(get(&db.conn, t.id).await.unwrap().last_error.is_none());
    }

    /// The parked column is the ONLY carrier of the user's extra merge
    /// instructions between the dialog and the pump's later dispatch — a drop
    /// anywhere along park → column → parse → wire loses what they asked for
    /// with no trace on the card or the timeline.
    #[tokio::test]
    async fn a_queued_merge_carries_the_users_extra_instructions() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-merge-extra").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = to_review(&db, t.id).await;

        let intent = WorkTaskQueuedMerge {
            message: None,
            delete_worktree: true,
            instructions: Some("prefer ours on conflict".into()),
            queued_at: Utc::now(),
        };
        assert!(queue_merge(&db.conn, t.id, &intent, seq, None).await.unwrap());

        let raw = get_model(&db.conn, t.id)
            .await
            .unwrap()
            .pending_merge
            .expect("parked");
        assert_eq!(
            queued_merge(Some(raw.as_str()))
                .expect("parses")
                .instructions
                .as_deref(),
            Some("prefer ours on conflict"),
            "this is what the pump replays into the merge generation"
        );
        assert_eq!(
            get(&db.conn, t.id)
                .await
                .unwrap()
                .merge_queued
                .expect("on the wire")
                .instructions
                .as_deref(),
            Some("prefer ours on conflict"),
            "reopening the dialog has to show what is parked"
        );

        // An intent WITHOUT them serializes exactly as it did before the field
        // existed. The queue CASes on this column's RAW text, so a key that
        // appeared out of nowhere would make every merge parked before the
        // upgrade miss its own claim.
        let bare = WorkTaskQueuedMerge {
            instructions: None,
            ..intent
        };
        assert!(!serde_json::to_string(&bare).unwrap().contains("instructions"));
        // …and the mirror: a row parked before the upgrade still parses.
        assert!(queued_merge(Some(
            r#"{"message":null,"delete_worktree":true,"queued_at":"2026-08-01T00:30:00Z"}"#
        ))
        .expect("legacy intent parses")
        .instructions
        .is_none());
    }

    /// The merge queue's row-level contract: an intent only lands on the exact
    /// review generation the caller validated, it survives on the row until
    /// something spends it, and a dispatch (from the pump or from a click that
    /// found the slot free) consumes it.
    #[tokio::test]
    async fn a_queued_merge_is_generation_bound_and_spent_by_the_dispatch() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-merge-queue").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        let intent = WorkTaskQueuedMerge {
            message: Some("feat: land it".into()),
            delete_worktree: true,
            instructions: None,
            queued_at: Utc::now(),
        };
        // Not in review yet — nothing to queue on.
        assert!(!queue_merge(&db.conn, t.id, &intent, 0, None).await.unwrap());

        let seq = to_review(&db, t.id).await;
        // A banner from an earlier refusal is what the user is answering by
        // clicking merge again; queuing clears it.
        assert!(set_review_error(&db.conn, t.id, seq, "wrong branch").await.unwrap());
        // A stale generation misses, the current one lands.
        assert!(!queue_merge(&db.conn, t.id, &intent, seq + 1, None).await.unwrap());
        assert!(queue_merge(&db.conn, t.id, &intent, seq, None).await.unwrap());
        let row = get_model(&db.conn, t.id).await.unwrap();
        assert!(row.last_error.is_none());
        assert_eq!(row.status, WorkTaskStatus::Review);
        let parked = queued_merge(row.pending_merge.as_deref()).expect("intent parked");
        assert_eq!(parked.message.as_deref(), Some("feat: land it"));
        assert!(parked.delete_worktree);
        let on_wire = get(&db.conn, t.id).await.unwrap().merge_queued.expect("on the wire");
        assert_eq!(on_wire.queued_at, parked.queued_at);
        // The options ride along, so reopening the dialog can show what is
        // parked instead of quietly replacing it with the defaults.
        assert_eq!(on_wire.message.as_deref(), Some("feat: land it"));
        assert!(on_wire.delete_worktree);

        // Re-queuing with new options is an edit of the same place in line —
        // the engine passes the original instant back in.
        let edited = WorkTaskQueuedMerge {
            message: None,
            delete_worktree: false,
            instructions: None,
            queued_at: parked.queued_at,
        };
        assert!(queue_merge(&db.conn, t.id, &edited, seq, None).await.unwrap());
        let row = get_model(&db.conn, t.id).await.unwrap();
        let parked = queued_merge(row.pending_merge.as_deref()).unwrap();
        assert_eq!(parked.queued_at, edited.queued_at);
        assert!(parked.message.is_none());

        // The dispatch spends it: the row must not carry an intent the pump
        // could dispatch a second time.
        let state = WorkTaskMergeState {
            pre_merge_head: "abc".into(),
            message: String::new(),
            strategy: "squash".into(),
            delete_worktree: false,
            auto_message: true,
            ..Default::default()
        };
        assert!(begin_merge(&db.conn, t.id, &state, seq, false, None)
            .await
            .unwrap()
            .is_some());
        let row = get_model(&db.conn, t.id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Merging);
        assert!(row.pending_merge.is_none());
        assert!(get(&db.conn, t.id).await.unwrap().merge_queued.is_none());
        assert_eq!(
            list_events(&db.conn, t.id, 100)
                .await
                .unwrap()
                .iter()
                .filter(|e| e.kind == "merge_queued")
                .count(),
            2
        );
    }

    /// The pump scans the queue, then spends real time on it — a worktree
    /// stat, the folder git lock, three git subprocesses — before the CAS that
    /// spends the intent. A user can withdraw or edit the merge anywhere in
    /// that window WITHOUT moving `run_seq`, so the intent itself is the token:
    /// every write the pump makes demands the column still equal what it
    /// scanned. Without this, a withdrawn merge still landed on the base
    /// branch, and a refusal could delete an edit made a moment earlier.
    #[tokio::test]
    async fn a_queued_dispatch_loses_to_a_withdrawal_or_an_edit_under_it() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-merge-queue-race").await;
        let state = WorkTaskMergeState {
            pre_merge_head: "abc".into(),
            message: String::new(),
            strategy: "squash".into(),
            delete_worktree: true,
            auto_message: true,
            ..Default::default()
        };
        let intent = |secs: i64| WorkTaskQueuedMerge {
            message: Some(format!("feat: land it {secs}")),
            delete_worktree: true,
            instructions: None,
            queued_at: chrono::DateTime::from_timestamp(1_800_000_000 + secs, 0)
                .expect("valid instant"),
        };
        /// What the pump scanned: the row's raw column value.
        async fn scanned(db: &crate::db::AppDatabase, id: i32) -> String {
            get_model(&db.conn, id)
                .await
                .unwrap()
                .pending_merge
                .expect("intent parked")
        }

        // Withdrawn under the pump: the dispatch must miss, and the task must
        // stay in review rather than land a merge nobody wants anymore.
        let withdrawn = create(&db.conn, draft(folder_id, "withdrawn")).await.unwrap();
        let seq = to_review(&db, withdrawn.id).await;
        assert!(queue_merge(&db.conn, withdrawn.id, &intent(1), seq, None)
            .await
            .unwrap());
        let claim = scanned(&db, withdrawn.id).await;
        assert!(unqueue_merge(&db.conn, withdrawn.id).await.unwrap());
        assert!(
            begin_merge(&db.conn, withdrawn.id, &state, seq, false, Some(&claim))
                .await
                .unwrap()
                .is_none(),
            "a withdrawn merge must not dispatch"
        );
        assert_eq!(
            get_model(&db.conn, withdrawn.id).await.unwrap().status,
            WorkTaskStatus::Review
        );

        // Edited under the pump: same miss, and the edit survives untouched —
        // the next pump picks it up.
        let edited = create(&db.conn, draft(folder_id, "edited")).await.unwrap();
        let seq = to_review(&db, edited.id).await;
        assert!(queue_merge(&db.conn, edited.id, &intent(1), seq, None)
            .await
            .unwrap());
        let claim = scanned(&db, edited.id).await;
        assert!(queue_merge(&db.conn, edited.id, &intent(2), seq, None)
            .await
            .unwrap());
        assert!(
            begin_merge(&db.conn, edited.id, &state, seq, false, Some(&claim))
                .await
                .unwrap()
                .is_none(),
            "the superseded intent must not dispatch"
        );
        // A re-park of the stale intent (the slot got taken first) must not
        // revert the edit either …
        assert!(
            !queue_merge(&db.conn, edited.id, &intent(1), seq, Some(&claim))
                .await
                .unwrap(),
            "the pump must not re-park an intent the user replaced"
        );
        // … and the refusal path must not sweep up the newer request.
        assert!(!clear_queued_merge(&db.conn, edited.id, &claim).await.unwrap());
        let parked = queued_merge(
            get_model(&db.conn, edited.id)
                .await
                .unwrap()
                .pending_merge
                .as_deref(),
        )
        .expect("the edit survives");
        assert_eq!(parked.message.as_deref(), Some("feat: land it 2"));

        // The auto sweep decides its eligibility from a snapshot taken before
        // the folder lock, so the CAS has to hold the line: a merge queued
        // while the sweep worked must not be landed — and silently cleared —
        // with the unattended defaults.
        let raced = create(&db.conn, draft(folder_id, "raced")).await.unwrap();
        let seq = to_review(&db, raced.id).await;
        assert!(queue_merge(&db.conn, raced.id, &intent(4), seq, None)
            .await
            .unwrap());
        assert!(
            begin_merge(&db.conn, raced.id, &state, seq, true, None)
                .await
                .unwrap()
                .is_none(),
            "an unattended dispatch must not take a row the user queued"
        );
        let row = get_model(&db.conn, raced.id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Review);
        assert!(row.pending_merge.is_some(), "the user's intent survives");
        // The queue's own dispatch still lands it, with the user's options.
        let claim = scanned(&db, raced.id).await;
        assert!(
            begin_merge(&db.conn, raced.id, &state, seq, false, Some(&claim))
                .await
                .unwrap()
                .is_some()
        );

        // The uncontested path still works: the intent the pump scanned is the
        // one on the row, so it dispatches.
        let clean = create(&db.conn, draft(folder_id, "clean")).await.unwrap();
        let seq = to_review(&db, clean.id).await;
        assert!(queue_merge(&db.conn, clean.id, &intent(3), seq, None)
            .await
            .unwrap());
        let claim = scanned(&db, clean.id).await;
        assert!(begin_merge(&db.conn, clean.id, &state, seq, false, Some(&claim))
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            get_model(&db.conn, clean.id).await.unwrap().status,
            WorkTaskStatus::Merging
        );
    }

    /// Withdrawing a queued merge: only from review, only when something is
    /// actually queued (so a withdrawal that lost the race to the pump reports
    /// the miss), and the engine's silent variant leaves no timeline entry.
    #[tokio::test]
    async fn unqueue_merge_is_guarded_and_the_silent_clear_is_not() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-merge-unqueue").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = to_review(&db, t.id).await;

        // Nothing queued yet.
        assert!(!unqueue_merge(&db.conn, t.id).await.unwrap());

        let intent = WorkTaskQueuedMerge {
            message: None,
            delete_worktree: true,
            instructions: None,
            queued_at: Utc::now(),
        };
        assert!(queue_merge(&db.conn, t.id, &intent, seq, None).await.unwrap());
        assert!(unqueue_merge(&db.conn, t.id).await.unwrap());
        assert!(get_model(&db.conn, t.id)
            .await
            .unwrap()
            .pending_merge
            .is_none());
        // Twice is a miss, not a second withdrawal.
        assert!(!unqueue_merge(&db.conn, t.id).await.unwrap());
        assert_eq!(
            list_events(&db.conn, t.id, 100)
                .await
                .unwrap()
                .iter()
                .filter(|e| e.kind == "user_action"
                    && e.payload
                        .as_ref()
                        .and_then(|p| p.get("action"))
                        .and_then(|v| v.as_str())
                        == Some("unqueue_merge"))
                .count(),
            1
        );

        // The engine's refusal path clears without a timeline entry — the
        // reason it writes on the row is what the user reads — and only when
        // the row still holds the exact intent being refused.
        assert!(queue_merge(&db.conn, t.id, &intent, seq, None).await.unwrap());
        let parked = get_model(&db.conn, t.id)
            .await
            .unwrap()
            .pending_merge
            .expect("intent parked");
        assert!(!clear_queued_merge(&db.conn, t.id, "{\"stale\":true}")
            .await
            .unwrap());
        assert!(get_model(&db.conn, t.id)
            .await
            .unwrap()
            .pending_merge
            .is_some());
        assert!(clear_queued_merge(&db.conn, t.id, &parked).await.unwrap());
        assert!(get_model(&db.conn, t.id)
            .await
            .unwrap()
            .pending_merge
            .is_none());
        assert_eq!(
            list_events(&db.conn, t.id, 100)
                .await
                .unwrap()
                .iter()
                .filter(|e| e.kind == "user_action"
                    && e.payload
                        .as_ref()
                        .and_then(|p| p.get("action"))
                        .and_then(|v| v.as_str())
                        == Some("unqueue_merge"))
                .count(),
            1
        );
    }

    /// A task that leaves review by any door drops its place in the merge
    /// queue with it — a follow-up, a stop, or an outright completion must not
    /// leave an intent the pump would later dispatch.
    #[tokio::test]
    async fn leaving_review_drops_the_queued_merge() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-merge-queue-exit").await;
        let intent = WorkTaskQueuedMerge {
            message: None,
            delete_worktree: true,
            instructions: None,
            queued_at: Utc::now(),
        };

        // Follow-up (claim for a new run).
        let followed = create(&db.conn, draft(folder_id, "followed")).await.unwrap();
        let seq = to_review(&db, followed.id).await;
        assert!(queue_merge(&db.conn, followed.id, &intent, seq, None).await.unwrap());
        assert!(claim_for_run(&db.conn, followed.id, WorkTaskStatus::Review, "user")
            .await
            .unwrap()
            .is_some());
        assert!(get_model(&db.conn, followed.id)
            .await
            .unwrap()
            .pending_merge
            .is_none());

        // Stopped.
        let stopped = create(&db.conn, draft(folder_id, "stopped")).await.unwrap();
        let seq = to_review(&db, stopped.id).await;
        assert!(queue_merge(&db.conn, stopped.id, &intent, seq, None).await.unwrap());
        assert!(cancel(&db.conn, stopped.id, None).await.unwrap());
        assert!(get_model(&db.conn, stopped.id)
            .await
            .unwrap()
            .pending_merge
            .is_none());

        // Accepted with nothing to land.
        let finished = create(&db.conn, draft(folder_id, "finished")).await.unwrap();
        let seq = to_review(&db, finished.id).await;
        assert!(queue_merge(&db.conn, finished.id, &intent, seq, None).await.unwrap());
        assert!(complete_without_merge(&db.conn, finished.id, "nothing to merge")
            .await
            .unwrap());
        assert!(get_model(&db.conn, finished.id)
            .await
            .unwrap()
            .pending_merge
            .is_none());
    }

    /// A task that changed nothing finishes without a merge commit — from
    /// review only, and once.
    #[tokio::test]
    async fn complete_without_merge_lands_done_from_review_only() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-complete").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        // A todo task is not up for acceptance.
        assert!(!complete_without_merge(&db.conn, t.id, "no changes")
            .await
            .unwrap());

        let seq = to_review(&db, t.id).await;
        // A refused merge leaves its banner on the review row; finishing the
        // task on purpose must not carry that error into Done.
        let state = WorkTaskMergeState {
            pre_merge_head: "abc".into(),
            message: "m".into(),
            strategy: "squash".into(),
            delete_worktree: false,
            auto_message: false,
            ..Default::default()
        };
        assert!(begin_merge(&db.conn, t.id, &state, seq, false, None)
            .await
            .unwrap()
            .is_some());
        assert!(merge_back_to_review(&db.conn, t.id, None, Some("nope".into()), None)
            .await
            .unwrap());
        assert_eq!(
            get(&db.conn, t.id).await.unwrap().last_error.as_deref(),
            Some("nope")
        );

        assert!(complete_without_merge(&db.conn, t.id, "no changes")
            .await
            .unwrap());
        let got = get(&db.conn, t.id).await.unwrap();
        assert_eq!(got.status, WorkTaskStatus::Done);
        // Nothing was merged, so nothing points at a merge commit.
        assert!(got.merge_commit.is_none());
        assert!(got.finished_at.is_some());
        assert!(got.last_error.is_none());

        // Terminal: a second acceptance and a late merge settle are no-ops.
        assert!(!complete_without_merge(&db.conn, t.id, "no changes")
            .await
            .unwrap());
        assert!(!merge_landed(&db.conn, t.id, "abc").await.unwrap());
        assert_eq!(
            get(&db.conn, t.id).await.unwrap().status,
            WorkTaskStatus::Done
        );

        let events = list_events(&db.conn, t.id, 100).await.unwrap();
        let settle = events
            .iter()
            .rfind(|e| e.kind == "status_changed")
            .expect("status change");
        assert_eq!(
            settle.payload.as_ref().and_then(|p| p.get("to")).and_then(|v| v.as_str()),
            Some("done")
        );
        // The caller's reason is what the timeline shows under the header.
        assert_eq!(
            settle
                .payload
                .as_ref()
                .and_then(|p| p.get("reason"))
                .and_then(|v| v.as_str()),
            Some("no changes")
        );
    }

    #[tokio::test]
    async fn boot_reconcile_interrupts_active_but_not_merging() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-boot").await;

        let queued = create(&db.conn, draft(folder_id, "q")).await.unwrap();
        claim_for_run(&db.conn, queued.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap();

        let running = create(&db.conn, draft(folder_id, "r")).await.unwrap();
        let seq = claim_for_run(&db.conn, running.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        start_running(&db.conn, running.id, seq, 1, "c").await.unwrap();

        let merging = create(&db.conn, draft(folder_id, "m")).await.unwrap();
        let seq = claim_for_run(&db.conn, merging.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        start_running(&db.conn, merging.id, seq, 2, "c2").await.unwrap();
        settle_review(&db.conn, merging.id, seq, None, None).await.unwrap();
        begin_merge(
            &db.conn,
            merging.id,
            &WorkTaskMergeState {
                pre_merge_head: "abc".into(),
                message: "m".into(),
                strategy: "squash".into(),
                delete_worktree: false,
                auto_message: false,
                ..Default::default()
            },
            seq,
            false,
            None,
        )
        .await
        .unwrap();

        assert_eq!(boot_reconcile_interrupted(&db.conn).await.unwrap(), 2);
        // Idempotent: a second sweep finds nothing.
        assert_eq!(boot_reconcile_interrupted(&db.conn).await.unwrap(), 0);

        let q = get(&db.conn, queued.id).await.unwrap();
        assert_eq!(q.status, WorkTaskStatus::Failed);
        assert_eq!(q.failure_reason.as_deref(), Some("interrupted"));
        // The interrupted queued task retries idempotently: failed → queued.
        assert!(claim_for_run(&db.conn, queued.id, WorkTaskStatus::Failed, "user")
            .await
            .unwrap()
            .is_some());

        // Merging is exempt — its recovery goes through git truth.
        assert_eq!(
            get(&db.conn, merging.id).await.unwrap().status,
            WorkTaskStatus::Merging
        );
    }

    #[tokio::test]
    async fn counts_and_settings_roundtrip() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-counts").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        assert_eq!(attention_count(&db.conn).await.unwrap(), 0);
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        start_running(&db.conn, t.id, seq, 1, "c").await.unwrap();
        assert_eq!(active_launched_count(&db.conn, folder_id).await.unwrap(), 1);
        settle_review(&db.conn, t.id, seq, None, None).await.unwrap();
        assert_eq!(attention_count(&db.conn).await.unwrap(), 1);
        assert_eq!(active_launched_count(&db.conn, folder_id).await.unwrap(), 0);

        // Settings default → save → read back.
        let s = settings_get(&db.conn, folder_id).await.unwrap();
        assert_eq!(s.max_concurrent, 2);
        assert!(s.delete_worktree_default);
        assert!(!s.auto_process);
        assert!(s.stage_prompts.is_empty());
        let mut s2 = s;
        s2.max_concurrent = 0;
        s2.merge_strategy = "merge".into();
        s2.stage_prompts
            .insert("merge".into(), "Write the message in Chinese.".into());
        settings_set(&db.conn, folder_id, &s2).await.unwrap();
        let s3 = settings_get(&db.conn, folder_id).await.unwrap();
        assert_eq!(s3.max_concurrent, 0);
        assert_eq!(s3.merge_strategy, "merge");
        assert_eq!(
            s3.stage_prompts.get("merge").map(String::as_str),
            Some("Write the message in Chinese.")
        );
    }

    #[tokio::test]
    async fn effective_settings_fall_back_to_the_global_row_wholesale() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-global").await;

        // No rows anywhere → built-in defaults.
        let s = settings_get_effective(&db.conn, folder_id).await.unwrap();
        assert_eq!(s.max_concurrent, 2);

        // A global row applies to folders without their own settings.
        let global = WorkTaskFolderSettings {
            max_concurrent: 5,
            init_command: Some("pnpm install".into()),
            ..Default::default()
        };
        settings_set(&db.conn, GLOBAL_SETTINGS_FOLDER_ID, &global)
            .await
            .unwrap();
        let s = settings_get_effective(&db.conn, folder_id).await.unwrap();
        assert_eq!(s.max_concurrent, 5);
        assert_eq!(s.init_command.as_deref(), Some("pnpm install"));
        // The raw read is untouched by the fallback.
        assert_eq!(settings_get(&db.conn, folder_id).await.unwrap().max_concurrent, 2);

        // Saving the folder's own settings detaches it entirely — no
        // field-by-field merge.
        let own = WorkTaskFolderSettings {
            max_concurrent: 1,
            ..Default::default()
        };
        settings_set(&db.conn, folder_id, &own).await.unwrap();
        let s = settings_get_effective(&db.conn, folder_id).await.unwrap();
        assert_eq!(s.max_concurrent, 1);
        assert!(s.init_command.is_none());
    }

    #[tokio::test]
    async fn folders_with_todos_lists_distinct_live_folders() {
        let db = fresh_in_memory_db().await;
        let f1 = seed_folder(&db, "/tmp/wt-todos-1").await;
        let f2 = seed_folder(&db, "/tmp/wt-todos-2").await;
        create(&db.conn, draft(f1, "a")).await.unwrap();
        create(&db.conn, draft(f1, "b")).await.unwrap();
        let started = create(&db.conn, draft(f2, "c")).await.unwrap();
        assert_eq!(folders_with_todos(&db.conn).await.unwrap(), vec![f1, f2]);
        // A folder whose only task left todo drops out.
        claim_for_run(&db.conn, started.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap();
        assert_eq!(folders_with_todos(&db.conn).await.unwrap(), vec![f1]);
    }

    #[tokio::test]
    async fn delete_rules() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-del").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        // The guard is the point: a tombstone must not land on a row that was
        // claimed since the caller looked at it, or the run it just started
        // would outlive the row (worktree and agent process included).
        assert!(!soft_delete(&db.conn, t.id, WorkTaskStatus::Queued)
            .await
            .unwrap());
        assert_eq!(get(&db.conn, t.id).await.unwrap().status, WorkTaskStatus::Todo);

        assert!(soft_delete(&db.conn, t.id, WorkTaskStatus::Todo).await.unwrap());
        assert!(get(&db.conn, t.id).await.is_err());
        assert!(list(&db.conn, Some(folder_id)).await.unwrap().is_empty());
        // A second delete is a clean no-op rather than a second tombstone.
        assert!(!soft_delete(&db.conn, t.id, WorkTaskStatus::Todo).await.unwrap());
    }

    /// Drive a claimed (queued) task all the way to running, the way a launch
    /// does: out of the queue into setup, then bound to its connection.
    async fn start_running(
        conn: &DatabaseConnection,
        id: i32,
        run_seq: i32,
        conversation_id: i32,
        connection_id: &str,
    ) -> Result<bool, DbError> {
        assert!(begin_setup(conn, id, run_seq).await?);
        mark_running(conn, id, run_seq, conversation_id, connection_id).await
    }

    /// Drive a task to review and return its current run_seq.
    async fn to_review(db: &crate::db::AppDatabase, id: i32) -> i32 {
        let seq = claim_for_run(&db.conn, id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, id, seq, 1, "c").await.unwrap());
        assert!(settle_review(&db.conn, id, seq, None, None).await.unwrap());
        seq
    }

    #[tokio::test]
    async fn preflight_is_generation_guarded_and_reset_by_claim_and_settle() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-preflight").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();
        let seq = to_review(&db, t.id).await;

        let mut light = crate::models::WorkTaskPreflight {
            status: "running".into(),
            command: "tests".into(),
            exit_code: None,
            output_tail: None,
        };
        // Stale generation / wrong status writes are no-ops.
        assert!(!set_preflight(&db.conn, t.id, seq + 1, &light).await.unwrap());
        assert!(set_preflight(&db.conn, t.id, seq, &light).await.unwrap());
        light.status = "failed".into();
        light.exit_code = Some(2);
        light.output_tail = Some("boom".into());
        assert!(set_preflight(&db.conn, t.id, seq, &light).await.unwrap());
        let info = get(&db.conn, t.id).await.unwrap();
        assert_eq!(
            info.preflight.as_ref().and_then(|p| p.get("status")).and_then(|s| s.as_str()),
            Some("failed")
        );
        let events = list_events(&db.conn, t.id, 100).await.unwrap();
        assert!(events.iter().any(|e| e.kind == "preflight_result"));

        // A return claim wipes the stale light; the next settle starts clean
        // (and a slow old-generation finish can no longer write).
        let seq2 = claim_for_run(&db.conn, t.id, WorkTaskStatus::Review, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(get_model(&db.conn, t.id).await.unwrap().preflight.is_none());
        assert!(start_running(&db.conn, t.id, seq2, 1, "c2").await.unwrap());
        assert!(settle_review(&db.conn, t.id, seq2, None, None).await.unwrap());
        assert!(!set_preflight(&db.conn, t.id, seq, &light).await.unwrap());
        assert!(get_model(&db.conn, t.id).await.unwrap().preflight.is_none());
    }

    #[tokio::test]
    async fn archive_is_terminal_only_and_resurrection_unarchives() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-archive").await;
        let t = create(&db.conn, draft(folder_id, "t")).await.unwrap();

        // Active tasks cannot be archived.
        assert!(!set_archived(&db.conn, t.id, true).await.unwrap());

        // failed → archived leaves the attention badge…
        let seq = claim_for_run(&db.conn, t.id, WorkTaskStatus::Todo, "user")
            .await
            .unwrap()
            .unwrap();
        assert!(start_running(&db.conn, t.id, seq, 1, "c").await.unwrap());
        assert!(fail(
            &db.conn,
            t.id,
            &[WorkTaskStatus::Running],
            Some(seq),
            "agent_error",
            None
        )
        .await
        .unwrap());
        assert_eq!(attention_count(&db.conn).await.unwrap(), 1);
        assert!(set_archived(&db.conn, t.id, true).await.unwrap());
        assert!(!set_archived(&db.conn, t.id, true).await.unwrap()); // already archived
        assert_eq!(attention_count(&db.conn).await.unwrap(), 0);
        assert!(get(&db.conn, t.id).await.unwrap().archived_at.is_some());

        // …a retry claim resurrects it out of the archive…
        assert!(claim_for_run(&db.conn, t.id, WorkTaskStatus::Failed, "user")
            .await
            .unwrap()
            .is_some());
        assert!(get(&db.conn, t.id).await.unwrap().archived_at.is_none());

        // …and so does requeueing an archived canceled task.
        assert!(cancel(&db.conn, t.id, None).await.unwrap());
        assert!(set_archived(&db.conn, t.id, true).await.unwrap());
        assert!(requeue_canceled(&db.conn, t.id, None, &[], false).await.unwrap());
        let row = get(&db.conn, t.id).await.unwrap();
        assert_eq!(row.status, WorkTaskStatus::Todo);
        assert!(row.archived_at.is_none());

        // Explicit unarchive requires an archived row.
        assert!(!set_archived(&db.conn, t.id, false).await.unwrap());
    }

    #[tokio::test]
    async fn template_save_upserts_by_name() {
        let db = fresh_in_memory_db().await;
        let d = |name: &str, title: &str| crate::models::WorkTaskTemplateDraft {
            name: name.to_string(),
            title: title.to_string(),
            config: serde_json::json!({ "display_text": title, "prompt_blocks": [] }),
        };

        let a = template_save(&db.conn, &d("Release", "cut a release")).await.unwrap();
        template_save(&db.conn, &d("Audit", "audit deps")).await.unwrap();
        // Same name replaces in place instead of duplicating.
        let a2 = template_save(&db.conn, &d("Release", "cut a patch release"))
            .await
            .unwrap();
        assert_eq!(a2.id, a.id);

        let listed = template_list(&db.conn).await.unwrap();
        assert_eq!(
            listed.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["Audit", "Release"],
            "name-ordered"
        );
        assert_eq!(listed[1].title, "cut a patch release");

        assert!(template_save(&db.conn, &d("  ", "x")).await.is_err());
        template_delete(&db.conn, a.id).await.unwrap();
        assert_eq!(template_list(&db.conn).await.unwrap().len(), 1);
    }

    fn source(key: &str) -> crate::models::WorkTaskSource {
        crate::models::WorkTaskSource {
            kind: "forge_issue".to_string(),
            key: key.to_string(),
            meta: serde_json::json!({
                "provider": "github",
                "server_host": "github.com",
                "owner_repo": "acme/app",
                "number": 123,
                "url": "https://github.com/acme/app/issues/123",
                "title": "Login times out",
            }),
        }
    }

    /// The forge path is the only writer of the source columns; the public
    /// create leaves them NULL, and the dedup guard turns a second trigger
    /// into a `Duplicate` answer unless the user forces a second live task.
    #[tokio::test]
    async fn forge_create_writes_source_and_dedups_active() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-forge").await;
        let key = "github:github.com:acme/app:issue:123";

        let plain = create(&db.conn, draft(folder_id, "manual card")).await.unwrap();
        assert_eq!(plain.source_kind, None);
        assert_eq!(plain.source_key, None);

        let first = match create_from_forge(&db.conn, draft(folder_id, "#123 · fix"), source(key), false)
            .await
            .unwrap()
        {
            ForgeCreateOutcome::Created(t) => t,
            other => panic!("expected Created, got {other:?}"),
        };
        assert_eq!(first.source_kind.as_deref(), Some("forge_issue"));
        assert_eq!(first.source_key.as_deref(), Some(key));
        assert_eq!(
            first.source_meta.as_ref().and_then(|m| m["number"].as_i64()),
            Some(123)
        );
        // The provenance audit event landed in the create transaction.
        let events = list_events(&db.conn, first.id, 100).await.unwrap();
        assert!(events.iter().any(|e| e.kind == "forge_linked"));

        // Second trigger answers with the live task instead of a twin…
        match create_from_forge(&db.conn, draft(folder_id, "#123 · again"), source(key), false)
            .await
            .unwrap()
        {
            ForgeCreateOutcome::Duplicate(existing) => assert_eq!(existing.id, first.id),
            other => panic!("expected Duplicate, got {other:?}"),
        }
        // …unless the user explicitly forces a second one.
        let forced = match create_from_forge(&db.conn, draft(folder_id, "#123 · fork"), source(key), true)
            .await
            .unwrap()
        {
            ForgeCreateOutcome::Created(t) => t,
            other => panic!("expected forced Created, got {other:?}"),
        };
        assert_ne!(forced.id, first.id);

        // The resurrection guard sees the OTHER live task for the same key.
        let other = other_active_with_same_source(&db.conn, forced.id, key)
            .await
            .unwrap()
            .expect("first task is still active");
        assert_eq!(other.id, first.id);
        // …and nothing for a key with a single live task.
        assert!(other_active_with_same_source(&db.conn, first.id, "github:github.com:acme/app:issue:999")
            .await
            .unwrap()
            .is_none());
    }

    /// A finished task is history, not a blocker: dedup only counts the
    /// ACTIVE set, so re-triggering a done issue creates a fresh card.
    #[tokio::test]
    async fn forge_dedup_ignores_terminal_tasks() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-forge-term").await;
        let key = "github:github.com:acme/app:issue:7";

        let first = match create_from_forge(&db.conn, draft(folder_id, "#7"), source(key), false)
            .await
            .unwrap()
        {
            ForgeCreateOutcome::Created(t) => t,
            other => panic!("expected Created, got {other:?}"),
        };
        // Drive it to a terminal state through the plain status column.
        let mut active: work_task::ActiveModel = get_model(&db.conn, first.id)
            .await
            .unwrap()
            .into_active_model();
        active.status = Set(WorkTaskStatus::Canceled);
        active.update(&db.conn).await.unwrap();

        match create_from_forge(&db.conn, draft(folder_id, "#7 again"), source(key), false)
            .await
            .unwrap()
        {
            ForgeCreateOutcome::Created(t) => assert_ne!(t.id, first.id),
            other => panic!("terminal task must not block a re-trigger, got {other:?}"),
        }
    }

    /// Editing a forge task keeps its provenance intact and refuses a folder
    /// move — the folder was validated against the source repo at trigger
    /// time, and a move would silently void that check.
    #[tokio::test]
    async fn forge_task_update_keeps_source_and_pins_folder() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-forge-upd").await;
        let other_folder = seed_folder(&db, "/tmp/wt-forge-upd-2").await;
        let key = "github:github.com:acme/app:issue:55";

        let t = match create_from_forge(&db.conn, draft(folder_id, "#55"), source(key), false)
            .await
            .unwrap()
        {
            ForgeCreateOutcome::Created(t) => t,
            other => panic!("expected Created, got {other:?}"),
        };

        // Title/config edits pass through and leave the source columns alone.
        let edited = update(&db.conn, t.id, draft(folder_id, "#55 · renamed"))
            .await
            .unwrap();
        assert_eq!(edited.title, "#55 · renamed");
        assert_eq!(edited.source_kind.as_deref(), Some("forge_issue"));
        assert_eq!(edited.source_key.as_deref(), Some(key));
        assert!(edited.source_meta.is_some());

        // A pristine PLAIN todo may still move (existing behaviour)…
        let plain = create(&db.conn, draft(folder_id, "movable")).await.unwrap();
        assert!(update(&db.conn, plain.id, draft(other_folder, "movable")).await.is_ok());
        // …but a forge todo may not.
        let err = update(&db.conn, t.id, draft(other_folder, "#55 · moved"))
            .await
            .expect_err("forge task folder move must be rejected");
        assert!(err.to_string().contains("forge-sourced"), "got: {err}");
    }

    /// `deliverable` is trigger-owned and no editor shows it, so an edit whose
    /// config OMITS the key must keep the stored value — dropping it would
    /// silently hand a report task the write licence back. An explicit null is
    /// a statement, not ignorance, and still clears it.
    #[tokio::test]
    async fn update_preserves_the_stored_deliverable_unless_explicitly_cleared() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-deliv").await;

        let mut with_report = draft(folder_id, "investigate #9");
        with_report.config["deliverable"] = serde_json::json!("report");
        let t = create(&db.conn, with_report).await.unwrap();

        // The dialog rebuilds the config from its own fields and never knew
        // the key: the stored marker survives the round trip, alongside the
        // fields the client DID send.
        let edited = update(&db.conn, t.id, draft(folder_id, "investigate #9 · renamed"))
            .await
            .unwrap();
        assert_eq!(edited.config["deliverable"], serde_json::json!("report"));
        assert_eq!(edited.config["display_text"], serde_json::json!("do the thing"));

        // An explicit null clears — the escape hatch stays open.
        let mut clearing = draft(folder_id, "investigate #9 · cleared");
        clearing.config["deliverable"] = serde_json::Value::Null;
        let cleared = update(&db.conn, t.id, clearing).await.unwrap();
        assert_eq!(cleared.config["deliverable"], serde_json::Value::Null);

        // And a task that never had one gains nothing.
        let plain = create(&db.conn, draft(folder_id, "plain")).await.unwrap();
        let edited = update(&db.conn, plain.id, draft(folder_id, "plain · renamed"))
            .await
            .unwrap();
        assert!(edited.config.get("deliverable").is_none());
    }

    /// The write-first transaction shape under real concurrency: a file-backed
    /// WAL database (multi-connection pool, like production) and two
    /// simultaneous `force=false` triggers must yield exactly one `Created`
    /// and one `Duplicate` — never `database is locked`. This is the exact
    /// failure a dedup-SELECT-before-first-write shape produced (deferred
    /// read→write upgrade loses under WAL instead of waiting).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_forge_creates_yield_one_created_one_duplicate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::test_helpers::fresh_disk_db(dir.path()).await;
        let folder_id = seed_folder(&db, "/tmp/wt-forge-race").await;
        let key = "github:github.com:acme/app:issue:900";

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for n in 0..2 {
            let conn = db.conn.clone();
            let barrier = barrier.clone();
            let d = draft(folder_id, &format!("#900 · racer {n}"));
            let s = source(key);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                create_from_forge(&conn, d, s, false).await
            }));
        }
        let mut created = 0;
        let mut duplicate = 0;
        for handle in handles {
            match handle.await.expect("join").expect("no lock error") {
                ForgeCreateOutcome::Created(_) => created += 1,
                ForgeCreateOutcome::Duplicate(_) => duplicate += 1,
            }
        }
        assert_eq!((created, duplicate), (1, 1));
    }

    /// Resurrection guards on BOTH roads back into the active set: a failed
    /// retry claim and a canceled requeue must refuse while another active
    /// task holds the same source key — and the explicit override waives it.
    #[tokio::test]
    async fn resurrection_guards_block_and_override_waives() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/wt-forge-res").await;
        let key = "github:github.com:acme/app:issue:70";

        let set_status = |id: i32, status: WorkTaskStatus| {
            let conn = db.conn.clone();
            async move {
                let mut active: work_task::ActiveModel =
                    get_model(&conn, id).await.unwrap().into_active_model();
                active.status = Set(status);
                active.update(&conn).await.unwrap();
            }
        };

        // Old task fails; a replacement is triggered (old is terminal, so the
        // create dedup rightly lets it through) and stays active.
        let old = match create_from_forge(&db.conn, draft(folder_id, "#70"), source(key), false)
            .await
            .unwrap()
        {
            ForgeCreateOutcome::Created(t) => t,
            other => panic!("expected Created, got {other:?}"),
        };
        set_status(old.id, WorkTaskStatus::Failed).await;
        let replacement =
            match create_from_forge(&db.conn, draft(folder_id, "#70 v2"), source(key), false)
                .await
                .unwrap()
            {
                ForgeCreateOutcome::Created(t) => t,
                other => panic!("expected Created, got {other:?}"),
            };

        // Retry road (failed → queued): blocked, then waived.
        let err = claim_for_run_with_action(
            &db.conn,
            old.id,
            WorkTaskStatus::Failed,
            "user",
            None,
            false,
        )
        .await
        .expect_err("retry must hit the resurrection guard");
        assert!(err.to_string().contains("duplicate_active_source"), "got: {err}");
        assert_eq!(
            get(&db.conn, old.id).await.unwrap().status,
            WorkTaskStatus::Failed,
            "the losing claim must roll back"
        );
        assert!(claim_for_run_with_action(
            &db.conn,
            old.id,
            WorkTaskStatus::Failed,
            "user",
            None,
            true,
        )
        .await
        .expect("override waives the guard")
        .is_some());

        // Requeue road (canceled → todo): same dance on a fresh terminal task.
        set_status(old.id, WorkTaskStatus::Canceled).await;
        let err = requeue_canceled(&db.conn, old.id, None, &[], false)
            .await
            .expect_err("requeue must hit the resurrection guard");
        assert!(err.to_string().contains("duplicate_active_source"), "got: {err}");
        assert_eq!(
            get(&db.conn, old.id).await.unwrap().status,
            WorkTaskStatus::Canceled,
            "the refused requeue must roll back"
        );
        assert!(requeue_canceled(&db.conn, old.id, None, &[], true).await.unwrap());
        assert_eq!(
            get(&db.conn, old.id).await.unwrap().status,
            WorkTaskStatus::Todo
        );

        // A plain (non-forge) failed task never trips the guard.
        let plain = create(&db.conn, draft(folder_id, "plain")).await.unwrap();
        set_status(plain.id, WorkTaskStatus::Failed).await;
        assert!(claim_for_run_with_action(
            &db.conn,
            plain.id,
            WorkTaskStatus::Failed,
            "user",
            None,
            false,
        )
        .await
        .unwrap()
        .is_some());
        let _ = replacement;
    }
