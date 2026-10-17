    use super::*;
    use crate::acp::types::{
        AcpEvent, AsyncTaskDelta, AsyncTaskUsage, ConnectionStatus, DelegationResultSummary,
        EventEnvelope, PromptCapabilitiesInfo, SessionConfigKindInfo, SessionConfigOptionInfo,
        SessionConfigSelectInfo, SessionModeInfo, SessionModeStateInfo, UserMessageBlock,
    };

    fn fresh_state() -> SessionState {
        SessionState::new(
            "conn-test".to_string(),
            AgentType::ClaudeCode,
            None,
            "win-test".to_string(),
            None,
        )
    }

    /// `ConversationLinked` must forget the live-title skip-cache.
    ///
    /// Today this clear can only ever be a no-op: `emit_conversation_update`
    /// refuses to cache a title while `conversation_id` is `None`, and both
    /// producers of `ConversationLinked` fire only from that same unbound
    /// state, so the cache is already empty every time this runs. It is kept —
    /// and pinned here — because the day something rebinds a LIVE connection to
    /// another row, a cache carried over from the old one would classify the
    /// new row's first title as a repeat and leave it Untitled for the rest of
    /// the connection, with no error anywhere to point at.
    #[test]
    fn conversation_linked_clears_the_native_title_skip_cache() {
        let mut s = fresh_state();
        s.last_native_title = Some("Fix the login flow".into());

        s.apply_event(&AcpEvent::ConversationLinked {
            conversation_id: 7,
            folder_id: 1,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        });

        assert_eq!(s.conversation_id, Some(7));
        assert!(s.last_native_title.is_none());
    }

    #[test]
    fn plan_approval_applies_clears_by_id_and_survives_snapshot() {
        let mut s = fresh_state();
        // Request → pending set + carried on the snapshot for mid-turn attach.
        s.apply_event(&AcpEvent::PlanApprovalRequest {
            approval_id: "ap-1".into(),
            tool_call_id: "call-1".into(),
            plan_markdown: "# Plan".into(),
        });
        let pending = s.pending_plan_approval.clone().expect("pending set");
        assert_eq!(pending.approval_id, "ap-1");
        assert_eq!(pending.tool_call_id, "call-1");
        assert_eq!(pending.plan_markdown, "# Plan");
        assert!(s.to_snapshot().pending_plan_approval.is_some());

        // A resolve for a DIFFERENT id must not wipe the live approval.
        s.apply_event(&AcpEvent::PlanApprovalResolved {
            approval_id: "other".into(),
        });
        assert!(s.pending_plan_approval.is_some());

        // Matching resolve clears it (and the snapshot).
        s.apply_event(&AcpEvent::PlanApprovalResolved {
            approval_id: "ap-1".into(),
        });
        assert!(s.pending_plan_approval.is_none());
        assert!(s.to_snapshot().pending_plan_approval.is_none());
    }

    #[test]
    fn session_failure_upserts_merge_monotonically_and_settle_at_turn_end() {
        fn failure(id: &str, revision: u64, severity: &str, title: &str) -> AcpEvent {
            AcpEvent::SessionFailure {
                record: SessionFailureRecord {
                    id: id.into(),
                    revision,
                    category: "limit".into(),
                    severity: severity.into(),
                    title: title.into(),
                    details: None,
                    actions: vec!["retry".into()],
                    resolved: false,
                },
            }
        }
        let mut s = fresh_state();

        // Upsert then revise in place: one entry, latest revision wins.
        s.apply_event(&failure("t1:error", 1, "warning", "retrying"));
        s.apply_event(&failure("t1:error", 2, "warning", "still retrying"));
        assert_eq!(s.session_failures.len(), 1);
        assert_eq!(s.session_failures["t1:error"].revision, 2);
        assert_eq!(s.session_failures["t1:error"].title, "still retrying");

        // Stale and equal-revision replays are rejected (claude re-publishes
        // still-active failures on session/load — must not thrash state).
        s.apply_event(&failure("t1:error", 1, "warning", "stale"));
        s.apply_event(&failure("t1:error", 2, "warning", "replay"));
        assert_eq!(s.session_failures["t1:error"].title, "still retrying");

        // Turn boundary settles warnings — errors stay active (codex keeps
        // terminal records active deliberately).
        s.apply_event(&failure("s:notice", 1, "error", "auth expired"));
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sid".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert!(s.session_failures["t1:error"].resolved);
        assert!(!s.session_failures["s:notice"].resolved);

        // Escalation via id reuse: a HIGHER-revision upsert re-arms the entry
        // (resolved resets) — how codex turns a retry warning into the turn's
        // terminal error.
        s.apply_event(&failure("t1:error", 3, "error", "gave up"));
        let escalated = &s.session_failures["t1:error"];
        assert!(!escalated.resolved);
        assert_eq!(escalated.severity, "error");

        // Starting a NEW prompt settles EVERYTHING — errors included — so a
        // client hydrating mid-turn can't resurrect a failure the owner
        // already acted past (mirrors the frontend reducer's prompt-start
        // settle). Watermarks survive: the stale rev-2 replay stays rejected,
        // and only a genuinely newer revision re-arms the record.
        s.apply_event(&AcpEvent::UserMessage {
            message_id: "m1".into(),
            blocks: vec![],
        });
        assert!(s.session_failures.values().all(|f| f.resolved));
        s.apply_event(&failure("t1:error", 2, "error", "stale replay"));
        assert!(s.session_failures["t1:error"].resolved);
        assert_eq!(s.session_failures["t1:error"].title, "gave up");
        s.apply_event(&failure("t1:error", 4, "error", "it recurred"));
        assert!(!s.session_failures["t1:error"].resolved);

        // The snapshot carries the WHOLE table — resolved entries and their
        // revision watermarks included — so an attaching client can keep
        // rejecting stale upserts.
        let snap = s.to_snapshot();
        assert_eq!(snap.session_failures.len(), 2);
        let json = serde_json::to_value(&snap).unwrap();
        let failures = json.get("session_failures").unwrap().as_array().unwrap();
        assert!(failures.iter().any(|f| {
            f.get("id").and_then(|v| v.as_str()) == Some("t1:error")
                && f.get("revision").and_then(|v| v.as_u64()) == Some(4)
        }));
    }

    fn async_task_delta(task_id: &str, spawned: bool) -> AsyncTaskDelta {
        AsyncTaskDelta {
            task_id: task_id.into(),
            spawned,
            name: None,
            task_type: None,
            description: None,
            show_in_transcript: None,
            can_stop: None,
            state: None,
            summary: None,
            last_tool_name: None,
            usage: None,
            output_file_path: None,
            tool_call_id: None,
            phase: None,
            current_agent: None,
        }
    }

    /// Only the spawn frame carries a task's identity, so it is the only one
    /// allowed to create a row: a progress delta for an id we never saw
    /// announced means codeg failed to read the announcement, and a row with a
    /// placeholder name and no type is worse than no row at all.
    #[test]
    fn async_task_rows_are_created_only_by_a_spawn_delta() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::AsyncTask {
            delta: async_task_delta("ghost", false),
        });
        assert!(s.async_tasks.is_empty());

        let spawn = AsyncTaskDelta {
            name: Some("pnpm test".into()),
            task_type: Some("shell".into()),
            description: Some("pnpm test --watch".into()),
            show_in_transcript: Some(false),
            can_stop: Some(true),
            ..async_task_delta("t1", true)
        };
        s.apply_event(&AcpEvent::AsyncTask { delta: spawn });
        let row = &s.async_tasks["t1"];
        assert_eq!(row.name, "pnpm test");
        assert_eq!(row.task_type, "shell");
        assert!(!row.show_in_transcript);
        assert!(row.can_stop);
        // A spawn frame carries no state field; the row starts live.
        assert_eq!(row.state, "running");
    }

    /// Progress/state deltas are PARTIAL: an absent field must leave the stored
    /// value alone, or the first progress tick would blank out the task's name.
    #[test]
    fn async_task_deltas_revise_only_the_fields_they_carry() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::AsyncTask {
            delta: AsyncTaskDelta {
                name: Some("pnpm test".into()),
                task_type: Some("shell".into()),
                ..async_task_delta("t1", true)
            },
        });
        s.apply_event(&AcpEvent::AsyncTask {
            delta: AsyncTaskDelta {
                last_tool_name: Some("Bash".into()),
                usage: Some(AsyncTaskUsage {
                    total_tokens: 1200,
                    tool_uses: 3,
                    duration_ms: 4500,
                }),
                output_file_path: Some("/tmp/tasks/t1.output".into()),
                ..async_task_delta("t1", false)
            },
        });
        let row = &s.async_tasks["t1"];
        assert_eq!(row.name, "pnpm test");
        assert_eq!(row.task_type, "shell");
        assert_eq!(row.last_tool_name.as_deref(), Some("Bash"));
        assert_eq!(row.usage.as_ref().unwrap().total_tokens, 1200);
        assert_eq!(row.output_file_path.as_deref(), Some("/tmp/tasks/t1.output"));

        // The adapter revises a task AFTER it settles — correcting a
        // best-effort `stopped` into the real outcome, or attaching a late
        // output path. Retaining the row is what lets that correction land as a
        // revision instead of resurrecting the task as a fresh running one.
        s.apply_event(&AcpEvent::AsyncTask {
            delta: AsyncTaskDelta {
                state: Some("stopped".into()),
                ..async_task_delta("t1", false)
            },
        });
        assert_eq!(s.async_tasks["t1"].state, "stopped");
        s.apply_event(&AcpEvent::AsyncTask {
            delta: AsyncTaskDelta {
                state: Some("completed".into()),
                summary: Some("all green".into()),
                ..async_task_delta("t1", false)
            },
        });
        assert_eq!(s.async_tasks["t1"].state, "completed");
        assert_eq!(s.async_tasks["t1"].summary.as_deref(), Some("all green"));

        // The whole table rides the snapshot so a client attaching mid-session
        // merges subsequent deltas against the same rows.
        let snap = s.to_snapshot();
        assert_eq!(snap.async_tasks.len(), 1);
        assert_eq!(snap.async_tasks[0].task_id, "t1");
    }

    /// Reaping an idle connection kills the agent CLI, and with it any
    /// background work. A live async task must hold the connection open on its
    /// own — the transcript watcher is a separate, overlapping observer, so the
    /// two combine by OR (never a sum, which would double-count one shell).
    #[test]
    fn a_live_async_task_alone_defers_the_idle_sweep() {
        let mut s = fresh_state();
        let now = Utc::now();
        assert!(!s.has_active_background_work(now));

        s.apply_event(&AcpEvent::AsyncTask {
            delta: AsyncTaskDelta {
                name: Some("watch".into()),
                ..async_task_delta("t1", true)
            },
        });
        // No `BackgroundActivity` has ever arrived, so this exemption is coming
        // from the async-task table alone.
        assert_eq!(s.background_outstanding, 0);
        assert!(s.has_active_background_work(now));

        s.apply_event(&AcpEvent::AsyncTask {
            delta: AsyncTaskDelta {
                state: Some("completed".into()),
                ..async_task_delta("t1", false)
            },
        });
        assert!(!s.has_active_background_work(now));
    }

    /// The exemption is what stops the idle sweep, so a row that never reaches a
    /// terminal state would otherwise hold the agent CLI open forever. Bounded
    /// by the same window as the watcher's half, and refreshed by any delta —
    /// a task that keeps reporting keeps its exemption for as long as it runs.
    #[test]
    fn a_silent_async_task_stops_deferring_the_sweep_after_the_keepalive_window() {
        let mut s = fresh_state();
        let now = Utc::now();
        s.apply_event(&AcpEvent::AsyncTask {
            delta: async_task_delta("t1", true),
        });
        assert!(s.has_active_background_work(now));

        let past_window = now + background_keepalive_max_age() + chrono::Duration::seconds(1);
        assert!(
            !s.has_active_background_work(past_window),
            "a row that never settles must not exempt the connection forever"
        );

        // Any delta re-arms it, terminal state notwithstanding: the adapter is
        // demonstrably still talking about this task.
        s.apply_event(&AcpEvent::AsyncTask {
            delta: AsyncTaskDelta {
                last_tool_name: Some("Bash".into()),
                ..async_task_delta("t1", false)
            },
        });
        assert!(s.has_active_background_work(Utc::now()));
    }

    /// A fork attaches to a NEW session id on the same process. The old
    /// session's task rows can never settle here again — their terminal frames
    /// are published on the id the `AgentSession` router has stopped routing to
    /// this connection — so they must go, or the strip shows work that never
    /// finishes and the keep-alive pins the CLI open.
    #[test]
    fn a_session_id_change_drops_the_previous_session_tasks() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "s1".into(),
        });
        s.apply_event(&AcpEvent::AsyncTask {
            delta: async_task_delta("t1", true),
        });
        assert!(s.has_active_background_work(Utc::now()));

        // A duplicate announcement of the SAME id is a replay, not a fork.
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "s1".into(),
        });
        assert_eq!(s.async_tasks.len(), 1);

        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "s2".into(),
        });
        assert!(s.async_tasks.is_empty());
        assert!(!s.has_active_background_work(Utc::now()));
    }

    /// A turn the agent started itself (Grok's follow-up to a background
    /// workflow) never took the prompt gate, so its end must not release it: a
    /// prompt admitted while it ran owns the flag until ITS turn completes.
    #[test]
    fn an_agent_initiated_turn_end_leaves_an_admitted_prompts_gate() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "a late frame of the turn before".into(),
            parent_tool_use_id: None,
        });
        assert!(s.begin_agent_initiated_turn());
        assert!(s.live_message.is_none(), "the turn starts from a clean slate");
        s.apply_event(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "report".into(),
            parent_tool_use_id: None,
        });
        // The manager admits a prompt mid-way.
        s.turn_in_flight = true;
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "cancelled".into(),
            agent_type: "grok".into(),
        });
        assert!(s.turn_in_flight, "the admitted prompt still owns the gate");
        assert!(!s.agent_initiated_turn);
        assert_eq!(s.last_assistant_text.as_deref(), Some("report"));

        // The prompt's own turn then releases it as always.
        s.apply_event(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "grok".into(),
        });
        assert!(!s.turn_in_flight);
    }

    /// An admitted prompt owns the next turn: the agent's own output then
    /// streams into it rather than into a turn of its own.
    #[test]
    fn an_agent_initiated_turn_yields_to_an_admitted_prompt() {
        let mut s = fresh_state();
        s.turn_in_flight = true;
        assert!(!s.begin_agent_initiated_turn());
        assert!(!s.agent_initiated_turn);
    }

    /// Text the agent sends between turns still lands in `live_message`, but a
    /// prompted turn starts from an empty one — as every client's reducer does
    /// at `Prompting` — so neither the snapshot a mid-turn attach renders nor
    /// the result the turn hands a delegation parent opens with it.
    #[test]
    fn a_prompted_turn_starts_from_an_empty_live_message() {
        let text = |text: &str| AcpEvent::ContentDelta {
            text: text.into(),
            parent_tool_use_id: None,
        };
        let mut s = fresh_state();
        s.apply_event(&text("The job finished."));
        assert!(s.live_message.is_some());

        s.apply_event(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        });
        assert!(s.live_message.is_none());

        s.apply_event(&text("You're welcome."));
        let live = s
            .to_snapshot()
            .live_message
            .expect("the turn's live message");
        assert!(matches!(
            live.content.as_slice(),
            [LiveContentBlock::Text { text, .. }] if text == "You're welcome."
        ));
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("You're welcome."));
    }

    #[test]
    fn session_failure_warnings_survive_non_clean_turn_ends() {
        fn failure(id: &str, revision: u64, severity: &str, title: &str) -> AcpEvent {
            AcpEvent::SessionFailure {
                record: SessionFailureRecord {
                    id: id.into(),
                    revision,
                    category: "connection".into(),
                    severity: severity.into(),
                    title: title.into(),
                    details: None,
                    actions: vec!["new_session".into()],
                    resolved: false,
                },
            }
        }
        fn turn_complete(stop_reason: &str) -> AcpEvent {
            AcpEvent::TurnComplete {
                session_id: "sid".into(),
                stop_reason: stop_reason.into(),
                agent_type: "claude_code".into(),
            }
        }
        let mut s = fresh_state();
        s.apply_event(&failure("t1:error", 5, "warning", "Reconnecting, attempt 5 of 5."));

        // A cancelled/failed/empty exit is NOT recovery — the incident (e.g.
        // reconnect attempts with the network still down) must stay active
        // instead of collapsing into a "recovered" row (2026-08-15 field
        // report: the banner claimed recovery while offline).
        for reason in ["cancelled", "empty", "refusal", "unknown"] {
            s.apply_event(&turn_complete(reason));
            assert!(
                !s.session_failures["t1:error"].resolved,
                "stop_reason={reason} must not settle warnings"
            );
        }

        // The terminal escalation rides the prompt RESPONSE and the loop
        // applies it BEFORE `TurnComplete` (see `response_session_failure`):
        // the same id flips to severity "error", so even the adapters'
        // disguised clean `end_turn` carrying it cannot settle the incident.
        s.apply_event(&failure("t1:error", 6, "error", "The connection was lost."));
        s.apply_event(&turn_complete("end_turn"));
        let escalated = &s.session_failures["t1:error"];
        assert!(!escalated.resolved);
        assert_eq!(escalated.severity, "error");
        assert_eq!(escalated.revision, 6);

        // A clean end still settles a genuine warning — and only the warning.
        s.apply_event(&failure("w2", 1, "warning", "transient notice"));
        s.apply_event(&turn_complete("end_turn"));
        assert!(s.session_failures["w2"].resolved);
        assert!(!s.session_failures["t1:error"].resolved);
    }

    /// Issue #496: a long turn that reconnects N times stacked N permanent
    /// strips, because the only settle points were a clean `end_turn` and the
    /// next prompt. Turn PROGRESS settles the incident instead — codex's own
    /// `completeRetryIncidentOnTurnProgress`.
    #[test]
    fn session_failure_retry_incidents_settle_on_turn_progress() {
        fn failure(id: &str, category: &str, severity: &str) -> AcpEvent {
            AcpEvent::SessionFailure {
                record: SessionFailureRecord {
                    id: id.into(),
                    revision: 1,
                    category: category.into(),
                    severity: severity.into(),
                    title: format!("{id} title"),
                    details: None,
                    actions: vec![],
                    resolved: false,
                },
            }
        }
        fn text(text: &str) -> AcpEvent {
            AcpEvent::ContentDelta {
                text: text.into(),
                parent_tool_use_id: None,
            }
        }

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        });
        s.apply_event(&failure("i1", "connection", "warning"));
        s.apply_event(&failure("i2", "service", "warning"));
        // Non-incident informational records: codex config/skill-budget
        // notices and claude advisories both land on category "unknown".
        // Nothing "recovers" those, so progress must leave them readable.
        s.apply_event(&failure("notice", "unknown", "warning"));
        s.apply_event(&failure("err", "connection", "error"));

        s.apply_event(&text("back online"));
        assert!(s.session_failures["i1"].resolved);
        assert!(s.session_failures["i2"].resolved);
        assert!(!s.session_failures["notice"].resolved);
        assert!(!s.session_failures["err"].resolved);

        // Thinking and a fresh tool call are turn output too.
        s.apply_event(&failure("i3", "limit", "warning"));
        s.apply_event(&AcpEvent::Thinking {
            text: "hmm".into(),
            parent_tool_use_id: None,
        });
        assert!(s.session_failures["i3"].resolved);

        s.apply_event(&failure("i4", "connection", "warning"));
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "Read".into(),
            kind: "read".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        assert!(s.session_failures["i4"].resolved);

        // The notice still waits for the clean turn boundary; the terminal
        // error survives it, exactly as before.
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sid".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert!(s.session_failures["notice"].resolved);
        assert!(!s.session_failures["err"].resolved);
    }

    #[test]
    fn turn_complete_clears_pending_plan_approval() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PlanApprovalRequest {
            approval_id: "ap-1".into(),
            tool_call_id: "c".into(),
            plan_markdown: String::new(),
        });
        assert!(s.pending_plan_approval.is_some());
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sid".into(),
            stop_reason: "end_turn".into(),
            agent_type: "grok".into(),
        });
        assert!(s.pending_plan_approval.is_none());
    }

    #[test]
    fn user_message_supersedes_stale_pending_plan_approval() {
        // A new turn starting without a clean TurnComplete (fork/resume re-prompt,
        // queued prompt sent instead of answering) must not leave a dead approval
        // in the snapshot for a mid-turn attach to render.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PlanApprovalRequest {
            approval_id: "ap-1".into(),
            tool_call_id: "c".into(),
            plan_markdown: "# plan".into(),
        });
        assert!(s.pending_plan_approval.is_some());
        s.apply_event(&AcpEvent::UserMessage {
            message_id: "m1".into(),
            blocks: vec![],
        });
        assert!(s.pending_plan_approval.is_none());
    }

    #[test]
    fn background_activity_mirrors_outstanding_and_gates_keepalive() {
        let mut s = fresh_state();
        assert!(!s.has_active_background_work(Utc::now()));

        s.apply_event(&AcpEvent::BackgroundActivity {
            session_id: "sid".into(),
            turns: vec![],
            outstanding: 2,
            settled: vec![],
            watermark: 42,
        });
        assert_eq!(s.background_outstanding, 2);
        let now = Utc::now();
        assert!(s.has_active_background_work(now));

        // The exemption lapses once the last watcher heartbeat is older than
        // the max age — a dead/wedged watcher can't pin a connection forever.
        let long_after = now + background_keepalive_max_age() + chrono::Duration::seconds(1);
        assert!(!s.has_active_background_work(long_after));

        // Settled back to zero: no exemption regardless of recency.
        s.apply_event(&AcpEvent::BackgroundActivity {
            session_id: "sid".into(),
            turns: vec![],
            outstanding: 0,
            settled: vec![],
            watermark: 43,
        });
        assert!(!s.has_active_background_work(Utc::now()));
    }

    #[test]
    fn snapshot_carries_background_outstanding_and_skips_zero_on_wire() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::BackgroundActivity {
            session_id: "sid".into(),
            turns: vec![],
            outstanding: 3,
            settled: vec![],
            watermark: 0,
        });
        assert_eq!(s.to_snapshot().background_outstanding, 3);
        let json = serde_json::to_value(s.to_snapshot()).unwrap();
        assert_eq!(
            json.get("background_outstanding").and_then(|v| v.as_u64()),
            Some(3)
        );

        // Zero is skipped so the common no-background snapshot stays
        // byte-identical with the pre-feature wire shape.
        let zero = fresh_state();
        let json = serde_json::to_value(zero.to_snapshot()).unwrap();
        assert!(json.get("background_outstanding").is_none());
    }

    #[test]
    fn new_session_starts_with_seq_zero_and_connecting_status() {
        let s = fresh_state();
        assert_eq!(s.event_seq, 0);
        assert_eq!(s.status, ConnectionStatus::Connecting);
        assert!(s.external_id.is_none());
        assert!(s.live_message.is_none());
        assert!(s.active_tool_calls.is_empty());
        assert!(s.pending_permission.is_none());
        assert!(!s.fork_supported);
        assert!(s.available_commands.is_empty());
        assert!(!s.selectors_ready);
        assert!(s.pending_user_message.is_none());
    }

    fn text_user_message(id: &str, text: &str) -> AcpEvent {
        AcpEvent::UserMessage {
            message_id: id.to_string(),
            blocks: vec![UserMessageBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn user_message_event_captures_pending_user_message() {
        // The in-flight user prompt is captured so a mid-turn attacher renders
        // the user turn from the snapshot (the one-shot event won't replay).
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-1", "hello agent"));
        let pending = s.pending_user_message.as_ref().expect("pending set");
        assert_eq!(pending.message_id, "user-1");
        assert_eq!(
            pending.blocks,
            vec![UserMessageBlock::Text {
                text: "hello agent".into()
            }]
        );
        assert!(
            s.pending_user_message_started_at.is_some(),
            "the turn-start instant is captured alongside the pending prompt"
        );
    }

    #[test]
    fn pending_user_message_started_at_has_no_sub_ms_residue() {
        // The recency gate in `apply_in_flight_message_id` compares this
        // stamp against millisecond-precision parsed-turn timestamps
        // (Cursor's journal upgrade rewrites the in-flight user turn to a
        // ms send stamp taken right after this event applies). Sub-ms
        // residue would order the threshold AFTER a stamp taken later in
        // real time and unstamp the turn.
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-1", "hello"));
        let at = s.pending_user_message_started_at.expect("stamp set");
        assert_eq!(at.timestamp_subsec_nanos() % 1_000_000, 0);
    }

    #[test]
    fn turn_complete_clears_pending_user_message() {
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-1", "hi"));
        assert!(s.pending_user_message.is_some());
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sess".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        });
        assert!(
            s.pending_user_message.is_none(),
            "a completed turn must clear the pending user message (no stale snapshot)"
        );
        assert!(
            s.pending_user_message_started_at.is_none(),
            "the turn-start instant is cleared in lockstep with the pending prompt"
        );
    }

    #[test]
    fn to_snapshot_carries_pending_user_message() {
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-7", "snapshot me"));
        let pending = s
            .to_snapshot()
            .pending_user_message
            .expect("snapshot carries pending");
        assert_eq!(pending.message_id, "user-7");
    }

    #[test]
    fn snapshot_round_trips_pending_user_message_and_omits_when_absent() {
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-9", "round trip"));
        let snap = s.to_snapshot();
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.pending_user_message, snap.pending_user_message);
        // No-pending snapshot keeps the field off the wire (byte-identical with
        // the pre-feature shape).
        let empty_json = serde_json::to_string(&fresh_state().to_snapshot()).expect("serialize");
        assert!(
            !empty_json.contains("pending_user_message"),
            "no-pending snapshot must omit the field"
        );
    }

    #[test]
    fn snapshot_carries_last_error_and_clears_on_next_prompt() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::Error {
            message: "ACP protocol error: forbidden".into(),
            agent_type: "claude_code".into(),
            code: Some("forbidden".into()),
            details: None,
            terminal: true,
        });

        let snap = s.to_snapshot();
        assert_eq!(
            snap.last_error,
            Some(SessionLastError {
                message: "ACP protocol error: forbidden".into(),
                code: Some("forbidden".into()),
                details: None,
            })
        );

        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.last_error, snap.last_error);

        let empty_json = serde_json::to_string(&fresh_state().to_snapshot()).expect("serialize");
        assert!(
            !empty_json.contains("last_error"),
            "no-error snapshot must omit last_error"
        );

        s.apply_event(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        });
        assert!(
            s.to_snapshot().last_error.is_none(),
            "new prompts clear stale snapshot-recoverable errors"
        );
    }

    #[test]
    fn latest_live_reply_prefers_answer_after_last_tool_call() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "let me check".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "Found 3 files.\nDetails here".into(),
            parent_tool_use_id: None,
        });
        // Last non-empty line of the text that follows the final tool call.
        assert_eq!(s.latest_live_reply(100).as_deref(), Some("Details here"));
    }

    #[test]
    fn latest_live_reply_falls_back_to_thinking_then_tool() {
        // Thinking only → `thinking:` prefix.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::Thinking {
            text: "pondering options".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(
            s.latest_live_reply(100).as_deref(),
            Some("thinking: pondering options")
        );

        // A tool call with no trailing text / thinking → `running tool:` prefix.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-9".into(),
            title: "grep files".into(),
            kind: "search".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        assert_eq!(
            s.latest_live_reply(100).as_deref(),
            Some("running tool: grep files")
        );
    }

    #[test]
    fn latest_live_reply_truncates_to_char_budget_and_handles_empty() {
        // No live message yet → nothing to report.
        assert_eq!(fresh_state().latest_live_reply(100), None);

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "0123456789abcdef".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(s.latest_live_reply(10).as_deref(), Some("0123456789…"));
    }

    #[test]
    fn latest_live_reply_extracts_last_line_from_large_multiline_and_truncates_utf8() {
        let mut s = fresh_state();
        // A large multi-line streamed answer, a final multi-byte line, then
        // trailing blank lines (which must be skipped). The tail extraction must
        // not copy the whole answer, and truncation must land on a codepoint
        // boundary.
        let huge = "x".repeat(5000);
        let last = "résumé 完成 ▸ 配置已更新";
        s.apply_event(&AcpEvent::ContentDelta {
            text: format!("{huge}\nintermediate\n{last}\n   \n"),
            parent_tool_use_id: None,
        });
        let out = s.latest_live_reply(8).unwrap();
        // First 8 chars of `last` are r é s u m é <space> 完, then a truncation
        // marker — codepoint-safe (8 multi-byte chars + the ellipsis), proving
        // the cap counts chars, not bytes.
        assert_eq!(out, "résumé 完…");
        assert_eq!(out.chars().count(), 9);
    }

    #[test]
    fn latest_live_reply_stitches_text_split_by_interleaved_thinking() {
        // A Thinking block between two text deltas yields two separate Text
        // blocks; their concatenation forms the single answer line.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "Answer ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::Thinking { text: "hmm".into(), parent_tool_use_id: None });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "continues here".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(
            s.latest_live_reply(100).as_deref(),
            Some("Answer continues here")
        );
    }

    #[test]
    fn selectors_ready_event_latches_state_and_snapshot() {
        let mut s = fresh_state();
        assert!(!s.selectors_ready);
        assert!(!s.to_snapshot().selectors_ready);
        s.apply_event(&AcpEvent::SelectorsReady);
        assert!(s.selectors_ready);
        assert!(s.to_snapshot().selectors_ready);
        // Idempotent — staying true on a second apply.
        s.apply_event(&AcpEvent::SelectorsReady);
        assert!(s.selectors_ready);
    }

    #[test]
    fn conversation_status_changed_event_is_a_visible_field_noop() {
        use crate::db::entities::conversation::ConversationStatus;
        // Seed a fully-populated state so we can verify nothing visible mutates
        // when ConversationStatusChanged is applied.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-1".into(),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "hello".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::ConversationLinked {
            conversation_id: 7,
            folder_id: 3,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        });
        let before = s.to_snapshot();
        let before_status = s.status.clone();
        let before_conversation_id = s.conversation_id;
        let before_external_id = s.external_id.clone();

        s.apply_event(&AcpEvent::ConversationStatusChanged {
            conversation_id: 7,
            status: ConversationStatus::InProgress,
        });

        // Visible state fields unchanged.
        assert_eq!(s.status, before_status);
        assert_eq!(s.conversation_id, before_conversation_id);
        assert_eq!(s.external_id, before_external_id);
        assert!(
            s.live_message.is_some(),
            "live_message must be preserved across status-changed event"
        );
        assert_eq!(s.active_tool_calls.len(), 1);
        assert!(s.active_tool_calls.contains_key("tc-1"));

        // Snapshot output unchanged (modulo last_activity_at which is internal).
        let after = s.to_snapshot();
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(&after).unwrap(),
            "snapshot must be byte-identical after no-op event"
        );
    }

    #[test]
    fn conversation_linked_event_writes_ids_into_state_and_snapshot() {
        let mut s = fresh_state();
        assert_eq!(s.conversation_id, None);
        assert_eq!(s.folder_id, None);
        s.apply_event(&AcpEvent::ConversationLinked {
            conversation_id: 42,
            folder_id: 7,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        });
        assert_eq!(s.conversation_id, Some(42));
        assert_eq!(s.folder_id, Some(7));
        let snap = s.to_snapshot();
        assert_eq!(snap.conversation_id, Some(42));
        assert_eq!(snap.folder_id, Some(7));
    }

    #[test]
    fn session_started_sets_external_id_and_connected_status() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-42".into(),
        });
        assert_eq!(s.external_id.as_deref(), Some("ext-42"));
        assert_eq!(s.status, ConnectionStatus::Connected);
    }

    #[tokio::test]
    async fn session_started_signal_fires_when_session_started_applies() {
        let mut s = fresh_state();
        let rx = s.install_session_started_signal();
        // Pre-fire: rx not ready.
        assert!(s.session_started_tx.is_some());

        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-1".into(),
        });

        // tx was take()'d.
        assert!(s.session_started_tx.is_none());
        // rx resolves with Ok(()) — bounded timeout because the test must
        // never hang if the signal logic regresses.
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx).await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "rx must fire on SessionStarted; got {result:?}"
        );
    }

    #[tokio::test]
    async fn session_started_signal_is_single_shot_safe_against_replay() {
        let mut s = fresh_state();
        let rx = s.install_session_started_signal();
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-1".into(),
        });
        // Replay (or any second SessionStarted) must not panic / double-fire.
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-2".into(),
        });
        // The first send delivered; rx is consumed.
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx).await;
        assert!(matches!(result, Ok(Ok(()))));
    }

    #[tokio::test]
    async fn session_started_rx_aborts_when_state_drops_before_session_started() {
        // Mirrors the production "agent died before SessionStarted" path:
        // SessionState owns tx, gets dropped → rx receives RecvError. The
        // dedup waiter in `spawn_agent` treats this as "abort, release
        // dedup_lock, let next caller proceed".
        let rx = {
            let mut s = fresh_state();
            s.install_session_started_signal()
            // s drops here, taking tx with it.
        };
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx).await;
        assert!(
            matches!(result, Ok(Err(_))),
            "rx must receive Err when sender drops without sending; got {result:?}"
        );
    }

    #[test]
    fn content_delta_creates_live_message_then_appends() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "hello ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "world".into(),
            parent_tool_use_id: None,
        });
        let live = s.live_message.as_ref().expect("live_message expected");
        assert_eq!(
            live.content.len(),
            1,
            "consecutive text deltas merge into one block"
        );
        match &live.content[0] {
            LiveContentBlock::Text { text, .. } => assert_eq!(text, "hello world"),
            _ => panic!("expected text block"),
        }
        assert!(matches!(live.role, MessageRole::Assistant));
    }

    #[test]
    fn thinking_delta_creates_separate_block_from_text() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta { text: "T".into(), parent_tool_use_id: None });
        s.apply_event(&AcpEvent::Thinking { text: "X".into(), parent_tool_use_id: None });
        s.apply_event(&AcpEvent::ContentDelta { text: "Y".into(), parent_tool_use_id: None });
        let live = s.live_message.as_ref().unwrap();
        assert_eq!(live.content.len(), 3);
        match &live.content[0] {
            LiveContentBlock::Text { text, .. } => assert_eq!(text, "T"),
            _ => panic!("expected text"),
        }
        match &live.content[1] {
            LiveContentBlock::Thinking { text, .. } => assert_eq!(text, "X"),
            _ => panic!("expected thinking"),
        }
        match &live.content[2] {
            LiveContentBlock::Text { text, .. } => assert_eq!(text, "Y"),
            _ => panic!("expected text"),
        }
    }

    /// Parent → subagent → parent interleave must produce three blocks: the
    /// merge predicate requires the SAME `parent_tool_use_id`, so subagent
    /// prose can never concatenate onto the main thread (and vice versa).
    #[test]
    fn parented_delta_interleave_never_merges_across_attribution() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "main ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "sub".into(),
            parent_tool_use_id: Some("toolu_parent".into()),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "more main".into(),
            parent_tool_use_id: None,
        });
        let live = s.live_message.as_ref().unwrap();
        assert_eq!(live.content.len(), 3, "attribution boundaries split blocks");
        match &live.content[1] {
            LiveContentBlock::Text {
                text,
                parent_tool_use_id,
            } => {
                assert_eq!(text, "sub");
                assert_eq!(parent_tool_use_id.as_deref(), Some("toolu_parent"));
            }
            other => panic!("expected parented text block, got {other:?}"),
        }
    }

    /// The frontend reducer drops an empty `CONTENT_DELTA` outright, so a
    /// streaming client never sees an empty `Text` block. If this side kept
    /// one, a snapshot-hydrated client would get an extra block the streaming
    /// one lacks — and prose either side of it would render as two runs
    /// instead of one (#494 on the snapshot path only).
    #[test]
    fn empty_text_delta_adds_no_block_and_never_splits_a_run() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::Thinking {
            text: "before".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: String::new(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::Thinking {
            text: " after".into(),
            parent_tool_use_id: None,
        });
        let live = s.live_message.as_ref().expect("live message");
        assert_eq!(live.content.len(), 1, "no empty Text block was inserted");
        assert!(
            matches!(&live.content[0], LiveContentBlock::Thinking { text, .. } if text == "before after"),
            "the thinking run stayed one block, got {:?}",
            live.content[0]
        );
    }

    /// …and it must not open a live message either — same as the reducer,
    /// which returns before `ensureLiveMessage`.
    #[test]
    fn empty_text_delta_does_not_open_a_live_message() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: String::new(),
            parent_tool_use_id: None,
        });
        assert!(s.live_message.is_none());
    }

    #[test]
    fn parented_deltas_with_same_parent_merge() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "a".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "b".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        // A different parent's thinking starts its own block.
        s.apply_event(&AcpEvent::Thinking {
            text: "t1".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "t2".into(),
            parent_tool_use_id: Some("toolu_q".into()),
        });
        let live = s.live_message.as_ref().unwrap();
        assert_eq!(live.content.len(), 3);
        assert!(
            matches!(&live.content[0], LiveContentBlock::Text { text, .. } if text == "ab"),
            "same-parent text deltas merge"
        );
        assert!(
            matches!(&live.content[2], LiveContentBlock::Thinking { text, parent_tool_use_id }
                if text == "t2" && parent_tool_use_id.as_deref() == Some("toolu_q")),
            "different-parent thinking splits"
        );
    }

    /// Out-of-turn parented chunks (async subagent still streaming after the
    /// parent turn settled) must not resurrect a live_message via
    /// `ensure_live_message` — the snapshot would hand that ghost to every
    /// attaching client. Main-thread chunks keep the unconditional append.
    #[test]
    fn parented_delta_outside_prompting_does_not_touch_live_message() {
        let mut s = fresh_state();
        assert_ne!(s.status, ConnectionStatus::Prompting);
        s.apply_event(&AcpEvent::ContentDelta {
            text: "late sub text".into(),
            parent_tool_use_id: Some("toolu_gone".into()),
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "late sub think".into(),
            parent_tool_use_id: Some("toolu_gone".into()),
        });
        assert!(
            s.live_message.is_none(),
            "parented chunks must not create live_message outside a turn"
        );
        s.apply_event(&AcpEvent::ContentDelta {
            text: "main".into(),
            parent_tool_use_id: None,
        });
        assert!(
            s.live_message.is_some(),
            "main-thread append stays unconditional"
        );
    }

    /// Snapshot round-trip: `parent_tool_use_id` survives serialization, and a
    /// snapshot written by an older backend (no field) still deserializes.
    #[test]
    fn live_block_parent_survives_snapshot_and_old_snapshots_parse() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "sub".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        let snap = s.to_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back: LiveSessionSnapshot = serde_json::from_str(&json).unwrap();
        let live = back.live_message.expect("live message in snapshot");
        assert!(matches!(
            &live.content[0],
            LiveContentBlock::Text { parent_tool_use_id, .. }
                if parent_tool_use_id.as_deref() == Some("toolu_p")
        ));

        let legacy: LiveContentBlock =
            serde_json::from_str(r#"{"kind":"text","text":"old"}"#).unwrap();
        assert!(matches!(
            legacy,
            LiveContentBlock::Text {
                parent_tool_use_id: None,
                ..
            }
        ));
    }

    /// `last_assistant_text` is the delegation child's result — a subagent's
    /// trailing prose is the CHILD's voice and must not read as the answer.
    #[test]
    fn last_assistant_text_ignores_parented_blocks() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "final answer".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: " SUBAGENT NOISE".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sess-1".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("final answer"));
    }

    #[test]
    fn latest_live_reply_ignores_parented_blocks() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::Thinking {
            text: "sub thinking".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "sub text".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        assert_eq!(
            s.latest_live_reply(200),
            None,
            "parented-only content must not surface as the parent's live reply"
        );
        s.apply_event(&AcpEvent::ContentDelta {
            text: "main progress".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(s.latest_live_reply(200).as_deref(), Some("main progress"));
    }

    #[test]
    fn tool_call_inserts_pending_entry() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").expect("tc-1 inserted");
        assert_eq!(entry.status, ToolCallStatus::Pending);
        assert_eq!(entry.kind, ToolKind::Execute);
        assert_eq!(entry.label, "ls");
        assert!(entry.input.is_none());
        assert!(entry.output.is_none());
    }

    #[test]
    fn snapshot_active_tool_calls_are_sorted_by_id() {
        let mut s = fresh_state();
        for id in ["tc-z", "tc-a", "tc-m"] {
            s.apply_event(&AcpEvent::ToolCall {
                tool_call_id: id.into(),
                title: id.into(),
                kind: "read".into(),
                status: "pending".into(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            });
        }
        let snap = s.to_snapshot();
        let ids: Vec<&str> = snap
            .active_tool_calls
            .iter()
            .map(|tc| tc.id.as_str())
            .collect();
        assert_eq!(ids, vec!["tc-a", "tc-m", "tc-z"]);
    }

    /// Open a tool call and finish it, with `output_bytes` of tool output.
    /// `settled` false leaves it in progress (still streaming its output).
    fn run_tool_call(s: &mut SessionState, id: &str, output_bytes: usize, settled: bool) {
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: id.into(),
            title: format!("Read src/{id}.rs"),
            kind: "read".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: Some(format!("{{\"file_path\":\"src/{id}.rs\"}}")),
            raw_output: None,
            locations: Some(serde_json::json!([{ "path": format!("src/{id}.rs") }])),
            meta: Some(serde_json::json!({ "codeg.delegation": { "status": "completed" } })),
            images: None,
        });
        let status = if settled { "completed" } else { "in_progress" };
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: id.into(),
            title: None,
            status: Some(status.to_string()),
            content: None,
            raw_input: None,
            raw_output: Some("o".repeat(output_bytes)),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
    }

    /// The ordinary turn stays byte-identical: nothing is trimmed while the
    /// table fits the budget, so the wire shape is exactly what it always was.
    #[test]
    fn snapshot_carries_every_tool_call_whole_while_it_fits_the_budget() {
        let mut s = fresh_state();
        for i in 0..20 {
            run_tool_call(&mut s, &format!("tc-{i:03}"), 4 * 1024, true);
        }

        let snap = s.to_snapshot();
        assert_eq!(snap.active_tool_calls.len(), 20);
        for tc in &snap.active_tool_calls {
            let live = &s.active_tool_calls[&tc.id];
            assert_eq!(
                serde_json::to_value(tc).unwrap(),
                serde_json::to_value(live).unwrap(),
                "{} must ship exactly as held",
                tc.id
            );
        }
    }

    /// #380: `active_tool_calls` is cleared only at `TurnComplete`, so a turn
    /// that keeps working accumulates every tool call it ever made — the
    /// reporter sampled 1983 entries and a 26.7 MB snapshot on a turn that had
    /// not reached one, and the client that parses that on attach stopped
    /// responding.
    ///
    /// The snapshot now spends a byte budget over the finished calls' payload,
    /// newest first, WITHOUT dropping an entry: every `ToolCallRef` in the live
    /// message must still resolve, or the reattaching client renders the
    /// in-flight turn with tool cards missing from the middle of it.
    #[test]
    fn snapshot_bounds_finished_tool_call_payload_on_a_long_turn() {
        const CALLS: usize = 300;
        const OUTPUT_BYTES: usize = 16 * 1024;
        let mut s = fresh_state();
        for i in 0..CALLS {
            run_tool_call(&mut s, &format!("tc-{i:04}"), OUTPUT_BYTES, true);
        }
        // …and one still running, the call the attaching client has to keep
        // rendering from live events.
        run_tool_call(&mut s, "tc-running", OUTPUT_BYTES, false);

        // What the state holds, and what shipping it whole would have cost.
        assert_eq!(s.active_tool_calls.len(), CALLS + 1);
        let held: usize = s
            .active_tool_calls
            .values()
            .map(tool_call_trimmable_bytes)
            .sum();
        assert!(
            held > 2 * MAX_SNAPSHOT_TOOL_PAYLOAD_BYTES,
            "the turn must hold well past the budget for this to test anything (held {held})"
        );

        let snap = s.to_snapshot();
        let wire = serde_json::to_string(&snap).expect("serialize snapshot");
        assert!(
            wire.len() < MAX_SNAPSHOT_TOOL_PAYLOAD_BYTES * 3 / 2,
            "snapshot must stay near the budget, got {} bytes for {held} bytes held",
            wire.len()
        );

        // Nothing is dropped: same count, same (id-sorted) order, and every
        // `ToolCallRef` block in the live message still resolves.
        assert_eq!(snap.active_tool_calls.len(), CALLS + 1);
        let wire_ids: Vec<&str> = snap
            .active_tool_calls
            .iter()
            .map(|tc| tc.id.as_str())
            .collect();
        let held_ids: Vec<&str> = s.active_tool_calls.keys().map(String::as_str).collect();
        assert_eq!(wire_ids, held_ids);
        let by_id: std::collections::BTreeMap<&str, &ToolCallState> = snap
            .active_tool_calls
            .iter()
            .map(|tc| (tc.id.as_str(), tc))
            .collect();
        for block in &s.live_message.as_ref().expect("live message").content {
            if let LiveContentBlock::ToolCallRef { tool_call_id } = block {
                assert!(
                    by_id.contains_key(tool_call_id.as_str()),
                    "{tool_call_id} is anchored in the live message but missing from the snapshot"
                );
            }
        }

        // The running call keeps its partial output at any size — nothing else
        // has it. So does the newest finished one.
        assert!(by_id["tc-running"].output.is_some());
        assert!(by_id[format!("tc-{:04}", CALLS - 1).as_str()].output.is_some());

        // The oldest finished calls ship without the payload, but keep every
        // field that identifies the card.
        let oldest = by_id["tc-0000"];
        assert!(oldest.output.is_none(), "oldest call must shed its output");
        assert!(oldest.content.is_none());
        assert!(oldest.input.is_none());
        assert!(oldest.locations.is_none());
        assert_eq!(oldest.label, "Read src/tc-0000.rs");
        assert_eq!(oldest.kind, ToolKind::Read);
        assert_eq!(oldest.status, ToolCallStatus::Completed);
        assert!(
            oldest.meta.is_some(),
            "delegation meta re-anchors an inline sub-thread on attach"
        );

        // And the trimming is a tail, not a purge: the budget is actually spent
        // on the recent calls rather than thrown away.
        let kept = snap
            .active_tool_calls
            .iter()
            .filter(|tc| tc.output.is_some())
            .count();
        assert!(
            kept > 32 && kept < CALLS,
            "expected a bounded recent window to keep its output, got {kept}"
        );
    }

    /// The budget bounds finished work only. A single running call bigger than
    /// the whole budget still ships whole: its output exists nowhere else yet,
    /// and its count tracks live concurrency, not how long the turn has run.
    #[test]
    fn snapshot_never_trims_a_running_tool_call() {
        let mut s = fresh_state();
        for i in 0..200 {
            run_tool_call(&mut s, &format!("tc-{i:04}"), 16 * 1024, true);
        }
        run_tool_call(&mut s, "tc-huge", MAX_SNAPSHOT_TOOL_PAYLOAD_BYTES + 1024, false);

        let snap = s.to_snapshot();
        let huge = snap
            .active_tool_calls
            .iter()
            .find(|tc| tc.id == "tc-huge")
            .expect("running call present");
        assert_eq!(huge.status, ToolCallStatus::InProgress);
        assert!(huge.output.is_some(), "a running call is never trimmed");
    }

    /// A generated image survives the trim, however old the call is.
    ///
    /// `isImageGenerationToolCall` classifies the call from the `label` the
    /// trim keeps, and `generated-images-block.tsx` renders an
    /// image-generation block whose `image` is null under a terminal status as
    /// "image generation failed". So shedding the bytes would not show less,
    /// it would report a success as a failure — and an image is exactly the
    /// payload a user would then go looking for.
    #[test]
    fn snapshot_keeps_a_generated_image_on_the_oldest_trimmed_call() {
        let mut s = fresh_state();
        // The oldest call, and the one carrying the image.
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-image".into(),
            title: "Image generation".into(),
            kind: "other".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: Some("{\"prompt\":\"a cat\"}".into()),
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-image".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: Some("o".repeat(16 * 1024)),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: Some(vec![ToolCallImageInfo {
                data: "R0lGODlhAQABAAAAACw=".repeat(64),
                mime_type: "image/png".into(),
                uri: None,
            }]),
        });
        // …then enough finished work after it to push it out of the budget.
        for i in 0..300 {
            run_tool_call(&mut s, &format!("tc-{i:04}"), 16 * 1024, true);
        }

        let snap = s.to_snapshot();
        let image_call = snap
            .active_tool_calls
            .iter()
            .find(|tc| tc.id == "tc-image")
            .expect("the image call still ships");
        assert!(
            image_call.output.is_none(),
            "it is old enough to be trimmed, which is what makes this a test"
        );
        assert_eq!(
            image_call.images.len(),
            1,
            "a trimmed success must not come back as a failed generation"
        );
        assert_eq!(image_call.label, "Image generation");
    }

    /// The bound covers terminal calls only, so a table that never reaches one
    /// ships whole however long it gets.
    ///
    /// Every agent codeg ships reports a terminal status, which is what makes
    /// the carve-out for in-flight calls safe. This pins the assumption rather
    /// than leaving it implicit: an agent that stopped reporting one would
    /// turn this test red instead of silently restoring #380.
    #[test]
    fn snapshot_ships_an_all_unsettled_table_whole() {
        let mut s = fresh_state();
        for i in 0..300 {
            run_tool_call(&mut s, &format!("tc-{i:04}"), 16 * 1024, false);
        }

        let snap = s.to_snapshot();
        assert_eq!(snap.active_tool_calls.len(), 300);
        assert!(
            snap.active_tool_calls
                .iter()
                .all(|tc| tc.output.is_some() && tc.status == ToolCallStatus::InProgress),
            "nothing unsettled is trimmed at any size"
        );
    }

    #[test]
    fn tool_call_content_field_is_preserved_on_state() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: Some("line one\nline two".into()),
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").expect("tc-1 inserted");
        assert_eq!(entry.content.as_deref(), Some("line one\nline two"));

        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: None,
            content: Some("line three".into()),
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        // Phase 2 chooses replace-on-update semantics: update == latest known content.
        assert_eq!(entry.content.as_deref(), Some("line three"));
    }

    #[test]
    fn tool_call_update_merges_status_and_output() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "cat foo.txt".into(),
            kind: "read".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        // raw_output text "\"file contents\"" — i.e. JSON-encoded string.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: Some("\"file contents\"".into()),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        assert_eq!(entry.status, ToolCallStatus::Completed);
        assert_eq!(entry.kind, ToolKind::Read);
        assert_eq!(entry.label, "cat foo.txt");
        match &entry.output {
            Some(ToolCallOutput::Text { content }) => assert_eq!(content, "file contents"),
            other => panic!("expected text output, got {:?}", other),
        }
    }

    #[test]
    fn turn_complete_clears_live_and_tool_calls_and_pending_permission() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta { text: "hi".into(), parent_tool_use_id: None });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "x".into(),
            kind: "read".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-1".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-1", "title": "danger"}),
            options: vec![],
            queued: 0,
        });
        assert!(s.live_message.is_some());
        assert!(s.pending_permission.is_some());
        assert_eq!(s.active_tool_calls.len(), 1);
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        });
        assert!(s.live_message.is_none());
        assert!(s.active_tool_calls.is_empty());
        assert!(s.pending_permission.is_none());
        assert_eq!(s.status, ConnectionStatus::Connected);
    }

    // --- active_delegations: running-only, snapshot-recoverable binding ---

    fn delegation_started(parent_tool_use_id: &str, child_conv: i32) -> AcpEvent {
        AcpEvent::DelegationStarted {
            parent_connection_id: "conn-test".into(),
            parent_tool_use_id: parent_tool_use_id.into(),
            child_connection_id: "child-conn-1".into(),
            child_conversation_id: child_conv,
            agent_type: AgentType::Codex,
            task_preview: "run the tests".into(),
            task_id: "task-ss-1".into(),
        }
    }

    fn delegation_completed(parent_tool_use_id: &str, child_conv: i32) -> AcpEvent {
        AcpEvent::DelegationCompleted {
            parent_connection_id: "conn-test".into(),
            parent_tool_use_id: parent_tool_use_id.into(),
            child_connection_id: "child-conn-1".into(),
            child_conversation_id: child_conv,
            agent_type: AgentType::Codex,
            result: DelegationResultSummary::Ok {
                duration_ms: 1,
                text_preview: None,
            },
        }
    }

    #[test]
    fn delegation_started_populates_active_delegations_and_snapshot() {
        let mut s = fresh_state();
        s.apply_event(&delegation_started("pt-1", 99));

        let d = s
            .active_delegations
            .get("pt-1")
            .expect("active delegation recorded");
        assert_eq!(d.child_conversation_id, 99);
        assert_eq!(d.child_connection_id, "child-conn-1");
        assert_eq!(d.agent_type, AgentType::Codex);

        // Surfaced on the snapshot, and survives the JSON round-trip the web
        // client hydrates from.
        let snap = s.to_snapshot();
        assert_eq!(snap.active_delegations.len(), 1);
        let json = serde_json::to_string(&snap).unwrap();
        let back: LiveSessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.active_delegations.len(), 1);
        assert_eq!(back.active_delegations[0].parent_tool_use_id, "pt-1");
        assert_eq!(back.active_delegations[0].child_conversation_id, 99);
    }

    #[test]
    fn active_delegations_survives_turn_complete() {
        // Core regression for the web-only bug: an async delegation's child runs
        // in the background AFTER the parent's `delegate_to_agent` tool call
        // returns and the parent turn completes. TurnComplete clears
        // live_message / active_tool_calls but MUST NOT clear active_delegations
        // — otherwise the running binding vanishes from the snapshot the instant
        // the parent turn ends, and a web/server attach (snapshot path) can't
        // recover it.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "pt-1".into(),
            title: "delegate_to_agent".into(),
            kind: "other".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&delegation_started("pt-1", 99));
        assert!(s.active_tool_calls.contains_key("pt-1"));
        assert!(s.active_delegations.contains_key("pt-1"));

        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        });

        assert!(
            s.active_tool_calls.is_empty(),
            "TurnComplete still clears in-flight tool calls"
        );
        assert!(
            s.active_delegations.contains_key("pt-1"),
            "running delegation binding must survive TurnComplete"
        );
        assert_eq!(
            s.to_snapshot().active_delegations.len(),
            1,
            "binding still on the snapshot a post-turn attach would receive"
        );
    }

    #[test]
    fn delegation_completed_removes_entry() {
        // Completed delegations are NOT retained here — their terminal state is
        // recovered from the child's persisted DB row (inject_delegation_meta)
        // and the live DelegationProvider binding, not from this in-flight set.
        let mut s = fresh_state();
        s.apply_event(&delegation_started("pt-1", 99));
        assert!(s.active_delegations.contains_key("pt-1"));
        s.apply_event(&delegation_completed("pt-1", 99));
        assert!(
            !s.active_delegations.contains_key("pt-1"),
            "completed delegation removed from the in-flight set"
        );
        assert!(s.to_snapshot().active_delegations.is_empty());
    }

    #[test]
    fn delegation_completed_without_started_is_noop() {
        // A stream that only delivered the completion (started never observed on
        // this connection) must not synthesize a phantom entry: removing an
        // absent key is a no-op, and there is no running child to bind.
        let mut s = fresh_state();
        s.apply_event(&delegation_completed("pt-unknown", 7));
        assert!(s.active_delegations.is_empty());
    }

    #[test]
    fn active_delegations_unbounded_by_running_fanout() {
        // No cap: a parent fanning out far past any old soft bound keeps every
        // running binding (size tracks live concurrency, not an artificial
        // limit). Completing them drains the set back to empty.
        let mut s = fresh_state();
        let n: i32 = 200;
        for i in 0..n {
            s.apply_event(&delegation_started(&format!("pt-{i}"), 1000 + i));
        }
        assert_eq!(s.active_delegations.len(), n as usize);
        assert_eq!(s.to_snapshot().active_delegations.len(), n as usize);
        for i in 0..n {
            s.apply_event(&delegation_completed(&format!("pt-{i}"), 1000 + i));
        }
        assert!(s.active_delegations.is_empty());
    }

    #[test]
    fn delegation_binding_survives_snapshot_split_like_live() {
        // Path A (live): apply started + completed straight through.
        // Path B (reconnect): apply started, snapshot round-trip mid-flight,
        // then apply completed. Both must converge — proving a running
        // delegation recovered from the snapshot ends identically to one tracked
        // live. This is the exact web-attach path the original bug broke.
        let mut a = fresh_state();
        a.apply_event(&delegation_started("tc-1", 99));
        a.apply_event(&delegation_completed("tc-1", 99));

        let mut b = fresh_state();
        b.apply_event(&delegation_started("tc-1", 99));
        // Snapshot round-trip while the child is still running: the running
        // binding must ride along on the wire shape the web client hydrates from.
        let snap = b.to_snapshot();
        assert_eq!(snap.active_delegations.len(), 1);
        assert_eq!(snap.active_delegations[0].parent_tool_use_id, "tc-1");
        let wire = serde_json::to_string(&snap).unwrap();
        let _back: LiveSessionSnapshot = serde_json::from_str(&wire).unwrap();
        b.apply_event(&delegation_completed("tc-1", 99));

        assert_eq!(
            serde_json::to_value(a.to_snapshot().active_delegations).unwrap(),
            serde_json::to_value(b.to_snapshot().active_delegations).unwrap(),
            "snapshot-recovered delegation must match the live-tracked one"
        );
    }

    #[test]
    fn turn_complete_captures_only_trailing_text_block() {
        // last_assistant_text (the delegation result text surfaced by
        // get_delegation_status) keeps only the final text run — the answer
        // after the last tool call — not intermediate narration.
        let mut s = fresh_state();
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "let me check ".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "tc".into(),
                },
                LiveContentBlock::Text {
                    text: "the answer is 42".into(),
                    parent_tool_use_id: None,
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("the answer is 42"));
    }

    #[test]
    fn turn_complete_no_tool_calls_captures_full_text() {
        // With no tool call to split on, the trailing run is the whole answer.
        let mut s = fresh_state();
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "part 1 ".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::Text {
                    text: "part 2".into(),
                    parent_tool_use_id: None,
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("part 1 part 2"));
    }

    #[test]
    fn turn_complete_trailing_tool_call_captures_no_text() {
        // A turn ending on a tool call has no concluding text block; the result
        // text stays unset (the LLM opens the child session for detail).
        let mut s = fresh_state();
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "running a tool".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "tc".into(),
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert_eq!(s.last_assistant_text, None);
    }

    #[test]
    fn turn_complete_keeps_final_text_before_a_trailing_plan_block() {
        // `PlanUpdate` re-appends a Plan block at the END of content, so the
        // agent's concluding answer often sits BEFORE a trailing Plan. The
        // result must still be the text after the last tool call, not empty.
        let mut s = fresh_state();
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "let me check".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "tc".into(),
                },
                LiveContentBlock::Text {
                    text: "the answer is 42".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::Plan {
                    entries: serde_json::json!([]),
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("the answer is 42"));
    }

    #[test]
    fn turn_complete_clears_stale_last_assistant_text() {
        // A turn that ends with no concluding text must CLEAR any prior value
        // rather than leak it as this turn's delegation result.
        let mut s = fresh_state();
        s.last_assistant_text = Some("stale text from an earlier turn".into());
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "working".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "tc".into(),
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert_eq!(s.last_assistant_text, None);
    }

    /// A turn that never opened a live message has no text of its own either.
    /// An agent whose only output was an empty text chunk reaches exactly that
    /// state (`append_text_delta` drops it), and the turn still ends on
    /// `end_turn` — so without clearing, `get_delegation_status` would hand the
    /// PREVIOUS turn's answer back as this turn's result.
    #[test]
    fn turn_complete_clears_stale_text_when_the_turn_produced_nothing() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.turn_in_flight = true;
        s.last_assistant_text = Some("stale text from an earlier turn".into());
        s.apply_event(&AcpEvent::ContentDelta {
            text: String::new(),
            parent_tool_use_id: None,
        });
        assert!(s.live_message.is_none(), "empty chunk opens no live message");
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
        });
        assert_eq!(s.last_assistant_text, None);
    }

    /// `TurnComplete` is emitted from three places, and the cancel path does
    /// not wait for the agent — so a second one can land on an already-settled
    /// turn. It must not wipe the text the first one captured.
    #[test]
    fn a_repeat_turn_complete_keeps_the_captured_text() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.turn_in_flight = true;
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![LiveContentBlock::Text {
                text: "the answer".into(),
                parent_tool_use_id: None,
            }],
            started_at: Utc::now(),
        });
        let complete = AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "cancelled".into(),
            agent_type: "codex".into(),
        };
        s.apply_event(&complete);
        assert_eq!(s.last_assistant_text.as_deref(), Some("the answer"));
        s.apply_event(&complete);
        assert_eq!(
            s.last_assistant_text.as_deref(),
            Some("the answer"),
            "the agent's late response must not erase the captured result"
        );
    }

    #[test]
    fn permission_resolved_clears_matching_request() {
        // Mirrors the pet snapshot semantics: when the user (or auto-approve)
        // responds, the snapshot's pending_permission must drop *before*
        // TurnComplete, otherwise a snapshot-recovering frontend (WS attach
        // after a refresh) would re-render a dialog the user has already
        // answered.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-1".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-1"}),
            options: vec![],
            queued: 0,
        });
        assert!(s.pending_permission.is_some());

        s.apply_event(&AcpEvent::PermissionResolved {
            request_id: "p-1".into(),
        });
        assert!(
            s.pending_permission.is_none(),
            "matching PermissionResolved must clear the pending permission"
        );
    }

    #[test]
    fn permission_queue_depth_updates_the_visible_card_only() {
        // A request arriving behind the visible card publishes no
        // `PermissionRequest` of its own, so the depth-only event is what keeps
        // the card's "N more waiting" hint from going stale — including for a
        // client that attaches mid-turn and hydrates from this snapshot.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-1".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-1"}),
            options: vec![],
            queued: 0,
        });
        s.apply_event(&AcpEvent::PermissionQueueDepth { depth: 2 });
        let p = s.pending_permission.as_ref().expect("card still up");
        assert_eq!(p.queued, 2);
        assert_eq!(p.request_id, "p-1", "depth must not change which card is up");
    }

    #[test]
    fn permission_queue_depth_without_a_card_is_a_noop() {
        // A depth event that lands after a drain must not resurrect a card.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PermissionQueueDepth { depth: 3 });
        assert!(s.pending_permission.is_none());
    }

    #[test]
    fn permission_resolved_stale_request_is_noop() {
        // A late `PermissionResolved` for an already-replaced request must
        // not wipe out the *new* outstanding permission — id mismatch is
        // the only thing distinguishing the two, since the snapshot only
        // tracks one pending permission at a time.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-2".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-2"}),
            options: vec![],
            queued: 0,
        });

        s.apply_event(&AcpEvent::PermissionResolved {
            request_id: "p-stale".into(),
        });
        let p = s
            .pending_permission
            .as_ref()
            .expect("stale PermissionResolved must not clear a non-matching pending permission");
        assert_eq!(p.request_id, "p-2");
    }

    #[test]
    fn permission_request_preserves_full_tool_call_value() {
        let mut s = fresh_state();
        // Realistic permission payload: title + kind + rawInput (used by the
        // frontend's permission parser to extract command / diff / plan).
        // After the refresh-survives-permission fix, all of this must round
        // trip via the snapshot — losing rawInput would force the user to
        // approve blind.
        let raw_tool_call = serde_json::json!({
            "toolCallId": "tc-9",
            "title": "Run rm -rf /",
            "kind": "execute",
            "rawInput": { "command": "rm -rf /" },
            "locations": [{ "path": "/", "line": 1 }],
        });
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-1".into(),
            tool_call: raw_tool_call.clone(),
            options: vec![],
            queued: 0,
        });
        let p = s.pending_permission.as_ref().expect("permission set");
        assert_eq!(p.request_id, "p-1");
        assert_eq!(p.tool_call_id, "tc-9");
        assert_eq!(
            p.tool_call, raw_tool_call,
            "full tool_call JSON must round-trip into PendingPermissionState"
        );

        // Snapshot round-trip preserves it byte-for-byte (the load-bearing
        // property — frontend re-renders the approval dialog from this).
        let snap = s.to_snapshot();
        let snap_perm = snap.pending_permission.as_ref().unwrap();
        assert_eq!(snap_perm.tool_call, raw_tool_call);
    }

    #[test]
    fn mode_changed_updates_current_mode_and_session_modes_seeds_state() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::SessionModes {
            modes: SessionModeStateInfo {
                current_mode_id: "default".into(),
                available_modes: vec![SessionModeInfo {
                    id: "default".into(),
                    name: "Default".into(),
                    description: None,
                }],
            },
        });
        assert_eq!(s.current_mode.as_deref(), Some("default"));
        assert!(s.modes.is_some());
        s.apply_event(&AcpEvent::ModeChanged {
            mode_id: "edit".into(),
        });
        assert_eq!(s.current_mode.as_deref(), Some("edit"));
        // Snapshot consistency invariant: ModeChanged must keep
        // `modes.current_mode_id` in sync with the scalar `current_mode`.
        // The frontend's `denormalizeSnapshot` reads `modes.current_mode_id`
        // exclusively; without this sync a post-refresh hydration would
        // show the stale default even though the live event stream had
        // long since switched modes.
        assert_eq!(
            s.modes.as_ref().unwrap().current_mode_id,
            "edit",
            "ModeChanged must keep modes.current_mode_id consistent for snapshot consumers"
        );
    }

    #[test]
    fn snapshot_excludes_internal_chunk_buffers_and_carries_negotiated_caps() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PromptCapabilities {
            prompt_capabilities: PromptCapabilitiesInfo {
                image: true,
                audio: false,
                embedded_context: true,
            },
        });
        s.apply_event(&AcpEvent::ForkSupported { supported: true });
        s.apply_event(&AcpEvent::SessionConfigOptions {
            config_options: vec![SessionConfigOptionInfo {
                id: "model".into(),
                name: "Model".into(),
                description: None,
                category: None,
                kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                    current_value: "sonnet".into(),
                    options: vec![],
                    groups: vec![],
                }),
                recommended_value: None,
            }],
        });
        s.apply_event(&AcpEvent::UsageUpdate {
            used: 1234,
            size: 200_000,
        });
        // Two raw_input fragments; the second is a complete JSON object
        // and should overwrite `entry.input` with the parsed value.
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "edit".into(),
            kind: "edit".into(),
            status: "pending".into(),
            content: None,
            raw_input: Some("{\"a\":".into()),
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: None,
            content: None,
            raw_input: Some("{\"a\":1}".into()),
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        assert_eq!(entry.input, Some(serde_json::json!({"a": 1})));
        assert_eq!(entry.raw_input_chunks.len(), 2);

        let snapshot = s.to_snapshot();
        assert_eq!(snapshot.connection_id, "conn-test");
        assert!(snapshot.fork_supported);
        assert_eq!(
            snapshot.usage,
            Some(UsageInfo {
                used: 1234,
                size: 200_000,
            })
        );
        assert!(snapshot.prompt_capabilities.is_some());
        assert_eq!(snapshot.config_options.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(snapshot.active_tool_calls.len(), 1);

        // Wire shape: raw_input_chunks must NOT be serialized.
        let json = serde_json::to_value(&snapshot).unwrap();
        let tc_json = json["active_tool_calls"][0].clone();
        assert!(
            tc_json.get("raw_input_chunks").is_none(),
            "raw_input_chunks must be #[serde(skip)] (got {})",
            tc_json
        );
        assert_eq!(tc_json["input"], serde_json::json!({"a": 1}));
    }

    fn scripted_event_sequence() -> Vec<AcpEvent> {
        vec![
            AcpEvent::SessionStarted {
                session_id: "ext-1".into(),
            },
            AcpEvent::ContentDelta {
                text: "Hello ".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::ContentDelta {
                text: "world".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::ToolCall {
                tool_call_id: "tc-1".into(),
                title: "ls".into(),
                kind: "execute".into(),
                status: "pending".into(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
            AcpEvent::ToolCallUpdate {
                tool_call_id: "tc-1".into(),
                title: None,
                status: Some("completed".into()),
                content: None,
                raw_input: None,
                raw_output: Some("\"done\"".into()),
                raw_output_append: None,
                locations: None,
                meta: None,
                images: None,
            },
            AcpEvent::Thinking {
                text: "considering".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::ContentDelta {
                text: " More text".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::UsageUpdate {
                used: 1234,
                size: 200_000,
            },
        ]
    }

    #[test]
    fn full_turn_lifecycle_increments_seq_monotonically() {
        let mut s = fresh_state();
        let events = scripted_event_sequence();
        let mut seq = 0u64;
        for e in &events {
            s.apply_event(e);
            seq += 1;
            s.event_seq = seq;
        }
        assert_eq!(s.event_seq, events.len() as u64);
    }

    /// Strip volatile fields that legitimately differ between Path A and Path B
    /// (e.g. `LiveMessage.id` is generated via `uuid::new_v4()` and `started_at`
    /// uses `Utc::now()`) but don't matter for snapshot/live consistency.
    fn normalize_snapshot(snap: &LiveSessionSnapshot) -> serde_json::Value {
        let mut v = serde_json::to_value(snap).unwrap();
        if let Some(lm) = v.get_mut("live_message") {
            if let Some(obj) = lm.as_object_mut() {
                obj.remove("id");
                obj.remove("started_at");
            }
        }
        v
    }

    /// 对账测试：从初始状态全程 apply 到 N 个事件 == 从 snapshot
    /// (apply 完前 K 个) + apply 剩下 N-K 个事件，最终状态等价。
    #[test]
    fn snapshot_filtered_events_yield_same_state_as_live_subscriber() {
        let events = scripted_event_sequence();
        let split = events.len() / 2;

        // Path A: live subscriber——全程 apply
        let mut a = fresh_state();
        for (i, e) in events.iter().enumerate() {
            a.apply_event(e);
            a.event_seq = (i + 1) as u64;
        }

        // Path B: snapshot 重连
        // 1) apply 前 split 个事件
        let mut b = fresh_state();
        for (i, e) in events.iter().take(split).enumerate() {
            b.apply_event(e);
            b.event_seq = (i + 1) as u64;
        }
        // 2) snapshot round-trip 通过 JSON
        let snapshot = b.to_snapshot();
        let _wire = serde_json::to_string(&snapshot).unwrap();
        // 3) 继续 apply 剩下事件
        for (i, e) in events.iter().enumerate().skip(split) {
            b.apply_event(e);
            b.event_seq = (i + 1) as u64;
        }

        let snap_a = a.to_snapshot();
        let snap_b = b.to_snapshot();

        assert_eq!(snap_a.event_seq, snap_b.event_seq);
        assert_eq!(snap_a.status, snap_b.status);
        assert_eq!(snap_a.external_id, snap_b.external_id);
        assert_eq!(snap_a.usage, snap_b.usage);

        // Full structural equivalence (with volatile fields stripped + tool
        // calls sorted by id). This is the load-bearing consistency check.
        assert_eq!(normalize_snapshot(&snap_a), normalize_snapshot(&snap_b));
    }

    // ---------- Phase 3c-3: snapshot fidelity ----------

    /// Helper: returns the kind discriminator + payload-id of each block in
    /// `live_message.content`, suitable for asserting block ordering.
    fn live_block_summary(s: &SessionState) -> Vec<(&'static str, String)> {
        s.live_message
            .as_ref()
            .map(|lm| {
                lm.content
                    .iter()
                    .map(|b| match b {
                        LiveContentBlock::Text { text, .. } => ("text", text.clone()),
                        LiveContentBlock::Thinking { text, .. } => ("thinking", text.clone()),
                        LiveContentBlock::ToolCallRef { tool_call_id } => {
                            ("tool_call_ref", tool_call_id.clone())
                        }
                        LiveContentBlock::Plan { entries } => {
                            ("plan", serde_json::to_string(entries).unwrap_or_default())
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn tool_call_event(id: &str, title: &str) -> AcpEvent {
        AcpEvent::ToolCall {
            tool_call_id: id.into(),
            title: title.into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        }
    }

    #[test]
    fn tool_call_pushes_ref_block_at_current_position() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "before ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&tool_call_event("tc-1", "ls"));
        s.apply_event(&AcpEvent::ContentDelta {
            text: "between".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&tool_call_event("tc-2", "pwd"));

        let summary = live_block_summary(&s);
        assert_eq!(
            summary,
            vec![
                ("text", "before ".to_string()),
                ("tool_call_ref", "tc-1".to_string()),
                ("text", "between".to_string()),
                ("tool_call_ref", "tc-2".to_string()),
            ],
            "tool-call refs must anchor at the position they arrived in the stream"
        );
    }

    #[test]
    fn tool_call_ref_push_is_idempotent() {
        let mut s = fresh_state();
        s.apply_event(&tool_call_event("tc-1", "ls"));
        // Defensive: second ToolCall with the same id (replay/unusual ordering)
        // must NOT push a duplicate ref block.
        s.apply_event(&tool_call_event("tc-1", "ls (retry)"));

        let summary = live_block_summary(&s);
        let ref_count = summary
            .iter()
            .filter(|(kind, id)| *kind == "tool_call_ref" && id == "tc-1")
            .count();
        assert_eq!(ref_count, 1, "duplicate ToolCall must not duplicate ref");
    }

    #[test]
    fn tool_call_update_does_not_duplicate_ref() {
        let mut s = fresh_state();
        s.apply_event(&tool_call_event("tc-1", "ls"));
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: Some("\"done\"".into()),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });

        let summary = live_block_summary(&s);
        let ref_count = summary
            .iter()
            .filter(|(kind, id)| *kind == "tool_call_ref" && id == "tc-1")
            .count();
        assert_eq!(
            ref_count, 1,
            "ToolCall + ToolCallUpdate for same id yields exactly one ref"
        );
    }

    #[test]
    fn tool_call_state_carries_locations_and_meta() {
        let mut s = fresh_state();
        let locs = serde_json::json!([{ "path": "/tmp/foo.rs", "line": 12 }]);
        let meta = serde_json::json!({ "parent_tool_use_id": "abc", "session": "ext-1" });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "edit".into(),
            kind: "edit".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: Some(locs.clone()),
            meta: Some(meta.clone()),
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").expect("tc-1 inserted");
        assert_eq!(entry.locations.as_ref(), Some(&locs));
        assert_eq!(entry.meta.as_ref(), Some(&meta));

        // Snapshot round-trip preserves both.
        let snap = s.to_snapshot();
        let tc = snap
            .active_tool_calls
            .iter()
            .find(|t| t.id == "tc-1")
            .unwrap();
        assert_eq!(tc.locations.as_ref(), Some(&locs));
        assert_eq!(tc.meta.as_ref(), Some(&meta));
    }

    #[test]
    fn tool_call_update_preserves_locations_when_omitted() {
        let mut s = fresh_state();
        let locs = serde_json::json!([{ "path": "/tmp/foo.rs" }]);
        let meta = serde_json::json!({ "k": "v" });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "edit".into(),
            kind: "edit".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: Some(locs.clone()),
            meta: Some(meta.clone()),
            images: None,
        });
        // Subsequent partial update without locations/meta — must not clobber.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: Some("\"ok\"".into()),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        assert_eq!(entry.status, ToolCallStatus::Completed);
        assert_eq!(
            entry.locations.as_ref(),
            Some(&locs),
            "ToolCallUpdate without locations must NOT clobber previously-set value"
        );
        assert_eq!(
            entry.meta.as_ref(),
            Some(&meta),
            "ToolCallUpdate without meta must NOT clobber previously-set value"
        );
    }

    #[test]
    fn tool_call_images_replace_or_preserve_on_update() {
        let mut s = fresh_state();
        let img_v1 = ToolCallImageInfo {
            data: "AAAA".into(),
            mime_type: "image/png".into(),
            uri: Some("/tmp/v1.png".into()),
        };
        let img_v2 = ToolCallImageInfo {
            data: "BBBB".into(),
            mime_type: "image/jpeg".into(),
            uri: None,
        };

        // Initial ToolCall carries one image — should be persisted.
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "ig-1".into(),
            title: "Image generation".into(),
            kind: "other".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: Some(vec![img_v1.clone()]),
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert_eq!(entry.images.len(), 1);
        assert_eq!(entry.images[0].data, "AAAA");

        // Update without images field — must preserve prior images.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "ig-1".into(),
            title: None,
            status: Some("in_progress".into()),
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert_eq!(
            entry.images.len(),
            1,
            "ToolCallUpdate with images=None must preserve prior images"
        );
        assert_eq!(entry.images[0].data, "AAAA");

        // Update with Some(new_vec) — must replace.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "ig-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: Some(vec![img_v2.clone()]),
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert_eq!(entry.images.len(), 1, "Some(vec) replaces prior images");
        assert_eq!(entry.images[0].data, "BBBB");
        assert_eq!(entry.images[0].mime_type, "image/jpeg");
        assert!(entry.images[0].uri.is_none());

        // Snapshot round-trip preserves images.
        let snap = s.to_snapshot();
        let tc = snap
            .active_tool_calls
            .iter()
            .find(|t| t.id == "ig-1")
            .unwrap();
        assert_eq!(tc.images.len(), 1);
        assert_eq!(tc.images[0].data, "BBBB");

        // Update with Some(empty) — must clear images (allows the agent to
        // explicitly drop a prior image if needed).
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "ig-1".into(),
            title: None,
            status: None,
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: Some(vec![]),
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert!(
            entry.images.is_empty(),
            "Some(empty vec) clears prior images"
        );
    }

    #[test]
    fn plan_update_appends_at_end_replacing_existing() {
        use crate::acp::types::PlanEntryInfo;
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta { text: "A".into(), parent_tool_use_id: None });
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "step v1".into(),
                priority: "high".into(),
                status: "pending".into(),
            }],
        });
        s.apply_event(&AcpEvent::ContentDelta { text: "B".into(), parent_tool_use_id: None });
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "step v2".into(),
                priority: "high".into(),
                status: "in_progress".into(),
            }],
        });

        let summary = live_block_summary(&s);
        // Expect: text("A"), text("B"), plan(v2). The old plan block is
        // removed and the fresh one is appended at end (after all current
        // text), matching the frontend reducer's replace-then-append.
        assert_eq!(summary.len(), 3, "summary was: {:?}", summary);
        assert_eq!(summary[0], ("text", "A".to_string()));
        assert_eq!(summary[1], ("text", "B".to_string()));
        assert_eq!(summary[2].0, "plan");
        assert!(
            summary[2].1.contains("step v2"),
            "plan block must be the v2 entries, not v1; got: {}",
            summary[2].1
        );
        assert!(
            !summary[2].1.contains("step v1"),
            "old plan block must be removed; got: {}",
            summary[2].1
        );
    }

    #[test]
    fn plan_update_creates_live_message_when_absent() {
        use crate::acp::types::PlanEntryInfo;
        let mut s = fresh_state();
        assert!(s.live_message.is_none());
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "first step".into(),
                priority: "medium".into(),
                status: "pending".into(),
            }],
        });
        let live = s
            .live_message
            .as_ref()
            .expect("PlanUpdate must lazily create live_message");
        assert_eq!(live.content.len(), 1);
        match &live.content[0] {
            LiveContentBlock::Plan { entries } => {
                assert!(
                    entries.to_string().contains("first step"),
                    "plan must carry the entries payload; got: {}",
                    entries
                );
            }
            other => panic!("expected Plan block, got {:?}", other),
        }
    }

    #[test]
    fn turn_complete_clears_plan_and_tool_refs() {
        use crate::acp::types::PlanEntryInfo;
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta { text: "x".into(), parent_tool_use_id: None });
        s.apply_event(&tool_call_event("tc-1", "ls"));
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "step".into(),
                priority: "low".into(),
                status: "pending".into(),
            }],
        });
        // Sanity precondition: live now has text, ref, plan.
        assert_eq!(live_block_summary(&s).len(), 3);
        assert_eq!(s.active_tool_calls.len(), 1);

        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        });
        // The existing `live_message = None` clear handles the new block kinds
        // automatically — they live inside live_message, not as siblings.
        assert!(s.live_message.is_none());
        assert!(s.active_tool_calls.is_empty());
    }

    /// 验证 envelope 序列化 + 反序列化 round-trip
    #[test]
    fn event_envelope_round_trips_through_json() {
        let env = EventEnvelope {
            seq: 7,
            connection_id: "conn-x".into(),
            payload: AcpEvent::ContentDelta { text: "abc".into(), parent_tool_use_id: None },
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seq, 7);
        assert_eq!(back.connection_id, "conn-x");
        match back.payload {
            AcpEvent::ContentDelta { text, .. } => assert_eq!(text, "abc"),
            _ => panic!("expected ContentDelta"),
        }
    }

    // --- live feedback: apply_event + snapshot --------------------------

    fn feedback_note(id: &str, text: &str) -> FeedbackItem {
        FeedbackItem::new_pending(id.into(), text.into(), Utc::now())
    }

    #[test]
    fn feedback_submitted_appends_idempotently() {
        let mut s = fresh_state();
        let item = feedback_note("f1", "use UserService");
        s.apply_event(&AcpEvent::FeedbackSubmitted { item: item.clone() });
        assert_eq!(s.feedback.len(), 1);
        // Replay / double-attach: a second apply with the same id is a no-op.
        s.apply_event(&AcpEvent::FeedbackSubmitted { item });
        assert_eq!(s.feedback.len(), 1, "duplicate id must not append twice");
        assert_eq!(s.feedback[0].status, FeedbackStatus::Pending);
        // A different id appends.
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f2", "skip the migration"),
        });
        assert_eq!(s.feedback.len(), 2);
    }

    #[test]
    fn feedback_consumed_marks_named_notes_delivered() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f1", "a"),
        });
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f2", "b"),
        });
        let at = Utc::now();
        s.apply_event(&AcpEvent::FeedbackConsumed {
            ids: vec!["f1".into()],
            delivered_at: at,
        });
        let f1 = s.feedback.iter().find(|f| f.id == "f1").unwrap();
        let f2 = s.feedback.iter().find(|f| f.id == "f2").unwrap();
        assert_eq!(f1.status, FeedbackStatus::Delivered);
        assert_eq!(f1.delivered_at, Some(at));
        assert_eq!(f2.status, FeedbackStatus::Pending, "unnamed note untouched");
        // Idempotent: re-applying the same consumption leaves f1 delivered and
        // does not flip its delivered_at to a new instant.
        s.apply_event(&AcpEvent::FeedbackConsumed {
            ids: vec!["f1".into()],
            delivered_at: Utc::now(),
        });
        let f1 = s.feedback.iter().find(|f| f.id == "f1").unwrap();
        assert_eq!(f1.delivered_at, Some(at), "delivered_at must not change");
    }

    #[test]
    fn user_message_clears_feedback_for_new_turn() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f1", "a"),
        });
        assert_eq!(s.feedback.len(), 1);
        // A new turn's user prompt resets the turn-scoped feedback set.
        s.apply_event(&text_user_message("user-1", "next prompt"));
        assert!(
            s.feedback.is_empty(),
            "feedback is turn-scoped; a new user_message clears it"
        );
    }

    #[test]
    fn snapshot_carries_feedback_and_omits_when_empty() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f1", "snapshot me"),
        });
        let snap = s.to_snapshot();
        assert_eq!(snap.feedback.len(), 1);
        assert_eq!(snap.feedback[0].id, "f1");
        // Round-trips through the wire shape the web client hydrates from.
        let json = serde_json::to_string(&snap).unwrap();
        let back: LiveSessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.feedback.len(), 1);
        // The empty case keeps the NOTES array off the wire (the always-present
        // `feedback_tool_available` bool is a separate field).
        let empty = serde_json::to_string(&fresh_state().to_snapshot()).unwrap();
        assert!(
            !empty.contains("\"feedback\":"),
            "no-feedback snapshot must omit the notes array"
        );
    }
