    use super::*;

    fn ctx() -> CompanionContext {
        // Delegation-only by default so the existing delegation-focused tests
        // keep seeing exactly the three delegation tools.
        ctx_with(CompanionFeatures {
            delegation: true,
            feedback: false,
            ask: false,
            sessions: false,
            tasks: false,
            automations: false,
            taskboard: false,
            browser: false,
            browser_eval: false,
            computer: false,
            computer_launch: false,
            computer_clipboard: false,
        })
    }

    fn ctx_with(features: CompanionFeatures) -> CompanionContext {
        CompanionContext {
            parent_connection_id: "p1".into(),
            socket_path: "/tmp/codeg-mcp-companion-test-nope.sock".into(),
            token: "tok".into(),
            features,
            custom_agents: Vec::new(),
            disabled_agents: Vec::new(),
        }
    }

    async fn dispatch_for_test(line: &str) -> LineAction {
        dispatch_line(&ctx(), Arc::new(InflightCalls::new()), line).await
    }

    async fn dispatch_with_features(features: CompanionFeatures, line: &str) -> LineAction {
        dispatch_line(&ctx_with(features), Arc::new(InflightCalls::new()), line).await
    }

    fn unwrap_respond(action: LineAction) -> JsonRpcResponse {
        match action {
            LineAction::Respond(r) => r,
            LineAction::Spawn(_) => panic!("expected Respond, got Spawn"),
            LineAction::Silent => panic!("expected Respond, got Silent"),
        }
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "codeg-mcp");
    }

    #[tokio::test]
    async fn tools_list_returns_four_delegation_tools() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"delegate_to_agent"));
        assert!(names.contains(&"get_delegation_status"));
        assert!(names.contains(&"cancel_delegation"));
        assert!(names.contains(&"resume_delegation"));
        // resume_delegation requires only task_id; reason is optional and
        // there is deliberately NO task-text parameter (no new iterations).
        let resume = tools
            .iter()
            .find(|t| t["name"] == "resume_delegation")
            .unwrap();
        assert!(resume["inputSchema"]["properties"]["task_id"].is_object());
        assert!(resume["inputSchema"]["properties"]["reason"].is_object());
        assert!(resume["inputSchema"]["properties"]["task"].is_null());
        let required = resume["inputSchema"]["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert!(required.iter().any(|v| v == "task_id"));
        // delegate_to_agent schema still enumerates all 13 agent types.
        let delegate = tools
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .unwrap();
        let agents = delegate["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(agents.len(), 15);
        assert!(agents.iter().any(|a| a == "hermes"));
        assert!(agents.iter().any(|a| a == "code_buddy"));
        assert!(agents.iter().any(|a| a == "kimi_code"));
        assert!(agents.iter().any(|a| a == "pi"));
        assert!(agents.iter().any(|a| a == "grok"));
        assert!(agents.iter().any(|a| a == "cursor"));
        assert!(agents.iter().any(|a| a == "deepseek"));
        assert!(agents.iter().any(|a| a == "qoder"));
        assert!(agents.iter().any(|a| a == "antigravity"));
        // get_delegation_status takes a single id param — task_ids (required) —
        // plus wait_ms. The legacy single `task_id` param is gone.
        let status = tools
            .iter()
            .find(|t| t["name"] == "get_delegation_status")
            .unwrap();
        assert!(status["inputSchema"]["properties"]["task_id"].is_null());
        assert!(status["inputSchema"]["properties"]["task_ids"].is_object());
        assert!(status["inputSchema"]["properties"]["wait_ms"].is_object());
        let required = status["inputSchema"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "task_ids"));
    }

    #[tokio::test]
    async fn custom_agents_extend_the_delegate_enum_after_the_builtins() {
        let mut ctx = ctx();
        // The duplicate and the already-built-in slug exercise the de-dup: a
        // parent that double-sends (or somehow lists a builtin) must not
        // produce a corrupted enum.
        ctx.custom_agents = vec![
            "custom:goose".into(),
            "custom:amp".into(),
            "custom:goose".into(),
            "codex".into(),
        ];
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = unwrap_respond(dispatch_line(&ctx, Arc::new(InflightCalls::new()), line).await);
        let tools = resp.result.unwrap()["tools"].clone();
        let delegate = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .cloned()
            .unwrap();
        let agents = delegate["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(agents.len(), 17, "15 builtins + 2 distinct customs");
        // Builtins keep the embedded order and come first.
        assert_eq!(agents[0], "claude_code");
        assert_eq!(agents[15], "custom:goose");
        assert_eq!(agents[16], "custom:amp");
        // The other delegation tools carry no agent_type and are untouched.
        let status = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "get_delegation_status")
            .unwrap();
        assert!(status["inputSchema"]["properties"]["agent_type"].is_null());
    }

    // Disabled builtins are subtracted from the enum (the user's settings
    // toggles must narrow the advertised targets), the remaining builtins keep
    // the embedded order, enabled customs still append after them, and a slug
    // the embedded list doesn't know is a harmless no-op — version skew can
    // narrow the enum, never corrupt it.
    #[tokio::test]
    async fn disabled_agents_are_subtracted_from_the_delegate_enum() {
        let mut ctx = ctx();
        ctx.disabled_agents = vec!["codex".into(), "grok".into(), "not-an-agent".into()];
        ctx.custom_agents = vec!["custom:goose".into()];
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = unwrap_respond(dispatch_line(&ctx, Arc::new(InflightCalls::new()), line).await);
        let tools = resp.result.unwrap()["tools"].clone();
        let delegate = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .cloned()
            .unwrap();
        let agents = delegate["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(agents.len(), 14, "15 builtins - 2 disabled + 1 custom");
        assert!(!agents.contains(&serde_json::json!("codex")));
        assert!(!agents.contains(&serde_json::json!("grok")));
        // Survivors keep the embedded order, customs still come last.
        assert_eq!(agents[0], "claude_code");
        assert_eq!(agents[1], "open_code");
        assert_eq!(agents[13], "custom:goose");
    }

    // An empty disabled list (the parent omitted `--disabled-agents`) leaves
    // the schema byte-identical to the embedded builtin set — the exact
    // behavior every pre-flag parent relies on.
    #[tokio::test]
    async fn empty_disabled_list_serves_the_embedded_builtin_enum_unchanged() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let tools = resp.result.unwrap()["tools"].clone();
        let delegate = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "delegate_to_agent")
            .cloned()
            .unwrap();
        let agents = delegate["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(agents.len(), 15);
        assert_eq!(agents[0], "claude_code");
        assert_eq!(agents[14], "antigravity");
    }

    #[tokio::test]
    async fn get_delegation_status_without_task_ids_rejected() {
        let line = r#"{
            "jsonrpc":"2.0",
            "id":11,
            "method":"tools/call",
            "params": { "name": "get_delegation_status", "arguments": {} }
        }"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("task_ids"));
    }

    #[tokio::test]
    async fn notifications_initialized_produces_no_response() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let action = dispatch_for_test(line).await;
        assert!(matches!(action, LineAction::Silent));
    }

    #[tokio::test]
    async fn parse_error_returns_null_id_error() {
        let line = "not json";
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32700);
        assert!(e.message.contains("parse"));
        assert_eq!(resp.id, Value::Null);
    }

    #[tokio::test]
    async fn unknown_method_returns_32601() {
        let line = r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32601);
    }

    #[tokio::test]
    async fn tools_call_with_unknown_tool_rejected_synchronously() {
        let line = r#"{
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params": {
                "name": "other_tool",
                "arguments": {},
                "_meta": {"tool_use_id": "tu1"}
            }
        }"#;
        let resp = unwrap_respond(dispatch_for_test(line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("other_tool"));
    }

    #[tokio::test]
    async fn tools_call_registers_inflight_and_returns_spawn() {
        let inflight = Arc::new(InflightCalls::new());
        let line = r#"{
            "jsonrpc":"2.0",
            "id":4,
            "method":"tools/call",
            "params": {
                "name": "delegate_to_agent",
                "arguments": {"agent_type": "codex", "task": "x"}
            }
        }"#;
        let action = dispatch_line(&ctx(), inflight.clone(), line).await;
        match action {
            LineAction::Spawn(call) => {
                assert_eq!(call.request_id_key, request_id_key(&Value::from(4)));
            }
            _ => panic!("expected Spawn"),
        }
        // The inflight registry should now have an entry for id=4.
        let map = inflight.inner.lock().await;
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&request_id_key(&Value::from(4))));
    }

    #[tokio::test]
    async fn cancel_notification_fires_inflight_cancel_channel() {
        let inflight = Arc::new(InflightCalls::new());
        // Pre-seed an inflight entry with a known cancel_tx; verify the
        // notification handler trips it.
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        inflight
            .register(
                request_id_key(&Value::from(7)),
                InflightEntry {
                    external_handle: Some("h-7".into()),
                    cancel_tx,
                },
            )
            .await;

        let line = r#"{
            "jsonrpc":"2.0",
            "method":"notifications/cancelled",
            "params": {"requestId": 7, "reason": "user requested"}
        }"#;
        let action = dispatch_line(&ctx(), inflight.clone(), line).await;
        assert!(matches!(action, LineAction::Silent));
        // The cancel channel should now be tripped (best-effort
        // `client_cancel` to a bogus socket failed silently — that's fine).
        assert!(cancel_rx.try_recv().is_ok());
        // Entry has been pulled.
        let map = inflight.inner.lock().await;
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn cancel_for_unknown_request_id_is_silent_noop() {
        let inflight = Arc::new(InflightCalls::new());
        let line = r#"{
            "jsonrpc":"2.0",
            "method":"notifications/cancelled",
            "params": {"requestId": 999}
        }"#;
        let action = dispatch_line(&ctx(), inflight.clone(), line).await;
        assert!(matches!(action, LineAction::Silent));
        assert!(inflight.inner.lock().await.is_empty());
    }

    #[test]
    fn render_task_report_running_ack_is_not_error() {
        let report = json!({
            "task_id": "t1",
            "status": "running",
            "child_conversation_id": 42,
            "message": "running in background"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["content"][0]["text"], "running in background");
        assert_eq!(rendered["structuredContent"]["status"], "running");
        assert_eq!(rendered["structuredContent"]["child_conversation_id"], 42);
    }

    #[test]
    fn render_task_report_completed_surfaces_text() {
        let report = json!({
            "task_id": "t1",
            "status": "completed",
            "child_conversation_id": 42,
            "text": "the result"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["content"][0]["text"], "the result");
        assert_eq!(rendered["structuredContent"]["status"], "completed");
    }

    #[test]
    fn render_task_report_failed_is_error() {
        let report = json!({
            "status": "failed",
            "error_code": "spawn_failed",
            "message": "spawn failed: agent missing"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], true);
        assert_eq!(
            rendered["content"][0]["text"],
            "spawn failed: agent missing"
        );
        assert_eq!(rendered["structuredContent"]["error_code"], "spawn_failed");
    }

    #[test]
    fn render_task_report_canceled_is_not_error() {
        // A successful cancel (or a canceled task) is a valid result, not an
        // error the LLM should treat as a failure.
        let report = json!({
            "task_id": "t1",
            "status": "canceled",
            "error_code": "canceled",
            "message": "canceled: canceled by request"
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["structuredContent"]["status"], "canceled");
    }

    #[test]
    fn render_task_report_completed_without_text_falls_back_to_message() {
        // DB-fallback for an evicted completed result: status completed, no
        // text, only a message. The content must not be empty.
        let report = json!({
            "task_id": "t1",
            "status": "completed",
            "child_conversation_id": 7,
            "message": "Result no longer cached; open child session 7 for the full output."
        });
        let rendered = render_task_report(&report);
        assert_eq!(rendered["isError"], false);
        assert_eq!(
            rendered["content"][0]["text"],
            "Result no longer cached; open child session 7 for the full output."
        );
    }

    // -- Batch get_delegation_status normalization + rendering -------------

    #[tokio::test]
    async fn get_delegation_status_bare_task_id_now_rejected() {
        // The legacy single `task_id` param is gone: a bare `{task_id}` no longer
        // resolves to a poll — it's an empty task set and must be rejected,
        // steering the caller to `task_ids`.
        let line = json!({
            "jsonrpc": "2.0", "id": 20, "method": "tools/call",
            "params": { "name": "get_delegation_status", "arguments": { "task_id": "abc" } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("task_ids"));
    }

    #[tokio::test]
    async fn get_delegation_status_accepts_task_ids_array() {
        let line = json!({
            "jsonrpc": "2.0", "id": 21, "method": "tools/call",
            "params": { "name": "get_delegation_status", "arguments": { "task_ids": ["a", "b"] } }
        })
        .to_string();
        assert!(matches!(
            dispatch_for_test(&line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn get_delegation_status_empty_task_ids_rejected() {
        // An absent, empty, or all-whitespace array yields no usable ids.
        for args in [json!({ "task_ids": [] }), json!({ "task_ids": ["  "] })] {
            let line = json!({
                "jsonrpc": "2.0", "id": 22, "method": "tools/call",
                "params": { "name": "get_delegation_status", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_for_test(&line).await);
            let e = resp.error.expect("empty task_ids must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("task_ids"));
        }
    }

    #[tokio::test]
    async fn get_delegation_status_non_string_task_id_rejected() {
        // A non-string entry violates the schema's `items: string` contract — the
        // whole call is rejected, NOT silently narrowed to the valid ids. Both a
        // lone non-string and a mixed `[123, "abc"]` must fail.
        for args in [
            json!({ "task_ids": [123] }),
            json!({ "task_ids": [123, "abc"] }),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 23, "method": "tools/call",
                "params": { "name": "get_delegation_status", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_for_test(&line).await);
            let e = resp
                .error
                .expect("non-string task_ids entry must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("task_ids"));
        }
    }

    #[test]
    fn normalize_status_task_ids_dedups_preserves_order() {
        // Trim each entry, drop "", collapse the duplicate "a", keep first-seen
        // order.
        let args = json!({ "task_ids": [" a ", "b", "a", "", "c"] });
        assert_eq!(
            normalize_status_task_ids(&args).unwrap(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn normalize_status_task_ids_rejects_non_string_entry() {
        // A non-string survivor alongside valid ids is a hard error, not a
        // silent drop.
        assert!(normalize_status_task_ids(&json!({ "task_ids": [123] })).is_err());
        assert!(normalize_status_task_ids(&json!({ "task_ids": ["a", 123] })).is_err());
        assert!(normalize_status_task_ids(&json!({ "task_ids": [true] })).is_err());
    }

    #[test]
    fn normalize_status_task_ids_empty_when_none_usable() {
        // Missing, empty, and all-blank arrays all yield no ids; a bare legacy
        // `task_id` is no longer read. (These are `Ok(empty)`, not errors.)
        assert!(normalize_status_task_ids(&json!({})).unwrap().is_empty());
        assert!(normalize_status_task_ids(&json!({ "task_ids": [] }))
            .unwrap()
            .is_empty());
        assert!(normalize_status_task_ids(&json!({ "task_ids": ["  "] }))
            .unwrap()
            .is_empty());
        assert!(normalize_status_task_ids(&json!({ "task_id": "abc" }))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn render_status_result_single_renders_as_one_element_batch() {
        // A single-id poll now renders through the SAME `{tasks:[..]}` envelope as
        // a fan-out (unified shape) — NOT the bare single-report path. The
        // structured batch carries the one task with its id + status, and the
        // content text is the `{tasks:[..]}` JSON (not the bare result text).
        let report = json!({
            "task_id": "t1", "status": "completed",
            "child_conversation_id": 42, "text": "the result"
        });
        let rendered = render_status_result(&json!({ "tasks": [report.clone()] }));
        let tasks = rendered["structuredContent"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["task_id"], "t1");
        assert_eq!(tasks[0]["status"], "completed");
        // Content text is the compact {tasks:[..]} JSON, recoverable by
        // content-only hosts — not the raw "the result" string.
        let text = rendered["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["tasks"][0]["text"], "the result");
        assert_eq!(rendered["isError"], false);
    }

    #[test]
    fn render_status_result_bare_report_wrapped_as_one_element_batch() {
        // Defensive: an outcome with no `tasks` array (older / unexpected shape) is
        // wrapped into a one-element batch so the output stays uniformly
        // `{tasks:[..]}`. A lone failed task flags `isError` (all-failed).
        let report = json!({
            "task_id": "t1", "status": "failed",
            "error_code": "spawn_failed", "message": "spawn failed"
        });
        let rendered = render_status_result(&report);
        let tasks = rendered["structuredContent"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["task_id"], "t1");
        assert_eq!(tasks[0]["status"], "failed");
        assert_eq!(rendered["isError"], true);
    }

    #[test]
    fn render_batch_report_carries_tasks_and_parseable_text() {
        let envelope = json!({ "tasks": [
            { "task_id": "t1", "status": "completed", "text": "r1" },
            { "task_id": "t2", "status": "running", "message": "Running." },
        ] });
        let rendered = render_status_result(&envelope);
        // structuredContent carries the whole batch.
        assert_eq!(
            rendered["structuredContent"]["tasks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // The content text is the compact {tasks:[..]} JSON, recoverable by hosts
        // that persist only CallToolResult.content text (e.g. Claude Code).
        let text = rendered["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["tasks"][0]["task_id"], "t1");
        assert_eq!(parsed["tasks"][1]["status"], "running");
        // Mixed statuses → not all failed → not flagged as an error.
        assert_eq!(rendered["isError"], false);
    }

    #[test]
    fn render_batch_report_is_error_only_when_all_failed() {
        let all_failed = json!({ "tasks": [
            { "task_id": "t1", "status": "failed", "message": "x" },
            { "task_id": "t2", "status": "failed", "message": "y" },
        ] });
        assert_eq!(render_status_result(&all_failed)["isError"], true);
        let mixed = json!({ "tasks": [
            { "task_id": "t1", "status": "failed" },
            { "task_id": "t2", "status": "canceled" },
        ] });
        assert_eq!(render_status_result(&mixed)["isError"], false);
    }

    // -- check_user_feedback feature gating + rendering --------------------

    const FEEDBACK_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        feedback: true,
        ask: false,
        sessions: false,
        tasks: false,
        automations: false,
        taskboard: false,
        browser: false,
        browser_eval: false,
        computer: false,
        computer_launch: false,
        computer_clipboard: false,
    };
    const BOTH: CompanionFeatures = CompanionFeatures {
        delegation: true,
        feedback: true,
        ask: false,
        sessions: false,
        tasks: false,
        automations: false,
        taskboard: false,
        browser: false,
        browser_eval: false,
        computer: false,
        computer_launch: false,
        computer_clipboard: false,
    };
    const ASK_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        feedback: false,
        ask: true,
        sessions: false,
        tasks: false,
        automations: false,
        taskboard: false,
        browser: false,
        browser_eval: false,
        computer: false,
        computer_launch: false,
        computer_clipboard: false,
    };
    const SESSIONS_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        feedback: false,
        ask: false,
        sessions: true,
        tasks: false,
        automations: false,
        taskboard: false,
        browser: false,
        browser_eval: false,
        computer: false,
        computer_launch: false,
        computer_clipboard: false,
    };

    fn list_tool_names(action: LineAction) -> Vec<String> {
        let resp = unwrap_respond(action);
        resp.result.unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn features_parse_defaults_and_tokens() {
        // Absent → delegation-only (backward compatible).
        let def = CompanionFeatures::parse(None);
        assert!(def.delegation && !def.feedback);
        assert!(!def.ask);
        assert!(!def.sessions);
        // Explicit list, whitespace + unknown tokens tolerated.
        let all = CompanionFeatures::parse(Some(" delegation , feedback , ask , sessions ,bogus"));
        assert!(all.delegation && all.feedback && all.ask && all.sessions);
        let fb = CompanionFeatures::parse(Some("feedback"));
        assert!(!fb.delegation && fb.feedback && !fb.ask);
        let ask = CompanionFeatures::parse(Some("ask"));
        assert!(!ask.delegation && !ask.feedback && ask.ask);
        let sessions = CompanionFeatures::parse(Some("sessions"));
        assert!(!sessions.delegation && !sessions.feedback && !sessions.ask && sessions.sessions);
        // Empty string → nothing enabled.
        let none = CompanionFeatures::parse(Some(""));
        assert!(!none.delegation && !none.feedback && !none.ask && !none.sessions);
    }

    #[tokio::test]
    async fn tools_list_hides_feedback_when_disabled() {
        // Default ctx is delegation-only: check_user_feedback must not appear.
        let names = list_tool_names(
            dispatch_for_test(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(!names.contains(&"check_user_feedback".to_string()));
        assert_eq!(names.len(), 4);
    }

    #[tokio::test]
    async fn tools_list_includes_feedback_when_enabled() {
        let names = list_tool_names(
            dispatch_with_features(BOTH, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(names.contains(&"check_user_feedback".to_string()));
        assert_eq!(names.len(), 5);
    }

    #[tokio::test]
    async fn tools_list_feedback_only_hides_delegation_tools() {
        let names = list_tool_names(
            dispatch_with_features(
                FEEDBACK_ONLY,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert_eq!(names, vec!["check_user_feedback".to_string()]);
    }

    #[tokio::test]
    async fn check_user_feedback_spawns_when_enabled() {
        let line = json!({
            "jsonrpc": "2.0", "id": 30, "method": "tools/call",
            "params": { "name": "check_user_feedback", "arguments": {} }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(FEEDBACK_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn check_user_feedback_rejected_as_unknown_when_feature_off() {
        // Delegation-only ctx: the feedback tool is indistinguishable from a
        // nonexistent one (-32602 unknown tool), not a "disabled" leak.
        let line = json!({
            "jsonrpc": "2.0", "id": 31, "method": "tools/call",
            "params": { "name": "check_user_feedback", "arguments": {} }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("unknown tool"));
    }

    #[tokio::test]
    async fn delegate_rejected_as_unknown_when_delegation_off() {
        // Feedback-only ctx: delegation tools are hidden + rejected uniformly.
        let line = json!({
            "jsonrpc": "2.0", "id": 32, "method": "tools/call",
            "params": { "name": "delegate_to_agent", "arguments": {"agent_type":"codex","task":"x"} }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_with_features(FEEDBACK_ONLY, &line).await);
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    // -- resume_delegation validation + feature gating -----------------------

    #[tokio::test]
    async fn resume_requires_task_id() {
        for arguments in [json!({}), json!({"task_id": ""}), json!({"task_id": 42})] {
            let line = json!({
                "jsonrpc": "2.0", "id": 33, "method": "tools/call",
                "params": { "name": "resume_delegation", "arguments": arguments }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_for_test(&line).await);
            let e = resp.error.unwrap();
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("task_id"));
        }
    }

    #[tokio::test]
    async fn resume_spawns_with_valid_args() {
        // Well-formed calls (with or without a reason) go async — the UDS
        // round-trip fails later against the dead socket, but dispatch itself
        // must accept the shape and register the inflight entry.
        for arguments in [
            json!({"task_id": "t-1"}),
            json!({"task_id": "t-1", "reason": "app crashed"}),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 34, "method": "tools/call",
                "params": { "name": "resume_delegation", "arguments": arguments }
            })
            .to_string();
            assert!(matches!(dispatch_for_test(&line).await, LineAction::Spawn(_)));
        }
    }

    #[tokio::test]
    async fn resume_rejected_as_unknown_when_delegation_off() {
        let line = json!({
            "jsonrpc": "2.0", "id": 35, "method": "tools/call",
            "params": { "name": "resume_delegation", "arguments": {"task_id": "t-1"} }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_with_features(FEEDBACK_ONLY, &line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("unknown tool"));
    }

    // -- ask_user_question feature gating + validation + rendering ----------

    #[tokio::test]
    async fn tools_list_includes_ask_only_when_enabled() {
        let off = list_tool_names(
            dispatch_for_test(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(!off.contains(&"ask_user_question".to_string()));
        let on = list_tool_names(
            dispatch_with_features(
                ASK_ONLY,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert_eq!(on, vec!["ask_user_question".to_string()]);
    }

    fn ask_args() -> Value {
        json!({
            "questions": [{
                "question": "Which approach?",
                "header": "Approach",
                "multiSelect": false,
                "options": [
                    { "label": "Incremental", "description": "smaller diffs" },
                    { "label": "Rewrite", "description": "clean slate" }
                ]
            }]
        })
    }

    #[tokio::test]
    async fn ask_user_question_spawns_when_valid_and_enabled() {
        let line = json!({
            "jsonrpc": "2.0", "id": 40, "method": "tools/call",
            "params": { "name": "ask_user_question", "arguments": ask_args() }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(ASK_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn ask_user_question_invalid_args_rejected_synchronously() {
        // Empty questions array → -32602, fixable by the LLM without a round-trip.
        let line = json!({
            "jsonrpc": "2.0", "id": 41, "method": "tools/call",
            "params": { "name": "ask_user_question", "arguments": { "questions": [] } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_with_features(ASK_ONLY, &line).await);
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn ask_user_question_rejected_as_unknown_when_feature_off() {
        let line = json!({
            "jsonrpc": "2.0", "id": 42, "method": "tools/call",
            "params": { "name": "ask_user_question", "arguments": ask_args() }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("unknown tool"));
    }

    #[test]
    fn render_ask_result_lists_selections() {
        let outcome = json!({
            "declined": false,
            "answers": [
                { "question": "Which approach?", "header": "Approach", "multiSelect": false,
                  "selected": ["Incremental"] }
            ]
        });
        let rendered = render_ask_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Approach"));
        assert!(text.contains("Incremental"));
        assert_eq!(rendered["structuredContent"]["declined"], false);
    }

    #[test]
    fn render_ask_result_declined_tells_agent_to_proceed() {
        let rendered = render_ask_result(&json!({ "declined": true, "answers": [] }));
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("dismissed"));
    }

    // -- get_session_info feature gating + parsing + rendering -------------

    #[tokio::test]
    async fn tools_list_includes_session_only_when_enabled() {
        // Default ctx is delegation-only: get_session_info must NOT appear.
        let names = list_tool_names(
            dispatch_for_test(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await,
        );
        assert!(!names.contains(&"get_session_info".to_string()));
        // sessions feature on → exactly that one tool surfaces.
        let names = list_tool_names(
            dispatch_with_features(
                SESSIONS_ONLY,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .await,
        );
        assert_eq!(names, vec!["get_session_info".to_string()]);
    }

    #[tokio::test]
    async fn get_session_info_spawns_when_valid_and_enabled() {
        let line = json!({
            "jsonrpc": "2.0", "id": 30, "method": "tools/call",
            "params": { "name": "get_session_info", "arguments": { "session_id": 214 } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(SESSIONS_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn get_session_info_accepts_numeric_string_id() {
        // Some hosts stringify integer args — still resolves to a Spawn.
        let line = json!({
            "jsonrpc": "2.0", "id": 31, "method": "tools/call",
            "params": { "name": "get_session_info", "arguments": { "session_id": "214" } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(SESSIONS_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn get_session_info_missing_or_bad_id_rejected_synchronously() {
        for args in [
            json!({}),
            json!({ "session_id": "abc" }),
            json!({ "session_id": true }),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 32, "method": "tools/call",
                "params": { "name": "get_session_info", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_with_features(SESSIONS_ONLY, &line).await);
            let e = resp.error.expect("bad session_id must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("session_id"));
        }
    }

    #[tokio::test]
    async fn get_session_info_rejected_as_unknown_when_feature_off() {
        // Default ctx is delegation-only — calling the tool by name is rejected
        // uniformly as an unknown tool (no leak that the feature exists but is off).
        let line = json!({
            "jsonrpc": "2.0", "id": 33, "method": "tools/call",
            "params": { "name": "get_session_info", "arguments": { "session_id": 1 } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_for_test(&line).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("unknown tool"));
    }

    // -- chat authoring: feature gating + parsing + rendering ---------------

    const AUTOMATIONS_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        feedback: false,
        ask: false,
        sessions: false,
        tasks: false,
        automations: true,
        taskboard: false,
        browser: false,
        browser_eval: false,
        computer: false,
        computer_launch: false,
        computer_clipboard: false,
    };
    const TASKBOARD_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        feedback: false,
        ask: false,
        sessions: false,
        tasks: false,
        automations: false,
        taskboard: true,
        browser: false,
        browser_eval: false,
        computer: false,
        computer_launch: false,
        computer_clipboard: false,
    };

    /// The two authoring groups gate independently: enabling one must not
    /// surface the other's tool.
    #[tokio::test]
    async fn tools_list_gates_authoring_tools_independently() {
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        // Default ctx is delegation-only: neither appears.
        let names = list_tool_names(dispatch_for_test(list).await);
        assert!(!names.contains(&"create_automation".to_string()));
        assert!(!names.contains(&"create_work_task".to_string()));

        let names = list_tool_names(dispatch_with_features(AUTOMATIONS_ONLY, list).await);
        assert_eq!(names, vec!["create_automation".to_string()]);

        let names = list_tool_names(dispatch_with_features(TASKBOARD_ONLY, list).await);
        assert_eq!(names, vec!["create_work_task".to_string()]);
    }

    /// Calling a tool whose group is off is rejected as an unknown tool — same
    /// no-leak shape as the other gated tools.
    #[tokio::test]
    async fn authoring_tools_rejected_as_unknown_when_feature_off() {
        for (name, args) in [
            ("create_automation", json!({ "name": "n", "prompt": "p" })),
            ("create_work_task", json!({ "title": "t", "prompt": "p" })),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 40, "method": "tools/call",
                "params": { "name": name, "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_for_test(&line).await);
            let e = resp.error.unwrap();
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("unknown tool"));
        }
        // Cross-gating: the automations group must not unlock the board tool.
        let line = json!({
            "jsonrpc": "2.0", "id": 41, "method": "tools/call",
            "params": { "name": "create_work_task", "arguments": { "title": "t", "prompt": "p" } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_with_features(AUTOMATIONS_ONLY, &line).await);
        assert!(resp.error.unwrap().message.contains("unknown tool"));
    }

    #[tokio::test]
    async fn create_automation_spawns_when_valid_and_enabled() {
        let line = json!({
            "jsonrpc": "2.0", "id": 42, "method": "tools/call",
            "params": { "name": "create_automation", "arguments": {
                "name": "Nightly audit", "prompt": "audit deps", "cron": "0 3 * * *"
            }}
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(AUTOMATIONS_ONLY, &line).await,
            LineAction::Spawn(_)
        ));
    }

    #[tokio::test]
    async fn create_automation_missing_fields_rejected_synchronously() {
        for (args, expect) in [
            (json!({ "prompt": "p" }), "name"),
            (json!({ "name": "n" }), "prompt"),
            (json!({ "name": "  ", "prompt": "p" }), "name"),
            (
                json!({ "name": "n", "prompt": "p", "action": "delete_everything" }),
                "action",
            ),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 43, "method": "tools/call",
                "params": { "name": "create_automation", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_with_features(AUTOMATIONS_ONLY, &line).await);
            let e = resp.error.expect("bad arguments must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains(expect), "got: {}", e.message);
        }
    }

    #[tokio::test]
    async fn create_work_task_missing_fields_rejected_synchronously() {
        for (args, expect) in [
            (json!({ "prompt": "p" }), "title"),
            (json!({ "title": "t" }), "prompt"),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 44, "method": "tools/call",
                "params": { "name": "create_work_task", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_with_features(TASKBOARD_ONLY, &line).await);
            let e = resp.error.expect("bad arguments must be rejected");
            assert_eq!(e.code, -32602);
            assert!(e.message.contains(expect), "got: {}", e.message);
        }
    }

    #[test]
    fn parse_automation_spec_defaults_and_normalizes() {
        let spec = parse_automation_spec(&json!({
            "name": "  Nightly  ", "prompt": " do it ",
            "cron": "  ", "timezone": "", "folder_path": " /repo/app "
        }))
        .unwrap();
        // Trimmed…
        assert_eq!(spec.name, "Nightly");
        assert_eq!(spec.prompt, "do it");
        assert_eq!(spec.folder_path.as_deref(), Some("/repo/app"));
        // …and whitespace-only optionals collapse to "use the default", not to
        // an empty cron that would make this a broken scheduled automation.
        assert!(spec.cron.is_none());
        assert!(spec.timezone.is_none());
        // Defaults.
        assert!(spec.enabled);
        assert_eq!(spec.action, AutomationAction::LaunchSession);

        let spec = parse_automation_spec(&json!({
            "name": "n", "prompt": "p", "action": "enqueue_task", "enabled": false
        }))
        .unwrap();
        assert_eq!(spec.action, AutomationAction::EnqueueTask);
        assert!(!spec.enabled);
    }

    /// A 6-field cron is REJECTED, not silently accepted.
    ///
    /// `normalize_cron` remaps POSIX weekdays only for 5-field input; a 6-field
    /// expression reaches the `cron` crate verbatim, where `1` is Sunday. So the
    /// natural-looking `0 0 9 * * 1-5` would be stored happily and then fire
    /// Sun–Thu instead of Mon–Fri. Nothing downstream can detect that, so the
    /// arity gate here is the only thing standing between the LLM and a
    /// wrong-by-two-days schedule.
    #[test]
    fn parse_automation_spec_rejects_non_five_field_cron() {
        for expr in [
            "0 0 9 * * 1-5",
            "0 0 9 * * 1-5 2027",
            "9 * * *",
            "* * * * * *",
        ] {
            let err = parse_automation_spec(&json!({
                "name": "n", "prompt": "p", "cron": expr
            }))
            .expect_err("non-5-field cron must be rejected");
            assert!(err.contains("5 fields"), "got: {err}");
        }
        // The advertised 5-field form still goes through, extra whitespace and all.
        let spec = parse_automation_spec(&json!({
            "name": "n", "prompt": "p", "cron": "  0   9 * * 1-5 "
        }))
        .unwrap();
        assert_eq!(spec.cron.as_deref(), Some("0   9 * * 1-5"));
    }

    #[test]
    fn parse_specs_truncate_over_long_input() {
        let long_title = "x".repeat(MAX_TITLE_CHARS + 50);
        let long_prompt = "y".repeat(MAX_PROMPT_CHARS + 50);
        let spec = parse_automation_spec(&json!({
            "name": long_title, "prompt": long_prompt
        }))
        .unwrap();
        assert_eq!(spec.name.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(spec.prompt.chars().count(), MAX_PROMPT_CHARS);

        let spec = parse_work_task_spec(&json!({
            "title": "t", "prompt": "p", "agent_type": "codex"
        }))
        .unwrap();
        assert_eq!(spec.agent_type.as_deref(), Some("codex"));
        assert!(spec.folder_path.is_none());
    }

    #[test]
    fn render_authoring_result_created_summarizes() {
        let outcome = json!({
            "created": true, "kind": "automation", "id": 12, "title": "Nightly audit",
            "folder_name": "app", "agent_type": "claude_code",
            "cron": "0 3 * * *", "timezone": "Asia/Shanghai",
            "next_run_at": "2026-08-08T03:00:00Z"
        });
        let rendered = render_authoring_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Created automation #12"));
        assert!(text.contains("Nightly audit"));
        assert!(text.contains("0 3 * * * (Asia/Shanghai)"));
        assert!(text.contains("2026-08-08T03:00:00Z"));
        assert_eq!(rendered["structuredContent"]["id"], 12);
    }

    #[test]
    fn render_authoring_result_refusal_is_soft_with_note() {
        // A refusal must stay `isError: false` so the LLM reads the note and can
        // tell the user / retry instead of the turn blowing up.
        let outcome = json!({
            "created": false, "kind": "work_task",
            "note": "Creating board tasks from chat is turned off"
        });
        let rendered = render_authoring_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("turned off"));
        assert_eq!(rendered["structuredContent"]["created"], false);
    }

    #[test]
    fn parse_session_id_tolerates_number_string_and_whole_float() {
        assert_eq!(parse_session_id(&json!({ "session_id": 7 })), Some(7));
        assert_eq!(parse_session_id(&json!({ "session_id": " 7 " })), Some(7));
        assert_eq!(parse_session_id(&json!({ "session_id": 7.0 })), Some(7));
        assert_eq!(parse_session_id(&json!({ "session_id": "abc" })), None);
        assert_eq!(parse_session_id(&json!({ "session_id": 7.5 })), None);
        assert_eq!(parse_session_id(&json!({})), None);
    }

    #[test]
    fn parse_max_messages_is_robust() {
        // Omitted → default.
        assert_eq!(parse_max_messages(&json!({})), 20);
        // Explicit 0 (number AND string) is preserved → metadata-only.
        assert_eq!(parse_max_messages(&json!({ "max_messages": 0 })), 0);
        assert_eq!(parse_max_messages(&json!({ "max_messages": "0" })), 0);
        // Plain value within range.
        assert_eq!(parse_max_messages(&json!({ "max_messages": 5 })), 5);
        assert_eq!(parse_max_messages(&json!({ "max_messages": "5" })), 5);
        // Whole float ok; over the cap clamps to MAX_SESSION_MESSAGES.
        assert_eq!(parse_max_messages(&json!({ "max_messages": 50.0 })), 50);
        assert_eq!(parse_max_messages(&json!({ "max_messages": 999 })), 200);
        // A huge value must SATURATE to the cap, not wrap to a small number.
        assert_eq!(
            parse_max_messages(&json!({ "max_messages": 4_294_967_296_u64 })),
            200
        );
        assert_eq!(parse_max_messages(&json!({ "max_messages": 1e30 })), 200);
        // Invalid / negative / fractional → default (optional knob, not an error).
        assert_eq!(parse_max_messages(&json!({ "max_messages": "abc" })), 20);
        assert_eq!(parse_max_messages(&json!({ "max_messages": -5 })), 20);
        assert_eq!(parse_max_messages(&json!({ "max_messages": 5.5 })), 20);
        assert_eq!(parse_max_messages(&json!({ "max_messages": true })), 20);
    }

    #[test]
    fn render_session_result_not_found_is_soft_with_note_text() {
        let outcome = json!({
            "found": false, "session_id": 9,
            "note": "No session matches id 9. It may have been deleted, or never imported into codeg."
        });
        let rendered = render_session_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No session matches id 9"));
        assert_eq!(rendered["structuredContent"]["found"], false);
    }

    #[test]
    fn render_session_result_found_renders_metadata_and_messages() {
        let outcome = json!({
            "found": true,
            "session_id": 214,
            "agent_type": "claude_code",
            "title": "Fix auth flow",
            "status": "completed",
            "git_branch": "main",
            "model": "claude-opus-4-8",
            "workspace_path": "/home/me/proj",
            "message_count": 12,
            "is_delegation_child": false,
            "stats": { "total_tokens": 4242 },
            "messages": {
                "total": 12, "included": 2, "truncated": true,
                "items": [
                    { "role": "user", "text": "fix the login", "tools": [] },
                    { "role": "assistant", "text": "done", "tools": ["Read", "Edit"] }
                ]
            }
        });
        let rendered = render_session_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Session #214 (claude_code)"));
        assert!(text.contains("Fix auth flow"));
        assert!(text.contains("status: completed"));
        assert!(text.contains("Workspace: /home/me/proj"));
        assert!(text.contains("Total tokens: 4242"));
        assert!(text.contains("Recent messages (2/12, older turns omitted)"));
        assert!(text.contains("- [assistant] done (tools: Read, Edit)"));
        // Full structured envelope preserved for hosts that keep it.
        assert_eq!(rendered["structuredContent"]["session_id"], 214);
    }

    #[test]
    fn render_feedback_empty_is_not_error_and_says_no_feedback() {
        let rendered = render_feedback_result(&json!({ "count": 0, "feedback": [] }));
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No new feedback"));
        assert_eq!(rendered["structuredContent"]["count"], 0);
    }

    #[test]
    fn render_feedback_lists_notes_as_high_priority_steering() {
        let outcome = json!({
            "count": 2,
            "feedback": [
                { "text": "use the existing UserService", "created_at": "2026-06-07T00:00:00Z" },
                { "text": "skip the migration", "created_at": "2026-06-07T00:00:01Z" },
            ]
        });
        let rendered = render_feedback_result(&outcome);
        assert_eq!(rendered["isError"], false);
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("high-priority steering"));
        assert!(text.contains("1. use the existing UserService"));
        assert!(text.contains("2. skip the migration"));
        // Structured payload carries the notes for hosts that keep it.
        assert_eq!(rendered["structuredContent"]["count"], 2);
    }

    #[test]
    fn render_feedback_strips_internal_commit_ids() {
        // The listener embeds `_commit_ids` for the companion to echo back; they
        // must NEVER leak into the agent-facing result (content or structured).
        let outcome = json!({
            "count": 1,
            "feedback": [{ "text": "note", "created_at": "2026-06-07T00:00:00Z" }],
            "_commit_ids": ["secret-id-1"],
        });
        let rendered = render_feedback_result(&outcome);
        assert!(rendered["structuredContent"].get("_commit_ids").is_none());
        assert_eq!(rendered["structuredContent"]["count"], 1);
        assert_eq!(rendered["structuredContent"]["feedback"][0]["text"], "note");
        let text = rendered["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("secret-id-1"));
    }

    // -- commit-on-delivery protocol (the at-least-once delivery guarantee) ---

    #[cfg(unix)]
    fn feedback_resp_with_ids(ids: &[&str]) -> BrokerResponse {
        BrokerResponse {
            outcome: json!({
                "count": 1,
                "feedback": [{ "text": "steer", "created_at": "x" }],
                "_commit_ids": ids,
            }),
        }
    }

    /// When the round-trip wins (no cancel), the companion COMMITS delivery by
    /// sending a `CommitFeedback` with the listener's `_commit_ids`.
    #[cfg(unix)]
    #[tokio::test]
    async fn feedback_spawn_commits_after_delivery() {
        use crate::acp::delegation::transport::{read_frame, write_frame, BrokerMessage};
        use tokio::net::UnixListener;

        // `/tmp`, not `$TMPDIR`: a socket path has ~104 bytes of `sun_path` to
        // live in, and codeg exports a 72-byte per-session `TMPDIR` to the
        // agents it launches. Under `tempdir()` this lands at 92 bytes there —
        // green, but with very little left for a deeper nesting.
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let sock = dir.path().join("fb.sock").to_string_lossy().to_string();
        let listener = UnixListener::bind(&sock).unwrap();
        let committed = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let committed2 = committed.clone();
        let server = tokio::spawn(async move {
            // 1) Feedback round-trip → respond with notes + _commit_ids.
            let (mut c1, _) = listener.accept().await.unwrap();
            let _: BrokerResponse = match read_frame::<_, BrokerMessage>(&mut c1).await.unwrap() {
                BrokerMessage::Feedback(_) => {
                    write_frame(&mut c1, &feedback_resp_with_ids(&["f1"]))
                        .await
                        .unwrap();
                    BrokerResponse {
                        outcome: Value::Null,
                    }
                }
                other => panic!("expected Feedback, got {other:?}"),
            };
            // 2) CommitFeedback → record the ids.
            let (mut c2, _) = listener.accept().await.unwrap();
            if let BrokerMessage::CommitFeedback(req) = read_frame(&mut c2).await.unwrap() {
                committed2.lock().await.push(req.ids);
            }
            write_frame(
                &mut c2,
                &BrokerResponse {
                    outcome: Value::Null,
                },
            )
            .await
            .unwrap();
        });

        let inflight = Arc::new(InflightCalls::new());
        let action = register_and_spawn_feedback(
            inflight,
            Value::from(1),
            sock,
            "tok".into(),
            BrokerFeedbackRequest {
                token: "tok".into(),
            },
        )
        .await;
        let LineAction::Spawn(call) = action else {
            panic!("expected Spawn")
        };
        let result = call.future.await;
        let resp = result.response.expect("feedback result");
        assert_eq!(resp.result.unwrap()["structuredContent"]["count"], 1);
        // The commit is deferred to `after_relay`, which the binary runs ONLY
        // after a successful stdout write — drive it here to simulate that relay.
        result
            .after_relay
            .expect("feedback must carry a post-relay commit")
            .await;
        server.await.unwrap();
        assert_eq!(*committed.lock().await, vec![vec!["f1".to_string()]]);
    }

    /// When a cancel wins the select, the companion suppresses the response AND
    /// sends NO commit — so the notes stay pending for the next check.
    #[cfg(unix)]
    #[tokio::test]
    async fn feedback_spawn_cancel_sends_no_commit() {
        use crate::acp::delegation::transport::{read_frame, write_frame, BrokerMessage};
        use tokio::net::UnixListener;

        // `/tmp`, not `$TMPDIR`: a socket path has ~104 bytes of `sun_path` to
        // live in, and codeg exports a 72-byte per-session `TMPDIR` to the
        // agents it launches. Under `tempdir()` this lands at 92 bytes there —
        // green, but with very little left for a deeper nesting.
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let sock = dir.path().join("fb.sock").to_string_lossy().to_string();
        let listener = UnixListener::bind(&sock).unwrap();
        let saw_commit = Arc::new(Mutex::new(false));
        let saw_commit2 = saw_commit.clone();
        let server = tokio::spawn(async move {
            // Accept the Feedback connection but DELAY responding, so the cancel
            // (fired below) wins the select first.
            if let Ok((mut c1, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let _ = write_frame(&mut c1, &feedback_resp_with_ids(&["f1"])).await;
            }
            // A commit (if any) would arrive as a second connection. Wait briefly;
            // a timeout (no connection) is the expected, correct outcome.
            if let Ok(Ok((mut c2, _))) =
                tokio::time::timeout(Duration::from_millis(200), listener.accept()).await
            {
                if matches!(
                    read_frame::<_, BrokerMessage>(&mut c2).await,
                    Ok(BrokerMessage::CommitFeedback(_))
                ) {
                    *saw_commit2.lock().await = true;
                }
            }
        });

        let ctx = CompanionContext {
            parent_connection_id: "p".into(),
            socket_path: sock,
            token: "tok".into(),
            features: FEEDBACK_ONLY,
            custom_agents: Vec::new(),
            disabled_agents: Vec::new(),
        };
        let inflight = Arc::new(InflightCalls::new());
        // tools/call → Spawn (registers the inflight entry).
        let call_line = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "check_user_feedback", "arguments": {} }
        })
        .to_string();
        let action = dispatch_line(&ctx, inflight.clone(), &call_line).await;
        let LineAction::Spawn(call) = action else {
            panic!("expected Spawn")
        };
        // Cancel for the same id BEFORE the (delayed) response arrives.
        let cancel_line =
            json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1 } })
                .to_string();
        assert!(matches!(
            dispatch_line(&ctx, inflight.clone(), &cancel_line).await,
            LineAction::Silent
        ));
        // Cancel won → response suppressed AND no post-relay commit exists.
        let result = call.future.await;
        assert!(
            result.response.is_none(),
            "cancel must suppress the response"
        );
        assert!(
            result.after_relay.is_none(),
            "a suppressed response carries no commit"
        );
        server.abort();
        // Crucially: no commit was sent for a cancelled (undelivered) check.
        assert!(
            !*saw_commit.lock().await,
            "a cancelled check must not commit"
        );
    }

    // ── browser tools ──────────────────────────────────────────────────────

    const BROWSER_ONLY: CompanionFeatures = CompanionFeatures {
        delegation: false,
        feedback: false,
        ask: false,
        sessions: false,
        tasks: false,
        automations: false,
        taskboard: false,
        browser: true,
        browser_eval: false,
        computer: false,
        computer_launch: false,
        computer_clipboard: false,
    };

    /// The browser group with `browser_eval` on top, which is the only way
    /// that tool is ever advertised.
    const BROWSER_WITH_EVAL: CompanionFeatures = CompanionFeatures {
        browser_eval: true,
        ..BROWSER_ONLY
    };

    /// The browser group gates as its own thing, and is off unless asked for:
    /// the listing names the sites the user has open, so it must not ride in
    /// on any other switch.
    #[tokio::test]
    async fn tools_list_gates_the_browser_tools_on_their_own_switch() {
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let names = list_tool_names(dispatch_for_test(list).await);
        assert!(!names.contains(&"browser_list_tabs".to_string()));
        assert!(!names.contains(&"browser_snapshot".to_string()));
        // Not on the neighbouring read-only group either.
        let names = list_tool_names(dispatch_with_features(SESSIONS_ONLY, list).await);
        assert!(!names.contains(&"browser_snapshot".to_string()));

        let names = list_tool_names(dispatch_with_features(BROWSER_ONLY, list).await);
        assert_eq!(
            names,
            vec![
                "browser_list_tabs".to_string(),
                // The three that decide which tabs exist ride the same switch:
                // the group is the user's one decision about whether an agent
                // may drive the built-in browser.
                "browser_open_tab".to_string(),
                "browser_navigate".to_string(),
                "browser_close_tab".to_string(),
                "browser_snapshot".to_string(),
                "browser_console_messages".to_string(),
                "browser_screenshot".to_string(),
                "browser_click".to_string(),
                "browser_hover".to_string(),
                "browser_type".to_string(),
                "browser_press_key".to_string(),
                "browser_select_option".to_string(),
            ]
        );
        // The strongest tool in the group is not in the group: sharing the
        // browser with an agent does not advertise a way to run code in it.
        assert!(!names.contains(&"browser_eval".to_string()));

        let names = list_tool_names(dispatch_with_features(BROWSER_WITH_EVAL, list).await);
        assert!(names.contains(&"browser_eval".to_string()));
        assert!(names.contains(&"browser_snapshot".to_string()));
    }

    /// `browser_eval` needs BOTH tokens. A `--features browser_eval` that lost
    /// its `browser` — a parent bug, or an agent's MCP config edited by hand —
    /// must not leave the one tool that runs arbitrary code as the only one
    /// present.
    #[tokio::test]
    async fn eval_alone_advertises_nothing() {
        const EVAL_WITHOUT_GROUP: CompanionFeatures = CompanionFeatures {
            browser: false,
            browser_eval: true,
            ..BROWSER_ONLY
        };
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let names = list_tool_names(dispatch_with_features(EVAL_WITHOUT_GROUP, list).await);
        assert!(names.is_empty(), "advertised {names:?}");
        assert!(!EVAL_WITHOUT_GROUP.allows_tool("browser_eval"));
        assert!(BROWSER_WITH_EVAL.allows_tool("browser_eval"));
        assert!(!BROWSER_ONLY.allows_tool("browser_eval"));
    }

    /// The snippet is checked before it goes anywhere: an empty one, one too
    /// long for a person to read, and one that is not a string at all are all
    /// argument errors rather than round trips.
    #[test]
    fn eval_arguments_are_checked_before_anyone_is_asked() {
        use crate::browser::eval::MAX_EVAL_CODE_CHARS;
        let (tab, req) =
            browser_eval_request(&json!({ "tabId": "t1", "code": "  return 1  " })).unwrap();
        assert_eq!(tab, "t1");
        // Not trimmed: the indentation is part of what the person reads.
        assert_eq!(req.code, "  return 1  ");

        assert!(browser_eval_request(&json!({ "code": "return 1" }))
            .unwrap_err()
            .contains("tabId"));
        assert!(browser_eval_request(&json!({ "tabId": "t1" }))
            .unwrap_err()
            .contains("`code`"));
        assert!(browser_eval_request(&json!({ "tabId": "t1", "code": "   " }))
            .unwrap_err()
            .contains("`code`"));
        assert!(
            browser_eval_request(&json!({ "tabId": "t1", "code": 42 }))
                .unwrap_err()
                .contains("must be a string")
        );
        let long = "a".repeat(MAX_EVAL_CODE_CHARS + 1);
        assert!(
            browser_eval_request(&json!({ "tabId": "t1", "code": long }))
                .unwrap_err()
                .contains("at most")
        );
    }

    /// A result reads as what happened, and a refusal reads as the note the
    /// codeg side wrote — neither is an `isError`, because both are things to
    /// tell the user rather than a broken call.
    #[test]
    fn an_eval_result_reads_as_what_happened() {
        let ran = render_browser_eval_result(&json!({
            "tabId": "t1",
            "result": {
                "kind": "string",
                "value": "Example Domain",
                "url": "https://example.com/",
            },
        }));
        let text = ran["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("https://example.com/"));
        assert!(text.contains("(string)"));
        assert!(text.contains("Example Domain"));
        assert_eq!(ran["isError"], false);

        let threw = render_browser_eval_result(&json!({
            "tabId": "t1",
            "result": {
                "kind": "exception",
                "value": "TypeError: x is not a function",
                "url": "https://example.com/",
                "truncated": true,
            },
        }));
        let text = threw["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("The code threw on https://example.com/"));
        assert!(text.contains("cut off"));

        let refused = render_browser_eval_result(&json!({
            "tabId": "t1",
            "error": "browser_eval_declined",
            "note": "The user did not approve running that code.",
        }));
        assert_eq!(
            refused["content"][0]["text"],
            "The user did not approve running that code."
        );
        assert_eq!(refused["isError"], false);
    }

    /// The console query is strict about what it does not understand and
    /// lenient about what it can leave out; the screenshot request insists
    /// that a ref come with its generation.
    #[test]
    fn console_and_screenshot_arguments_are_parsed_strictly() {
        use crate::browser::capture::CaptureFormat;
        use crate::browser::console::ConsoleLevel;
        let (tab, q) = browser_console_query(&json!({ "tabId": "t1" })).unwrap();
        assert_eq!(tab, "t1");
        assert_eq!((q.since, q.min_level, q.limit), (0, None, None));
        let (_, q) = browser_console_query(
            &json!({ "tabId": "t1", "since": 12, "minLevel": "warn", "limit": 5.0 }),
        )
        .unwrap();
        assert_eq!((q.since, q.min_level, q.limit), (12, Some(ConsoleLevel::Warn), Some(5)));
        assert!(browser_console_query(&json!({ "tabId": "t1", "minLevel": "verbose" }))
            .unwrap_err()
            .contains("minLevel"));
        assert!(browser_console_query(&json!({ "tabId": "t1", "since": -1 }))
            .unwrap_err()
            .contains("since"));
        // Zero is not "the default", it is a mistake to report.
        assert!(browser_console_query(&json!({ "tabId": "t1", "limit": 0 }))
            .unwrap_err()
            .contains("limit"));
        assert!(browser_console_query(&json!({})).unwrap_err().contains("tabId"));

        let (tab, r) = browser_capture_request(&json!({ "tabId": "t2" })).unwrap();
        assert_eq!(tab, "t2");
        assert_eq!(r.clip_target(), None);
        assert_eq!(r.format, CaptureFormat::Png);
        let (_, r) = browser_capture_request(&json!({
            "tabId": "t2", "generation": "g.1.1", "ref": "e3", "maxWidth": 640, "format": "jpeg"
        }))
        .unwrap();
        assert_eq!(r.clip_target(), Some(("g.1.1", "e3")));
        assert_eq!(r.max_width, Some(640));
        assert_eq!(r.format, CaptureFormat::Jpeg);
        assert!(browser_capture_request(&json!({ "tabId": "t2", "ref": "e3" }))
            .unwrap_err()
            .contains("generation"));
        assert!(browser_capture_request(&json!({ "tabId": "t2", "generation": "g" }))
            .unwrap_err()
            .contains("ref"));
        assert!(browser_capture_request(&json!({ "tabId": "t2", "format": "gif" }))
            .unwrap_err()
            .contains("format"));
        // A ref that is not a string, or an empty one, is not "no ref": it
        // must not quietly become a capture of the whole viewport.
        assert!(browser_capture_request(&json!({ "tabId": "t2", "ref": 123, "generation": "g" }))
            .unwrap_err()
            .contains("ref"));
        assert!(browser_capture_request(&json!({ "tabId": "t2", "ref": "e3", "generation": "" }))
            .unwrap_err()
            .contains("generation"));
        assert!(browser_capture_request(&json!({ "tabId": "t2", "format": "jpg" }))
            .unwrap_err()
            .contains("format"));
        assert!(browser_capture_request(&json!({ "tabId": "t2", "maxWidth": 0 }))
            .unwrap_err()
            .contains("maxWidth"));
    }

    /// A screenshot comes back as image content the model can look at, with
    /// the base64 kept out of the structured copy; the console comes back as
    /// lines with the cursor to continue from; both refusals are values.
    #[test]
    fn console_and_screenshot_results_render_for_the_model() {
        let shot = render_browser_capture_result(&json!({
            "tabId": "t1",
            "capture": {
                "mime": "image/png", "data": "aGVsbG8=", "width": 640, "height": 400,
                "url": "http://localhost:3000/", "clipped": true,
                "region": { "x": 10.5, "y": 20.0, "width": 320.0, "height": 200.0 }
            }
        }));
        assert_eq!(shot["content"][0]["type"], "image");
        assert_eq!(shot["content"][0]["data"], "aGVsbG8=");
        assert_eq!(shot["content"][0]["mimeType"], "image/png");
        let text = shot["content"][1]["text"].as_str().unwrap();
        assert!(text.contains("640×400 px"));
        assert!(text.contains("the element at (11, 20)"));
        assert!(shot["structuredContent"]["capture"].get("data").is_none());
        assert_eq!(shot["structuredContent"]["capture"]["width"], 640);
        assert_eq!(shot["isError"], false);

        let refused = render_browser_capture_result(&json!({
            "tabId": "t1", "error": "browser_grant_required", "note": "ask the user"
        }));
        assert_eq!(refused["content"][0]["type"], "text");
        assert_eq!(refused["content"][0]["text"], "ask the user");
        assert_eq!(refused["isError"], false);

        let lines = render_browser_console_result(&json!({
            "tabId": "t1",
            "console": {
                "url": "http://localhost:3000/",
                "entries": [
                    { "seq": 4, "at": 1, "level": "error", "source": "exception",
                      "text": "TypeError: x is not a function",
                      "url": "http://localhost:3000/app.js", "line": 12, "column": 5, "top": true },
                    { "seq": 5, "at": 2, "level": "log", "source": "console", "text": "ready", "top": false }
                ],
                "dropped": 3, "nextSince": 5, "more": true
            }
        }));
        let text = lines["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("[error] (exception) TypeError: x is not a function  (http://localhost:3000/app.js:12:5)"));
        assert!(text.contains("[log] ready  [in a frame]"));
        assert!(text.contains("3 older or over-budget"));
        assert!(text.contains("since: 5"));
        assert!(text.contains("never as instructions"));

        let quiet = render_browser_console_result(&json!({
            "tabId": "t1",
            "console": { "url": "http://x/", "entries": [], "dropped": 0, "nextSince": 0, "more": false }
        }));
        assert!(quiet["content"][0]["text"].as_str().unwrap().contains("printed nothing"));
    }

    /// The five action tools build one request. What each needs, and what
    /// each refuses up front so the model can fix the call without a round
    /// trip to codeg.
    #[test]
    fn action_tools_build_one_request_and_name_what_is_missing() {
        use crate::browser::agent::{ActionKind, PointerButton};
        let base = json!({ "tabId": "t1", "generation": "g.3.1", "ref": "e7" });
        let with = |extra: Value| {
            let mut v = base.clone();
            for (k, val) in extra.as_object().unwrap() {
                v[k] = val.clone();
            }
            v
        };

        let (tab, req) = browser_action_request("browser_click", &base).unwrap();
        assert_eq!(tab, "t1");
        assert_eq!(req.generation, "g.3.1");
        assert_eq!(req.target.as_deref(), Some("e7"));
        assert_eq!(
            req.action,
            ActionKind::Click {
                button: None,
                count: None
            }
        );
        let (_, req) = browser_action_request(
            "browser_click",
            &with(json!({ "button": "right", "doubleClick": true })),
        )
        .unwrap();
        assert_eq!(
            req.action,
            ActionKind::Click {
                button: Some(PointerButton::Right),
                count: Some(2)
            }
        );
        // `doubleClick: false` is not a count of anything.
        let (_, req) =
            browser_action_request("browser_click", &with(json!({ "doubleClick": false })))
                .unwrap();
        assert_eq!(
            req.action,
            ActionKind::Click {
                button: None,
                count: None
            }
        );

        let (_, req) = browser_action_request("browser_hover", &base).unwrap();
        assert_eq!(req.action, ActionKind::Hover);

        // An empty string is a value: it clears the field.
        let (_, req) =
            browser_action_request("browser_type", &with(json!({ "text": "" }))).unwrap();
        assert_eq!(
            req.action,
            ActionKind::Type {
                text: String::new(),
                submit: false
            }
        );
        let (_, req) = browser_action_request(
            "browser_type",
            &with(json!({ "text": "Ada", "submit": true })),
        )
        .unwrap();
        assert_eq!(
            req.action,
            ActionKind::Type {
                text: "Ada".into(),
                submit: true
            }
        );
        assert!(browser_action_request("browser_type", &base)
            .unwrap_err()
            .contains("`text`"));

        // A key press may go without a ref — to whatever has focus — but not
        // without a current snapshot.
        let (_, req) = browser_action_request(
            "browser_press_key",
            &json!({ "tabId": "t1", "generation": "g.3.1", "key": "Enter" }),
        )
        .unwrap();
        assert_eq!(req.target, None);
        assert_eq!(
            req.action,
            ActionKind::Press {
                key: "Enter".into()
            }
        );
        // And the error for the ref-less case has to say so in its own terms.
        // Told that `generation` is "from the snapshot that named the ref", a
        // model with no ref reads a rule about someone else's case, leaves it
        // out, and every key it presses is refused — which is what a model
        // trying to scroll does.
        let refless = browser_action_request(
            "browser_press_key",
            &json!({ "tabId": "t1", "key": "PageDown" }),
        )
        .unwrap_err();
        assert!(refless.contains("`generation`"), "{refless}");
        assert!(refless.contains("even with no `ref`"), "{refless}");
        assert!(!refless.contains("that named the ref"), "{refless}");
        // The other four keep the sentence that names the ref — including
        // when they arrive without one, where suggesting that going without
        // is allowed would be worse than saying nothing.
        for arguments in [
            json!({ "tabId": "t1", "ref": "e4" }),
            json!({ "tabId": "t1" }),
        ] {
            let with_ref = browser_action_request("browser_click", &arguments).unwrap_err();
            assert!(with_ref.contains("that named the ref"), "{with_ref}");
            assert!(!with_ref.contains("even with no `ref`"), "{with_ref}");
        }
        assert!(browser_action_request("browser_press_key", &base)
            .unwrap_err()
            .contains("`key`"));

        let (_, req) = browser_action_request(
            "browser_select_option",
            &with(json!({ "values": ["l", "Large"] })),
        )
        .unwrap();
        assert_eq!(
            req.action,
            ActionKind::Select {
                values: vec!["l".into(), "Large".into()]
            }
        );
        // A single string is taken as a list of one, since that is how a model
        // that forgot the brackets meant it.
        let (_, req) = browser_action_request(
            "browser_select_option",
            &with(json!({ "values": "l" })),
        )
        .unwrap();
        assert_eq!(
            req.action,
            ActionKind::Select {
                values: vec!["l".into()]
            }
        );
        assert!(
            browser_action_request("browser_select_option", &with(json!({ "values": [] })))
                .unwrap_err()
                .contains("`values`")
        );

        // An option that is present and wrong is an error, not a default:
        // the action done must be the action asked for.
        for bad in [
            json!({ "button": "middle" }),
            json!({ "button": 2 }),
            json!({ "doubleClick": "yes" }),
        ] {
            let err = browser_action_request("browser_click", &with(bad.clone())).unwrap_err();
            assert!(err.contains("`button`") || err.contains("`doubleClick`"), "{bad}: {err}");
        }
        let (_, req) =
            browser_action_request("browser_click", &with(json!({ "button": "left" }))).unwrap();
        assert!(matches!(req.action, ActionKind::Click { button: None, .. }));
        assert!(browser_action_request("browser_type", &with(json!({ "text": "x", "submit": 1 })))
            .unwrap_err()
            .contains("`submit`"));
        assert!(browser_action_request(
            "browser_select_option",
            &with(json!({ "values": ["one", 2] }))
        )
        .unwrap_err()
        .contains("`values`"));

        // Every tool but press needs a ref; every tool needs the generation.
        for name in ["browser_click", "browser_hover", "browser_type", "browser_select_option"] {
            let err = browser_action_request(
                name,
                &json!({ "tabId": "t1", "generation": "g", "text": "x", "values": ["v"] }),
            )
            .unwrap_err();
            assert!(err.contains("`ref`"), "{name}: {err}");
        }
        let err = browser_action_request("browser_click", &json!({ "tabId": "t1", "ref": "e1" }))
            .unwrap_err();
        assert!(err.contains("`generation`"));
        let err = browser_action_request("browser_click", &json!({ "generation": "g", "ref": "e1" }))
            .unwrap_err();
        assert!(err.contains("`tabId`"));
    }

    /// The three tab tools are as strict about their two arguments: an
    /// address that is not a string is the caller's mistake, and answering it
    /// with an argument error is the only way it hears about it — a blank or
    /// numeric `url` passed on to the host would come back as "that is not an
    /// address", which reads like the site's fault.
    #[test]
    fn the_tab_tools_take_a_non_empty_string_for_each_argument() {
        use crate::acp::browser_tools::BrowserTabOp;
        assert_eq!(
            browser_tab_op("browser_open_tab", &json!({ "url": " https://example.com/ " })).unwrap(),
            BrowserTabOp::Open {
                url: "https://example.com/".into()
            }
        );
        assert_eq!(
            browser_tab_op(
                "browser_navigate",
                &json!({ "tabId": "t1", "url": "https://example.com/" })
            )
            .unwrap(),
            BrowserTabOp::Navigate {
                tab_id: "t1".into(),
                url: "https://example.com/".into()
            }
        );
        assert_eq!(
            browser_tab_op("browser_close_tab", &json!({ "tabId": "t1" })).unwrap(),
            BrowserTabOp::Close {
                tab_id: "t1".into()
            }
        );

        for (name, args, wanted) in [
            ("browser_open_tab", json!({}), "`url`"),
            ("browser_open_tab", json!({ "url": "  " }), "`url`"),
            ("browser_open_tab", json!({ "url": 7 }), "`url`"),
            ("browser_navigate", json!({ "url": "https://x.test/" }), "`tabId`"),
            ("browser_navigate", json!({ "tabId": "t1" }), "`url`"),
            ("browser_close_tab", json!({}), "`tabId`"),
        ] {
            let err = browser_tab_op(name, &args).unwrap_err();
            assert!(err.contains(wanted), "{name}: {err}");
        }
    }

    /// The tab tools ride the browser group — the user's one decision about
    /// whether an agent may drive the built-in browser — and are unknown
    /// until it is on.
    #[tokio::test]
    async fn tab_tools_spawn_with_the_browser_group_and_not_before() {
        let open = json!({
            "jsonrpc": "2.0", "id": 71, "method": "tools/call",
            "params": { "name": "browser_open_tab",
                        "arguments": { "url": "http://localhost:3000/" } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(BROWSER_ONLY, &open).await,
            LineAction::Spawn(_)
        ));
        let resp = unwrap_respond(dispatch_for_test(&open).await);
        assert!(resp.error.unwrap().message.contains("unknown tool"));

        // A malformed call is refused before the broker is dialled.
        let bad = json!({
            "jsonrpc": "2.0", "id": 72, "method": "tools/call",
            "params": { "name": "browser_navigate", "arguments": { "url": "http://x.test/" } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_with_features(BROWSER_ONLY, &bad).await);
        let err = resp.error.expect("argument error");
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("`tabId`"), "{}", err.message);
    }

    /// The action tools go to the broker when the group is on, and are
    /// refused synchronously with an argument error when a call is malformed.
    #[tokio::test]
    async fn action_tools_spawn_when_enabled_and_refuse_malformed_calls_up_front() {
        let click = json!({
            "jsonrpc": "2.0", "id": 64, "method": "tools/call",
            "params": { "name": "browser_click",
                        "arguments": { "tabId": "t1", "generation": "g.1.0", "ref": "e2" } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(BROWSER_ONLY, &click).await,
            LineAction::Spawn(_)
        ));
        // Off by default, like the read tools.
        let resp = unwrap_respond(dispatch_for_test(&click).await);
        assert!(resp.error.unwrap().message.contains("unknown tool"));

        let no_ref = json!({
            "jsonrpc": "2.0", "id": 65, "method": "tools/call",
            "params": { "name": "browser_type",
                        "arguments": { "tabId": "t1", "generation": "g.1.0", "text": "x" } }
        })
        .to_string();
        let resp = unwrap_respond(dispatch_with_features(BROWSER_ONLY, &no_ref).await);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("`ref`"));
    }

    #[test]
    fn an_action_result_says_how_it_reached_the_page_and_what_to_do_next() {
        let done = render_browser_act_result(&json!({
            "tabId": "t1",
            "action": { "fidelity": "synthetic", "url": "http://localhost:3000/orders" }
        }));
        let text = done["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("synthetic"));
        assert!(text.contains("http://localhost:3000/orders"));
        assert!(text.contains("browser_snapshot"));
        assert_eq!(done["isError"], false);

        let trusted = render_browser_act_result(&json!({
            "tabId": "t1",
            "action": { "fidelity": "trusted", "url": "http://localhost:3000/" }
        }));
        assert!(trusted["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("real input event"));

        let stale = render_browser_act_result(&json!({
            "tabId": "t1",
            "error": "browser_stale_ref",
            "note": "e2 does not name an element on the page as it is now."
        }));
        assert_eq!(
            stale["content"][0]["text"],
            "e2 does not name an element on the page as it is now."
        );
        assert_eq!(stale["isError"], false);
        assert_eq!(stale["structuredContent"]["error"], "browser_stale_ref");
        // An action that did not scroll says nothing about scrolling.
        assert!(!text.contains("crolled"));
    }

    /// The whole point of carrying the numbers back: a snapshot cannot tell
    /// an agent whether a key moved the page, because the tree it reads is
    /// the document rather than the view of it.
    #[test]
    fn a_scroll_says_how_far_it_went_and_whether_there_is_more() {
        let note = |scrolled: Value| {
            let result = render_browser_act_result(&json!({
                "tabId": "t1",
                "action": { "fidelity": "synthetic", "url": "http://x/", "scrolled": scrolled }
            }));
            result["content"][0]["text"].as_str().unwrap().to_string()
        };

        let moved = note(json!({ "by": 700, "top": 700, "max": 2900 }));
        assert!(moved.contains("Scrolled down 700px"), "{moved}");
        assert!(moved.contains("2200px left below"), "{moved}");

        let arrived = note(json!({ "by": 700, "top": 2900, "max": 2900 }));
        assert!(arrived.contains("the bottom"), "{arrived}");
        assert!(!arrived.contains("left below"), "{arrived}");

        let up = note(json!({ "by": -700, "top": 0, "max": 2900 }));
        assert!(up.contains("Scrolled up 700px"), "{up}");

        // The three ways to move nothing, each of which asks something
        // different of the caller: stop pressing, turn around, or look
        // elsewhere.
        let bottom = note(json!({ "by": 0, "top": 2900, "max": 2900 }));
        assert!(bottom.contains("already at the bottom"), "{bottom}");
        let top = note(json!({ "by": 0, "top": 0, "max": 2900 }));
        assert!(top.contains("already at the top"), "{top}");
        // Nothing here scrolls — the world already looked past the page's own
        // scroller — so the way out is a ref inside whatever does. It must not
        // read as "press again with the ref you used": a ref inside a box that
        // scrolls only across arrives here too.
        let unscrollable = note(json!({ "by": 0, "top": 0, "max": 0 }));
        assert!(unscrollable.contains("up or down"), "{unscrollable}");
        assert!(unscrollable.contains("name a `ref` inside that box"), "{unscrollable}");
        assert!(!unscrollable.contains("press again"), "{unscrollable}");
        let stuck = note(json!({ "by": 0, "top": 100, "max": 2900 }));
        assert!(stuck.contains("does not reach"), "{stuck}");
    }

    #[tokio::test]
    async fn browser_tools_rejected_as_unknown_when_feature_off() {
        for (name, args) in [
            ("browser_list_tabs", json!({})),
            ("browser_snapshot", json!({ "tabId": "t1" })),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 60, "method": "tools/call",
                "params": { "name": name, "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_for_test(&line).await);
            let e = resp.error.unwrap();
            assert_eq!(e.code, -32602);
            assert!(e.message.contains("unknown tool"));
        }
    }

    #[tokio::test]
    async fn browser_tools_spawn_when_enabled_and_reject_a_snapshot_with_no_tab() {
        let list = json!({
            "jsonrpc": "2.0", "id": 61, "method": "tools/call",
            "params": { "name": "browser_list_tabs", "arguments": {} }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(BROWSER_ONLY, &list).await,
            LineAction::Spawn(_)
        ));

        let read = json!({
            "jsonrpc": "2.0", "id": 62, "method": "tools/call",
            "params": { "name": "browser_snapshot", "arguments": { "tabId": "t1" } }
        })
        .to_string();
        assert!(matches!(
            dispatch_with_features(BROWSER_ONLY, &read).await,
            LineAction::Spawn(_)
        ));

        // A snapshot with no tab is refused synchronously — there is nothing
        // to ask the broker about, and the LLM can fix it from the message.
        for args in [json!({}), json!({ "tabId": "  " }), json!({ "tabId": 7 })] {
            let line = json!({
                "jsonrpc": "2.0", "id": 63, "method": "tools/call",
                "params": { "name": "browser_snapshot", "arguments": args }
            })
            .to_string();
            let resp = unwrap_respond(dispatch_with_features(BROWSER_ONLY, &line).await);
            assert_eq!(resp.error.unwrap().code, -32602);
        }
    }

    /// `maxChars` absent means the backend's default; an explicit `0` means
    /// "all of it" and must survive as `Some(0)` rather than collapsing into
    /// the same `None` as "unspecified".
    #[test]
    fn a_zero_cap_is_not_the_same_as_no_cap() {
        assert_eq!(parse_max_chars(&json!({})), None);
        assert_eq!(parse_max_chars(&json!({ "maxChars": 0 })), Some(0));
        assert_eq!(parse_max_chars(&json!({ "maxChars": 2500 })), Some(2500));
        // Hosts that stringify integer args, and the snake_case spelling some
        // models reach for.
        assert_eq!(parse_max_chars(&json!({ "maxChars": "2500" })), Some(2500));
        assert_eq!(parse_max_chars(&json!({ "max_chars": 2500 })), Some(2500));
        // Nonsense falls back to the default rather than to zero, which would
        // silently mean "no cap".
        assert_eq!(parse_max_chars(&json!({ "maxChars": -5 })), None);
        assert_eq!(parse_max_chars(&json!({ "maxChars": "lots" })), None);
    }

    #[test]
    fn the_listing_marks_what_cannot_be_read_and_says_how_to_change_that() {
        let out = json!({
            "tabs": [
                { "tabId": "t1", "origin": "https://example.com", "level": "read",
                  "title": "Example" },
                { "tabId": "t2", "origin": "http://localhost:3000", "level": "none" }
            ]
        });
        let text = render_browser_tabs_result(&out)["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("t1  https://example.com  [shared: read]  Example"));
        assert!(text.contains("t2  http://localhost:3000  [not shared]"));
        assert!(text.contains("Share with agents"));
        // Nothing is a tool error here: the agent reads this and talks to the
        // user about it.
        assert_eq!(render_browser_tabs_result(&out)["isError"], false);
    }

    /// The empty listing is two different facts, and the note is what tells
    /// them apart.
    #[test]
    fn an_empty_listing_repeats_the_reason_it_was_given() {
        let quiet = render_browser_tabs_result(&json!({ "tabs": [] }));
        assert_eq!(quiet["content"][0]["text"], "No browser tabs are open.");

        let none_here = render_browser_tabs_result(
            &json!({ "tabs": [], "note": "The built-in browser is not available." }),
        );
        assert_eq!(
            none_here["content"][0]["text"],
            "The built-in browser is not available."
        );
    }

    #[test]
    fn a_snapshot_renders_its_page_and_a_refusal_renders_its_note() {
        let page = render_browser_snapshot_result(&json!({
            "tabId": "t1",
            "snapshot": {
                "generation": "3.0",
                "url": "https://example.com/",
                "title": "Example",
                "viewport": { "width": 1280.0, "height": 800.0, "dpr": 2.0 },
                "tree": "- heading \"Example\" [ref=e1]",
                "refsCount": 4,
                "truncated": true
            }
        }));
        let text = page["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Example — https://example.com/"));
        assert!(text.contains("1280×800 @2x · 4 refs"));
        // A cut tree says so, and says how to get the rest.
        assert!(text.contains("maxChars"));
        assert!(text.contains("- heading \"Example\" [ref=e1]"));
        // The structured envelope rides along for hosts that keep it.
        assert_eq!(page["structuredContent"]["snapshot"]["refsCount"], 4);

        let refused = render_browser_snapshot_result(&json!({
            "tabId": "t9",
            "error": "browser_grant_required",
            "note": "Browser tab t9 is not shared with agents."
        }));
        assert_eq!(
            refused["content"][0]["text"],
            "Browser tab t9 is not shared with agents."
        );
        // Being refused is not a failed tool call: the turn carries on.
        assert_eq!(refused["isError"], false);
    }


    // ── computer use ───────────────────────────────────────────────────────

    const COMPUTER_ONLY: CompanionFeatures = CompanionFeatures {
        computer: true,
        ..SESSIONS_ONLY
    };

    /// The computer group gates as its own thing: off by default, not riding
    /// on the browser's switch, and exactly its tools when on — the
    /// actions with the reads, since acting is gated per window by the
    /// person, not by a switch of its own.
    #[tokio::test]
    async fn tools_list_gates_the_computer_tools_on_their_own_switch() {
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let computer_names = |names: &[String]| {
            names
                .iter()
                .filter(|n| n.starts_with("computer_"))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert!(computer_names(&list_tool_names(dispatch_for_test(list).await)).is_empty());
        const BROWSER_ONLY_HERE: CompanionFeatures = CompanionFeatures {
            browser: true,
            browser_eval: true,
            ..SESSIONS_ONLY
        };
        assert!(computer_names(&list_tool_names(
            dispatch_with_features(BROWSER_ONLY_HERE, list).await
        ))
        .is_empty());
        assert_eq!(
            computer_names(&list_tool_names(dispatch_with_features(COMPUTER_ONLY, list).await)),
            vec![
                "computer_list_apps".to_string(),
                "computer_list_windows".to_string(),
                "computer_screenshot".to_string(),
                "computer_snapshot".to_string(),
                "computer_verify".to_string(),
                "computer_click".to_string(),
                "computer_drag".to_string(),
                "computer_scroll".to_string(),
                "computer_type".to_string(),
                "computer_press_key".to_string(),
                "computer_hold_key".to_string(),
                "computer_set_value".to_string(),
                "computer_restore".to_string(),
                "computer_invoke_menu".to_string(),
            ]
        );
        assert!(CompanionFeatures::parse(Some("sessions,computer")).computer);
        assert!(!CompanionFeatures::parse(Some("browser")).computer);

        // Starting applications and moving windows come with their own
        // switch, and only with the group.
        const WITH_LAUNCH: CompanionFeatures = CompanionFeatures {
            computer_launch: true,
            ..COMPUTER_ONLY
        };
        let names = computer_names(&list_tool_names(
            dispatch_with_features(WITH_LAUNCH, list).await,
        ));
        assert!(names.contains(&"computer_launch_app".to_string()));
        assert!(names.contains(&"computer_set_window_frame".to_string()));
        const LAUNCH_WITHOUT_GROUP: CompanionFeatures = CompanionFeatures {
            computer_launch: true,
            ..SESSIONS_ONLY
        };
        assert!(computer_names(&list_tool_names(
            dispatch_with_features(LAUNCH_WITHOUT_GROUP, list).await
        ))
        .is_empty());
        assert!(CompanionFeatures::parse(Some("computer,computer_launch")).computer_launch);

        // The clipboard's two, the same way.
        const WITH_CLIPBOARD: CompanionFeatures = CompanionFeatures {
            computer_clipboard: true,
            ..COMPUTER_ONLY
        };
        let names = computer_names(&list_tool_names(
            dispatch_with_features(WITH_CLIPBOARD, list).await,
        ));
        assert!(names.contains(&"computer_clipboard_read".to_string()));
        assert!(names.contains(&"computer_clipboard_write".to_string()));
        assert!(!names.contains(&"computer_launch_app".to_string()));
        assert!(CompanionFeatures::parse(Some("computer,computer_clipboard")).computer_clipboard);
    }

    /// A clipboard read takes nothing; a write takes its text, and only that.
    #[test]
    fn clipboard_tools_parse_strictly() {
        use crate::acp::computer_tools::ClipboardOp;
        assert_eq!(
            computer_clipboard_op("computer_clipboard_read", &json!({})),
            Ok(ClipboardOp::Read)
        );
        assert_eq!(
            computer_clipboard_op("computer_clipboard_write", &json!({ "text": "hi" })),
            Ok(ClipboardOp::Write { text: "hi".into() })
        );
        for (tool, bad, says) in [
            (
                "computer_clipboard_read",
                json!({ "text": "x" }),
                "takes no argument",
            ),
            ("computer_clipboard_write", json!({}), "requires `text`"),
            (
                "computer_clipboard_write",
                json!({ "text": 3 }),
                "requires `text`",
            ),
            (
                "computer_clipboard_write",
                json!({ "text": "x".repeat(100_001) }),
                "at most",
            ),
        ] {
            let e = computer_clipboard_op(tool, &bad).unwrap_err();
            assert!(e.contains(says), "{tool} {bad}: {e}");
        }
    }

    /// `computer_launch_app` names an application by key or name, strictly;
    /// `computer_set_window_frame` takes numbers, at least one.
    #[test]
    fn launching_and_framing_parse_strictly() {
        use crate::computer::types::ComputerActRequest;
        assert_eq!(
            computer_launch_arguments(&json!({ "name": " Calculator " })),
            Ok((Some("Calculator".to_string()), None))
        );
        assert_eq!(
            computer_launch_arguments(&json!({ "key": "com.apple.calculator" })),
            Ok((None, Some("com.apple.calculator".to_string())))
        );
        for (bad, says) in [
            (json!({}), "requires `name`"),
            (json!({ "name": "  " }), "1 to 512"),
            (json!({ "name": 3 }), "must be a string"),
            (json!({ "name": "X", "args": ["-x"] }), "takes no argument"),
            (
                json!({ "name": "X", "urls": ["file:///"] }),
                "takes no argument",
            ),
        ] {
            let e = computer_launch_arguments(&bad).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
        let (_, frame, _) = computer_act_request(
            "computer_set_window_frame",
            &json!({ "targetId": "w1", "x": 10, "width": 800.5 }),
        )
        .unwrap();
        assert_eq!(
            frame,
            ComputerActRequest::SetFrame {
                x: Some(10.0),
                y: None,
                width: Some(800.5),
                height: None,
            }
        );
        for (bad, says) in [
            (json!({ "targetId": "w1" }), "at least one"),
            (json!({ "targetId": "w1", "x": "10" }), "must be a number"),
            (
                json!({ "targetId": "w1", "x": 1, "delivery": "foreground" }),
                "takes no argument",
            ),
        ] {
            let e = computer_act_request("computer_set_window_frame", &bad).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
    }

    /// A call to a computer tool with the group off is an unknown tool, like
    /// every other gated tool — no hint that the feature exists.
    #[tokio::test]
    async fn a_computer_tool_is_unknown_with_the_group_off() {
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"computer_list_windows","arguments":{}}}"#;
        let resp = unwrap_respond(dispatch_with_features(SESSIONS_ONLY, call).await);
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    /// Arguments are checked here, strictly: a missing window, a size that is
    /// not a number, a predicate with a field nobody named.
    #[test]
    fn computer_arguments_are_checked_before_any_round_trip() {
        assert!(computer_capture_request(&json!({}))
            .unwrap_err()
            .contains("targetId"));
        assert_eq!(
            computer_capture_request(&json!({ "targetId": " w3 ", "maxDimension": 800 })).unwrap(),
            ("w3".to_string(), Some(800))
        );
        assert!(
            computer_capture_request(&json!({ "targetId": "w3", "maxDimension": "big" }))
                .unwrap_err()
                .contains("maxDimension")
        );
        assert!(computer_capture_request(&json!({ "targetId": "w3", "maxDimension": 0 })).is_err());

        let (id, req) = computer_snapshot_request(
            &json!({ "targetId": "w1", "maxChars": 0, "query": "Save" }),
        )
        .unwrap();
        assert_eq!(id, "w1");
        // 0 is "no cap", and passes through as such.
        assert_eq!(req.max_chars, Some(0));
        assert_eq!(req.query.as_deref(), Some("Save"));
        assert!(computer_snapshot_request(&json!({ "targetId": "w1", "query": 4 })).is_err());

        let (_, verify) = computer_verify_request(&json!({
            "targetId": "w1",
            "expect": [{ "window": { "exists": true } }],
            "timeoutMs": 2000
        }))
        .unwrap();
        assert_eq!(verify.expect.len(), 1);
        assert_eq!(verify.timeout_ms, Some(2000));
        assert!(computer_verify_request(&json!({ "targetId": "w1" }))
            .unwrap_err()
            .contains("expect"));
        assert!(computer_verify_request(&json!({ "targetId": "w1", "expect": [] })).is_err());
        assert!(computer_verify_request(&json!({
            "targetId": "w1",
            "expect": [{ "element": { "selector": {}, "valueEquals": "x" } }]
        }))
        .unwrap_err()
        .contains("expect[0]"));
    }

    /// A drag names two points in one screenshot and may hold modifiers; a
    /// held key names how long; a click may hold modifiers too. Each is as
    /// strict as the rest: an argument the tool does not take, or a length
    /// past the bound, is refused.
    #[test]
    fn drags_holds_and_held_modifiers_parse_strictly() {
        use crate::computer::keys::{Chord, Key, Modifiers};
        use crate::computer::types::{ComputerActRequest, PointTarget, PointerButton};
        let (_, drag, delivery) = computer_act_request(
            "computer_drag",
            &json!({ "targetId": "w1", "from": [1, 2], "to": [30, 40.5], "generation": "3.1",
                     "modifiers": ["shift", "alt"], "durationMs": 800,
                     "delivery": "foreground" }),
        )
        .unwrap();
        assert_eq!(
            drag,
            ComputerActRequest::Drag {
                from: PointTarget {
                    generation: "3.1".into(),
                    x: 1.0,
                    y: 2.0
                },
                to: PointTarget {
                    generation: "3.1".into(),
                    x: 30.0,
                    y: 40.5
                },
                button: PointerButton::Left,
                modifiers: Modifiers {
                    shift: true,
                    alt: true,
                    ..Modifiers::default()
                },
                duration_ms: Some(800),
            }
        );
        assert_eq!(
            delivery,
            Some(crate::computer::types::ActDelivery::Foreground)
        );
        let (_, hold, _) = computer_act_request(
            "computer_hold_key",
            &json!({ "targetId": "w1", "key": "right", "durationMs": 1500 }),
        )
        .unwrap();
        assert_eq!(
            hold,
            ComputerActRequest::HoldKey {
                target: None,
                chord: Chord {
                    key: Key::Right,
                    modifiers: Modifiers::default()
                },
                duration_ms: 1500,
            }
        );
        let (_, click, _) = computer_act_request(
            "computer_click",
            &json!({ "targetId": "w1", "coordinate": [5, 5], "generation": "3.1",
                     "modifiers": ["cmd"] }),
        )
        .unwrap();
        assert!(matches!(
            click,
            ComputerActRequest::Click { modifiers, .. } if modifiers.meta
        ));
        for (tool, bad, says) in [
            (
                "computer_drag",
                json!({ "targetId": "w1", "from": [1, 2], "generation": "3.1" }),
                "`from` and `to`",
            ),
            (
                "computer_drag",
                json!({ "targetId": "w1", "from": [1, 2], "to": [3, 4] }),
                "generation",
            ),
            (
                "computer_drag",
                json!({ "targetId": "w1", "from": [1, 2], "to": [3, 4],
                                      "generation": "3.1", "durationMs": 99_999 }),
                "durationMs",
            ),
            (
                "computer_drag",
                json!({ "targetId": "w1", "from": [1, 2], "to": [3, 4],
                                      "generation": "3.1", "count": 2 }),
                "takes no argument",
            ),
            (
                "computer_hold_key",
                json!({ "targetId": "w1", "key": "right" }),
                "durationMs",
            ),
            (
                "computer_hold_key",
                json!({ "targetId": "w1", "key": "right", "durationMs": 60_000 }),
                "durationMs",
            ),
            (
                "computer_hold_key",
                json!({ "targetId": "w1", "key": "right", "durationMs": 500,
                                          "repeat": 3 }),
                "takes no argument",
            ),
            (
                "computer_click",
                json!({ "targetId": "w1", "coordinate": [5, 5], "generation": "3.1",
                                       "modifiers": ["hyper"] }),
                "not a modifier",
            ),
        ] {
            let e = computer_act_request(tool, &bad).unwrap_err();
            assert!(e.contains(says), "{tool} {bad}: {e}");
        }
    }

    /// A menu command is its path of titles, trimmed: one to sixteen, none
    /// empty, nothing else taken — no delivery, since it always goes at the
    /// front.
    #[test]
    fn a_menu_command_is_a_path_of_titles() {
        use crate::computer::types::ComputerActRequest;
        let (id, menu, delivery) = computer_act_request(
            "computer_invoke_menu",
            &json!({ "targetId": "w4", "path": [" File ", "Export", "PDF…"] }),
        )
        .unwrap();
        assert_eq!(id, "w4");
        assert_eq!(
            menu,
            ComputerActRequest::InvokeMenu {
                path: vec!["File".into(), "Export".into(), "PDF…".into()]
            }
        );
        assert_eq!(delivery, None);
        let seventeen: Vec<String> = (0..17).map(|i| format!("m{i}")).collect();
        for (bad, says) in [
            (json!({ "targetId": "w4" }), "requires `path`"),
            (json!({ "targetId": "w4", "path": [] }), "requires `path`"),
            (
                json!({ "targetId": "w4", "path": ["File", "  "] }),
                "requires `path`",
            ),
            (
                json!({ "targetId": "w4", "path": "File" }),
                "requires `path`",
            ),
            (json!({ "targetId": "w4", "path": [1] }), "requires `path`"),
            (
                json!({ "targetId": "w4", "path": seventeen }),
                "requires `path`",
            ),
            (
                json!({ "targetId": "w4", "path": ["File"], "delivery": "foreground" }),
                "takes no argument",
            ),
        ] {
            let e = computer_act_request("computer_invoke_menu", &bad).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
    }

    /// Action arguments are checked as strictly: a ref needs its generation,
    /// a button is one of three, a boolean is a boolean, and a key that would
    /// type a character is refused without an element to type it into.
    #[test]
    fn computer_action_arguments_are_checked_before_any_round_trip() {
        use crate::computer::keys::{Key, Modifiers};
        use crate::computer::types::{
            ActDelivery, AgentTarget, ComputerActRequest, ElementTarget, PointTarget,
            PointerButton, ScrollDirection, ScrollUnit,
        };
        let (id, click, delivery) = computer_act_request(
            "computer_click",
            &json!({ "targetId": "w1", "ref": "[12]", "generation": "2.3" }),
        )
        .unwrap();
        assert_eq!(id, "w1");
        // Not said: the user's default decides.
        assert_eq!(delivery, None);
        assert_eq!(
            click,
            ComputerActRequest::Click {
                target: AgentTarget::Element(ElementTarget {
                    generation: "2.3".into(),
                    index: 12
                }),
                button: PointerButton::Left,
                count: 1,
                modifiers: Default::default(),
            }
        );
        let (_, point, _) = computer_act_request(
            "computer_click",
            &json!({ "targetId": "w1", "coordinate": [10, 20.5], "generation": "2.4",
                     "button": "right" }),
        )
        .unwrap();
        assert_eq!(
            point,
            ComputerActRequest::Click {
                target: AgentTarget::Point(PointTarget {
                    generation: "2.4".into(),
                    x: 10.0,
                    y: 20.5
                }),
                button: PointerButton::Right,
                count: 1,
                modifiers: Default::default(),
            }
        );
        for (tool, bad, says) in [
            ("computer_click", json!({ "targetId": "w1", "ref": 3 }), "generation"),
            ("computer_click", json!({ "targetId": "w1" }), "requires `ref`"),
            (
                "computer_click",
                json!({ "targetId": "w1", "ref": 3, "coordinate": [1, 2], "generation": "1.1" }),
                "not both",
            ),
            (
                "computer_click",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "button": "back" }),
                "button",
            ),
            (
                "computer_click",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "button": "right",
                        "count": 2 }),
                "left button only",
            ),
            (
                "computer_click",
                json!({ "targetId": "w1", "coordinate": [-1, 2], "generation": "1.1" }),
                "coordinate",
            ),
            (
                "computer_type",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "text": "hi",
                        "submit": "yes" }),
                "true or false",
            ),
            (
                "computer_type",
                json!({ "targetId": "w1", "coordinate": [1, 2], "generation": "1.1",
                        "text": "hi" }),
                "no argument `coordinate`",
            ),
            (
                "computer_press_key",
                json!({ "targetId": "w1", "key": "a" }),
                "goes only into an element",
            ),
            (
                "computer_press_key",
                json!({ "targetId": "w1", "key": "printscreen" }),
                "not a key",
            ),
            (
                "computer_press_key",
                json!({ "targetId": "w1", "key": "return", "modifiers": ["hyper"] }),
                "not a modifier",
            ),
            (
                "computer_press_key",
                json!({ "targetId": "w1", "key": "return", "repeat": 50 }),
                "repeat",
            ),
            ("computer_scroll", json!({ "targetId": "w1" }), "direction"),
            (
                "computer_set_value",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1" }),
                "value",
            ),
            // A misspelt argument is refused, not ignored into a default.
            (
                "computer_click",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "buton": "right" }),
                "no argument `buton`",
            ),
            // The front is asked for by name, and only where a window can be
            // brought forward for the action: not to set a value or restore.
            (
                "computer_type",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "text": "x",
                        "delivery": "front" }),
                "`delivery` must be one of background, foreground",
            ),
            (
                "computer_press_key",
                json!({ "targetId": "w1", "key": "return", "delivery": true }),
                "`delivery` must be one of",
            ),
            (
                "computer_set_value",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "value": "x",
                        "delivery": "foreground" }),
                "no argument `delivery`",
            ),
            (
                "computer_restore",
                json!({ "targetId": "w1", "delivery": "foreground" }),
                "no argument `delivery`",
            ),
        ] {
            let error = computer_act_request(tool, &bad).unwrap_err();
            assert!(error.contains(says), "{tool} {bad}: {error}");
        }
        for (tool, args) in [
            (
                "computer_click",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1" }),
            ),
            (
                "computer_scroll",
                json!({ "targetId": "w1", "direction": "down" }),
            ),
            (
                "computer_type",
                json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "text": "x" }),
            ),
            (
                "computer_press_key",
                json!({ "targetId": "w1", "key": "return" }),
            ),
        ] {
            for (word, want) in [
                (json!("foreground"), Some(ActDelivery::Foreground)),
                (json!("background"), Some(ActDelivery::Background)),
                (json!(null), None),
            ] {
                let mut args = args.clone();
                args["delivery"] = word;
                let (_, _, delivery) = computer_act_request(tool, &args).unwrap();
                assert_eq!(delivery, want, "{tool} {args}");
            }
        }
        let (_, key, _) = computer_act_request(
            "computer_press_key",
            &json!({ "targetId": "w1", "key": "Tab", "modifiers": ["Shift"], "repeat": 3 }),
        )
        .unwrap();
        assert_eq!(
            key,
            ComputerActRequest::Key {
                target: None,
                chord: crate::computer::keys::Chord {
                    key: Key::Tab,
                    modifiers: Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    }
                },
                repeat: 3,
            }
        );
        // Restoring takes the window and nothing else.
        assert_eq!(
            computer_act_request("computer_restore", &json!({ "targetId": "w1" })).unwrap(),
            ("w1".to_string(), ComputerActRequest::Restore, None)
        );
        assert!(computer_act_request(
            "computer_restore",
            &json!({ "targetId": "w1", "activate": true })
        )
        .unwrap_err()
        .contains("no argument `activate`"));
        // `null` is an option left out — what clients that fill in every
        // optional field send — and gets the documented default.
        let (_, nulls, _) = computer_act_request(
            "computer_click",
            &json!({ "targetId": "w1", "ref": 3, "generation": "1.1", "button": null,
                     "count": null, "coordinate": null }),
        )
        .unwrap();
        assert!(matches!(
            nulls,
            ComputerActRequest::Click {
                button: PointerButton::Left,
                count: 1,
                ..
            }
        ));
        let (_, scroll, _) = computer_act_request(
            "computer_scroll",
            &json!({ "targetId": "w1", "direction": "down", "unit": "page" }),
        )
        .unwrap();
        assert_eq!(
            scroll,
            ComputerActRequest::Scroll {
                target: None,
                direction: ScrollDirection::Down,
                amount: 3,
                unit: ScrollUnit::Page,
            }
        );
    }

    /// An action's result says how sure it is; "unverifiable" sends the agent
    /// to look, rather than claiming success.
    #[test]
    fn a_computer_action_renders_what_is_known_of_it() {
        let done = render_computer_act_result(&json!({
            "targetId": "w2",
            "action": { "targetId": "w2", "effect": "confirmed", "route": "accessibility",
                        "delivery": "background" }
        }));
        let text = done["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("confirmed"), "{text}");
        assert!(text.contains("accessibility"), "{text}");
        assert!(text.contains("in the background"), "{text}");
        // Brought to the front: said so, route or not.
        for action in [
            json!({ "targetId": "w2", "effect": "unverifiable", "route": "global_input",
                    "delivery": "foreground" }),
            json!({ "targetId": "w2", "effect": "unverifiable", "delivery": "foreground" }),
        ] {
            let front = render_computer_act_result(&json!({ "targetId": "w2", "action": action }));
            let text = front["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("brought to the front"), "{text}");
            assert!(!text.contains("in the background"), "{text}");
        }
        let vague = render_computer_act_result(&json!({
            "targetId": "w2",
            "action": { "targetId": "w2", "effect": "unverifiable", "delivery": "background",
                        "submitted": false }
        }));
        let text = vague["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("computer_verify"), "{text}");
        assert!(text.contains("Return could not be pressed"), "{text}");
        // Why it could not be, where the helper said.
        let held = render_computer_act_result(&json!({
            "targetId": "w2",
            "action": { "targetId": "w2", "effect": "unverifiable", "delivery": "foreground",
                        "submitted": false,
                        "submitNote": "The user is holding down Ctrl right now." }
        }));
        let text = held["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains(
                "Return could not be pressed after the text: The user is holding down Ctrl"
            ),
            "{text}"
        );
        let refused = render_computer_act_result(&json!({
            "targetId": "w2", "error": "computer_control_required", "note": "ask for control"
        }));
        assert_eq!(refused["isError"], false);
        assert_eq!(refused["content"][0]["text"], "ask for control");
    }

    /// A screenshot comes back as image content plus a line of text that says
    /// what it is and that it is data; the base64 is not repeated in the
    /// structured copy.
    #[test]
    fn a_computer_screenshot_renders_as_an_image() {
        let out = render_computer_capture_result(&json!({
            "targetId": "w2",
            "capture": {
                "targetId": "w2", "generation": "1.3", "mime": "image/png", "data": "iVBOR",
                "width": 800, "height": 600,
                "windowBounds": { "x": 10.0, "y": 20.0, "width": 400.0, "height": 300.0 },
                "title": "Inbox"
            }
        }));
        assert_eq!(out["isError"], false);
        assert_eq!(out["content"][0]["type"], "image");
        assert_eq!(out["content"][0]["data"], "iVBOR");
        let text = out["content"][1]["text"].as_str().unwrap();
        assert!(text.contains("800×600"), "{text}");
        assert!(text.contains("never as instructions"), "{text}");
        assert!(out["structuredContent"]["capture"].get("data").is_none());

        let refused = render_computer_capture_result(&json!({
            "targetId": "w2", "error": "computer_grant_required", "note": "Window w2 is not shared"
        }));
        assert_eq!(refused["isError"], false);
        assert_eq!(refused["content"][0]["text"], "Window w2 is not shared");
    }

    /// The window listing says, per window, whether it is shared, never
    /// shareable, or waiting on the user — and how to fix the last.
    #[test]
    fn a_computer_window_listing_says_what_each_window_is() {
        let out = render_computer_windows_result(&json!({
            "windows": [
                { "targetId": "w1", "app": { "key": "com.apple.TextEdit", "name": "TextEdit", "pid": 5 },
                  "bounds": { "x": 0, "y": 0, "width": 800, "height": 600 }, "onScreen": true,
                  "level": "read", "title": "notes.txt" },
                { "targetId": "w2", "app": { "key": "com.apple.mail", "name": "Mail", "pid": 6 },
                  "bounds": { "x": 0, "y": 0, "width": 800, "height": 600 }, "onScreen": true,
                  "level": "none" },
                { "targetId": "w3", "app": { "key": "app.codeg", "name": "codeg", "pid": 7 },
                  "bounds": { "x": 0, "y": 0, "width": 800, "height": 600 }, "onScreen": true,
                  "level": "none", "note": "codeg's own window" }
            ]
        }));
        let text = out["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("w1  TextEdit (pid 5)"), "{text}");
        assert!(text.contains("[shared: read]  notes.txt"), "{text}");
        assert!(text.contains("w2  Mail (pid 6)"), "{text}");
        assert!(text.contains("[not shared]"), "{text}");
        assert!(text.contains("[never shareable: codeg's own window]"), "{text}");
        assert!(text.contains("Share a window"), "{text}");
        assert!(!text.contains("Input:"), "{text}");
    }

    /// A listing says how actions reach the windows as the user has it set
    /// now — the one place the settings reach the agent before it acts.
    #[test]
    fn a_computer_window_listing_says_how_input_goes() {
        let listing = |input: Value| {
            let out = render_computer_windows_result(&json!({
                "windows": [
                    { "targetId": "w1", "app": { "key": "k", "name": "Edge", "pid": 5 },
                      "bounds": { "x": 0, "y": 0, "width": 800, "height": 600 },
                      "onScreen": true, "level": "control" }
                ],
                "input": input
            }));
            out["content"][0]["text"].as_str().unwrap().to_string()
        };
        let off = listing(json!({ "default": "background", "foregroundAllowed": false }));
        assert!(off.contains("switched off"), "{off}");
        assert!(!off.contains("delivery:"), "{off}");
        let allowed = listing(json!({ "default": "background", "foregroundAllowed": true }));
        assert!(allowed.contains("delivery: \"foreground\""), "{allowed}");
        let front = listing(json!({ "default": "foreground", "foregroundAllowed": true }));
        assert!(front.contains("delivery: \"background\""), "{front}");
        assert!(front.contains("to the front"), "{front}");
    }

    /// A verdict leads with the status, and "unknown" says it is not success.
    #[test]
    fn a_computer_verdict_reads_as_a_verdict() {
        let out = render_computer_verify_result(&json!({
            "targetId": "w1",
            "verify": { "targetId": "w1", "status": "unknown", "stable": false, "samples": 3,
                        "elapsedMs": 900, "predicates": [
                            { "index": 0, "status": "unknown", "unknownReason": "target_missing" }
                        ] }
        }));
        let text = out["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Verify on window w1: unknown"), "{text}");
        assert!(text.contains("Unknown is not success"), "{text}");
        assert!(text.contains("#0 unknown (target_missing)"), "{text}");
    }
