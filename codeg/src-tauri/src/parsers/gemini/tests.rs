    use super::resolve_gemini_base_dir_from;
    use super::GeminiParser;
    use crate::models::{ContentBlock, TurnRole};
    use crate::parsers::AgentParser;
    use chrono::{DateTime, Utc};
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_gemini_session_detail_from_chat_json() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time ok")
            .as_nanos();
        let base: PathBuf = env::temp_dir().join(format!("codeg-gemini-test-{nanos}"));
        let chats_dir = base.join("tmp").join("codeg").join("chats");
        fs::create_dir_all(&chats_dir).expect("create chat dir");
        fs::write(
            base.join("tmp").join("codeg").join(".project_root"),
            "/Users/test/workspace/demo",
        )
        .expect("write project root");

        let file_path = chats_dir.join("session-2026-03-02T04-30-32c7d221.json");
        let content = r#"{
  "sessionId": "32c7d221-0553-46c8-ba50-e664719cae7f",
  "projectHash": "abc",
  "startTime": "2026-03-02T04:30:20.796Z",
  "lastUpdated": "2026-03-02T04:33:13.631Z",
  "messages": [
    {
      "id": "u1",
      "timestamp": "2026-03-02T04:30:20.796Z",
      "type": "user",
      "content": [{"text": "你会做什么"}]
    },
    {
      "id": "a1",
      "timestamp": "2026-03-02T04:33:13.631Z",
      "type": "gemini",
      "content": "我是一个助手",
      "toolCalls": [
        {
          "id": "cli_help-1",
          "name": "cli_help",
          "args": {"question": "你会做什么"},
          "resultDisplay": "ok",
          "status": "success"
        }
      ],
      "tokens": {"input": 12, "output": 34, "cached": 5},
      "model": "gemini-3.1-pro-preview"
    }
  ]
}"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0].id,
            "32c7d221-0553-46c8-ba50-e664719cae7f".to_string()
        );

        let detail = parser
            .get_conversation("32c7d221-0553-46c8-ba50-e664719cae7f")
            .expect("get conversation");
        assert_eq!(detail.turns.len(), 2);
        assert_eq!(
            detail.summary.folder_path.as_deref(),
            Some("/Users/test/workspace/demo")
        );
        assert!(detail.session_stats.is_some());
        let stats = detail.session_stats.expect("session stats");
        // `input: 12` is `promptTokenCount` and ALREADY contains `cached: 5`,
        // so the prompt window holds 12 tokens, not 12 + 5. The old expectation
        // of 51 additionally folded in the 34 output tokens, which gemini itself
        // never counts against the context window.
        assert_eq!(stats.context_window_used_tokens, Some(12));
        assert_eq!(stats.context_window_max_tokens, Some(1_048_576));
        let percent = stats
            .context_window_usage_percent
            .expect("context window percent");
        assert!((percent - (12.0 / 1_048_576.0) * 100.0).abs() < 1e-9);
        // 7 uncached input + 5 cached + 34 output.
        assert_eq!(stats.total_tokens, Some(46));

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn gemini_prefers_update_topic_title_over_first_user_message() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time ok")
            .as_nanos();
        let base: PathBuf = env::temp_dir().join(format!("codeg-gemini-topic-{nanos}"));
        let chats_dir = base.join("tmp").join("codeg").join("chats");
        fs::create_dir_all(&chats_dir).expect("create chat dir");
        fs::write(
            base.join("tmp").join("codeg").join(".project_root"),
            "/Users/test/workspace/demo",
        )
        .expect("write project root");

        let file_path = chats_dir.join("session-2026-03-02T04-30-topic.json");
        let content = r#"{
  "sessionId": "topic-session",
  "startTime": "2026-03-02T04:30:20.796Z",
  "lastUpdated": "2026-03-02T04:33:13.631Z",
  "messages": [
    {"id": "u1", "timestamp": "2026-03-02T04:30:20.796Z", "type": "user", "content": [{"text": "first user prompt"}]},
    {"id": "a1", "timestamp": "2026-03-02T04:31:00.000Z", "type": "gemini", "content": "ok", "toolCalls": [
      {"id": "ut-1", "name": "update_topic", "args": {"title": "Stale Topic", "strategic_intent": "x"}, "status": "success"}
    ]},
    {"id": "a2", "timestamp": "2026-03-02T04:32:00.000Z", "type": "gemini", "content": "ok", "toolCalls": [
      {"id": "ut-2", "name": "update_topic", "args": {"title": "Analyzing Commit", "strategic_intent": "y"}, "status": "success"}
    ]}
  ]
}"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(summaries.len(), 1);
        // Newest update_topic title wins, over the stale topic and the first
        // user message.
        assert_eq!(summaries[0].title.as_deref(), Some("Analyzing Commit"));
    }

    #[test]
    fn gemini_falls_back_to_real_prompt_skipping_session_context() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time ok")
            .as_nanos();
        let base: PathBuf = env::temp_dir().join(format!("codeg-gemini-ctx-{nanos}"));
        let chats_dir = base.join("tmp").join("codeg").join("chats");
        fs::create_dir_all(&chats_dir).expect("create chat dir");
        fs::write(
            base.join("tmp").join("codeg").join(".project_root"),
            "/Users/test/workspace/demo",
        )
        .expect("write project root");

        let file_path = chats_dir.join("session-2026-03-02T04-30-ctx.json");
        let content = r#"{
  "sessionId": "ctx-session",
  "startTime": "2026-03-02T04:30:20.796Z",
  "lastUpdated": "2026-03-02T04:33:13.631Z",
  "messages": [
    {"id": "u0", "timestamp": "2026-03-02T04:30:20.796Z", "type": "user", "content": [{"text": "<session_context>\nThis is the Gemini CLI. Setting up context.\n</session_context>"}]},
    {"id": "u1", "timestamp": "2026-03-02T04:30:21.000Z", "type": "user", "content": [{"text": "the real first prompt"}]}
  ]
}"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(summaries.len(), 1);
        // The injected <session_context> envelope is skipped so the real prompt
        // becomes the title.
        assert_eq!(summaries[0].title.as_deref(), Some("the real first prompt"));
    }

    #[test]
    fn parses_gemini_session_detail_from_jsonl_chat_log() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time ok")
            .as_nanos();
        let base: PathBuf = env::temp_dir().join(format!("codeg-gemini-jsonl-test-{nanos}"));
        let chats_dir = base.join("tmp").join("codeg-jsonl").join("chats");
        fs::create_dir_all(&chats_dir).expect("create chat dir");
        fs::write(
            base.join("tmp").join("codeg-jsonl").join(".project_root"),
            "/Users/test/workspace/jsonl-demo",
        )
        .expect("write project root");

        let file_path = chats_dir.join("session-2026-05-11T13-22-jsonl.jsonl");
        let content = r#"{"kind":"main","sessionId":"jsonl-session-1","projectHash":"abc","startTime":"2026-05-11T13:22:43.000Z","lastUpdated":"2026-05-11T13:22:43.000Z"}
{"kind":"main","sessionId":"jsonl-session-1","projectHash":"abc","startTime":"2026-05-11T13:22:43.000Z","lastUpdated":"2026-05-11T13:22:44.000Z"}
{"id":"u1","timestamp":"2026-05-11T13:23:16.870Z","type":"user","content":[{"text":"hello from jsonl"}]}
{"$set":{"lastUpdated":"2026-05-11T13:23:16.870Z"}}
{"id":"a1","timestamp":"2026-05-11T13:23:23.568Z","type":"gemini","content":"partial answer","model":"gemini-2.5-pro"}
{"id":"a1","timestamp":"2026-05-11T13:23:23.568Z","type":"gemini","content":"final answer","toolCalls":[{"id":"read-1","name":"read_file","args":{"path":"README.md"},"resultDisplay":{"summary":"Read README.md"},"status":"success"}],"tokens":{"input":10,"output":20,"cached":3},"model":"gemini-2.5-pro"}
"#;
        fs::write(&file_path, content).expect("write jsonl chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "jsonl-session-1");
        assert_eq!(summaries[0].message_count, 2);
        assert_eq!(summaries[0].title.as_deref(), Some("hello from jsonl"));
        assert_eq!(
            summaries[0].folder_path.as_deref(),
            Some("/Users/test/workspace/jsonl-demo")
        );

        let detail = parser
            .get_conversation("jsonl-session-1")
            .expect("get conversation");
        assert_eq!(detail.turns.len(), 2);
        assert!(matches!(detail.turns[0].role, TurnRole::User));
        assert!(matches!(detail.turns[1].role, TurnRole::Assistant));

        let assistant = &detail.turns[1];
        assert_eq!(assistant.model.as_deref(), Some("gemini-2.5-pro"));
        let text_blocks: Vec<&str> = assistant
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text_blocks, vec!["final answer"]);
        assert!(assistant.blocks.iter().any(|block| matches!(
            block,
            ContentBlock::ToolResult {
                output_preview: Some(output),
                is_error: false,
                ..
            } if output == "Read README.md"
        )));
        let stats = detail.session_stats.expect("session stats");
        // 7 uncached input (10 prompt − 3 cached) + 3 cached + 20 output.
        assert_eq!(stats.total_tokens, Some(30));

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn parse_detail_completion_time_uses_assistant_timestamp_not_next_message_gap() {
        // Regression: Gemini's `duration_ms` heuristic is `next_msg.ts -
        // assistant.ts`, which means a quick user follow-up makes the gap
        // meaningless as a duration. completed_at must NOT be derived from
        // that gap; it must reflect when the assistant message was logged.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time ok")
            .as_nanos();
        let base: PathBuf = env::temp_dir().join(format!("codeg-gemini-completed-{nanos}"));
        let chats_dir = base.join("tmp").join("codeg").join("chats");
        fs::create_dir_all(&chats_dir).expect("create chat dir");

        let file_path = chats_dir.join("session-completed.json");
        let content = r#"{
  "sessionId": "completed-1",
  "projectHash": "abc",
  "startTime": "2026-03-02T04:30:00.000Z",
  "lastUpdated": "2026-03-02T04:30:50.000Z",
  "messages": [
    {"id": "u1", "timestamp": "2026-03-02T04:30:00.000Z", "type": "user", "content": [{"text": "ping"}]},
    {"id": "a1", "timestamp": "2026-03-02T04:30:02.000Z", "type": "gemini", "content": "pong", "model": "gemini-3.1-pro-preview"},
    {"id": "u2", "timestamp": "2026-03-02T04:30:50.000Z", "type": "user", "content": [{"text": "follow-up after 48s"}]}
  ]
}"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let detail = parser
            .get_conversation("completed-1")
            .expect("get conversation");

        let assistant = detail
            .turns
            .iter()
            .find(|t| matches!(t.role, TurnRole::Assistant))
            .expect("assistant turn");
        let completed_at = assistant.completed_at.expect("completed_at populated");
        let expected = "2026-03-02T04:30:02.000Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(completed_at, expected);
        // The naive `timestamp + (next_user.ts - assistant.ts)` would land
        // on the second user message timestamp.
        let wrong = "2026-03-02T04:30:50.000Z".parse::<DateTime<Utc>>().unwrap();
        assert_ne!(completed_at, wrong);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn gemini_cli_home_env_overrides_user_home() {
        let resolved = resolve_gemini_base_dir_from(
            Some(std::ffi::OsString::from("/tmp/gemini-home")),
            Some(PathBuf::from("/Users/default")),
        );
        assert_eq!(resolved, PathBuf::from("/tmp/gemini-home/.gemini"));
    }

    #[test]
    fn gemini_defaults_to_home_dot_gemini() {
        let resolved = resolve_gemini_base_dir_from(None, Some(PathBuf::from("/Users/default")));
        assert_eq!(resolved, PathBuf::from("/Users/default/.gemini"));
    }

    /// Build a `<base>/tmp/<alias>/chats/` directory with a `.project_root`,
    /// returning the base so the caller can point a parser at it.
    fn chat_fixture(tag: &str, alias: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time ok")
            .as_nanos();
        let base: PathBuf = env::temp_dir().join(format!("codeg-gemini-{tag}-{nanos}"));
        let chats_dir = base.join("tmp").join(alias).join("chats");
        fs::create_dir_all(&chats_dir).expect("create chat dir");
        fs::write(
            base.join("tmp").join(alias).join(".project_root"),
            "/Users/test/workspace/demo",
        )
        .expect("write project root");
        base
    }

    fn message_texts(detail: &crate::models::ConversationDetail) -> Vec<String> {
        detail
            .turns
            .iter()
            .flat_map(|turn| turn.blocks.iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// The shape every ACP-driven session actually has: the messages live ONLY
    /// inside a `$set.messages` array. This used to parse as zero messages.
    #[test]
    fn jsonl_rebuilds_history_from_set_messages() {
        let base = chat_fixture("setmsgs", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-2026-09-09T06-29-setmsgs.jsonl");
        let content = r#"{"sessionId":"set-msgs-1","projectHash":"abc","startTime":"2026-09-09T06:29:23.000Z","lastUpdated":"2026-09-09T06:29:23.000Z","kind":"main"}
{"$set":{"messages":[{"id":"u1","timestamp":"2026-09-09T06:29:24.000Z","type":"user","content":[{"text":"hello from set"}]},{"id":"a1","timestamp":"2026-09-09T06:29:25.000Z","type":"gemini","content":"hi there","model":"gemini-3-pro"}],"lastUpdated":"2026-09-09T06:29:25.000Z"}}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].message_count, 2);
        assert_eq!(summaries[0].title.as_deref(), Some("hello from set"));

        let detail = parser
            .get_conversation("set-msgs-1")
            .expect("get conversation");
        assert_eq!(message_texts(&detail), vec!["hello from set", "hi there"]);

        let _ = fs::remove_dir_all(base);
    }

    /// `$set.messages` REPLACES the history wholesale (upstream clears the map
    /// first), and bare records appended afterwards extend it.
    #[test]
    fn jsonl_set_messages_replaces_then_appends() {
        let base = chat_fixture("replace", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-replace.jsonl");
        let content = r#"{"sessionId":"replace-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z"}
{"id":"old","timestamp":"2026-09-09T06:00:01.000Z","type":"user","content":[{"text":"dropped by the rewrite"}]}
{"$set":{"messages":[{"id":"u1","timestamp":"2026-09-09T06:00:02.000Z","type":"user","content":[{"text":"kept one"}]}]}}
{"id":"a1","timestamp":"2026-09-09T06:00:03.000Z","type":"gemini","content":"appended after"}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let detail = parser.get_conversation("replace-1").expect("conversation");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(message_texts(&detail), vec!["kept one", "appended after"]);
    }

    /// `$rewindTo` drops the target and everything after it.
    #[test]
    fn jsonl_rewind_truncates_from_target() {
        let base = chat_fixture("rewind", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-rewind.jsonl");
        let content = r#"{"sessionId":"rewind-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z"}
{"id":"u1","timestamp":"2026-09-09T06:00:01.000Z","type":"user","content":[{"text":"first"}]}
{"id":"a1","timestamp":"2026-09-09T06:00:02.000Z","type":"gemini","content":"answer one"}
{"id":"u2","timestamp":"2026-09-09T06:00:03.000Z","type":"user","content":[{"text":"regretted question"}]}
{"id":"a2","timestamp":"2026-09-09T06:00:04.000Z","type":"gemini","content":"regretted answer"}
{"$rewindTo":"u2"}
{"id":"u3","timestamp":"2026-09-09T06:00:05.000Z","type":"user","content":[{"text":"second attempt"}]}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let detail = parser.get_conversation("rewind-1").expect("conversation");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(
            message_texts(&detail),
            vec!["first", "answer one", "second attempt"]
        );
    }

    /// A rewind onto an id that is no longer present clears the WHOLE history
    /// upstream, rather than leaving it untouched.
    #[test]
    fn jsonl_rewind_to_unknown_id_clears_everything() {
        let base = chat_fixture("rewind-miss", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-rewind-miss.jsonl");
        let content = r#"{"sessionId":"rewind-miss-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z"}
{"id":"u1","timestamp":"2026-09-09T06:00:01.000Z","type":"user","content":[{"text":"first"}]}
{"$rewindTo":"never-written"}
{"id":"u2","timestamp":"2026-09-09T06:00:02.000Z","type":"user","content":[{"text":"only survivor"}]}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let detail = parser
            .get_conversation("rewind-miss-1")
            .expect("conversation");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(message_texts(&detail), vec!["only survivor"]);
    }

    /// A repeated id REPLACES the record in place: the position is the first
    /// one seen, and fields the newer record omits do NOT survive.
    #[test]
    fn jsonl_repeated_id_replaces_whole_record_in_place() {
        let base = chat_fixture("replace-id", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-replace-id.jsonl");
        let content = r#"{"sessionId":"replace-id-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z"}
{"id":"a1","timestamp":"2026-09-09T06:00:01.000Z","type":"gemini","content":"draft","toolCalls":[{"id":"ghost-1","name":"read_file","args":{"path":"x"},"status":"success"}]}
{"id":"u1","timestamp":"2026-09-09T06:00:02.000Z","type":"user","content":[{"text":"later user turn"}]}
{"id":"a1","timestamp":"2026-09-09T06:00:03.000Z","type":"gemini","content":"final"}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let detail = parser
            .get_conversation("replace-id-1")
            .expect("conversation");
        let _ = fs::remove_dir_all(&base);

        // Position of `a1` is where it FIRST appeared, ahead of `u1`.
        assert_eq!(message_texts(&detail), vec!["final", "later user turn"]);
        // The tool call from the superseded record must not linger.
        assert!(
            !detail
                .turns
                .iter()
                .flat_map(|turn| turn.blocks.iter())
                .any(|block| matches!(block, ContentBlock::ToolUse { .. })),
            "stale toolCalls survived a whole-record replace"
        );
    }

    /// One unparseable line must not take the whole transcript down with it.
    #[test]
    fn jsonl_skips_malformed_lines() {
        let base = chat_fixture("badline", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-badline.jsonl");
        let content = r#"{"sessionId":"badline-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z"}
{"id":"u1","timestamp":"2026-09-09T06:00:01.000Z","type":"user","content":[{"text":"before the tear"}]}
{"id":"a1","timestamp":"2026-09-09T06:00:02.000Z","type":"gemini","content":"tru
{"id":"a2","timestamp":"2026-09-09T06:00:03.000Z","type":"gemini","content":"after the tear"}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let detail = parser.get_conversation("badline-1").expect("conversation");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(
            message_texts(&detail),
            vec!["before the tear", "after the tear"]
        );
    }

    /// A `.jsonl` file that is really one pretty-printed JSON object falls back
    /// to the legacy single-record reader.
    #[test]
    fn jsonl_falls_back_to_legacy_single_object() {
        let base = chat_fixture("legacy", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-legacy.jsonl");
        let content = r#"{
  "sessionId": "legacy-1",
  "startTime": "2026-09-09T06:00:00.000Z",
  "lastUpdated": "2026-09-09T06:00:01.000Z",
  "messages": [
    {"id": "u1", "timestamp": "2026-09-09T06:00:00.000Z", "type": "user", "content": [{"text": "legacy prompt"}]}
  ]
}"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let detail = parser.get_conversation("legacy-1").expect("conversation");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(message_texts(&detail), vec!["legacy prompt"]);
    }

    /// A brand-new session whose history never changed carries metadata only —
    /// `updateMessagesFromHistory` writes nothing when nothing moved. It must
    /// still list, just with no messages.
    #[test]
    fn jsonl_metadata_only_session_still_lists() {
        let base = chat_fixture("empty", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-empty.jsonl");
        let content = r#"{"sessionId":"empty-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z","kind":"main"}
{"$set":{"lastUpdated":"2026-09-09T06:00:05.000Z"}}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "empty-1");
        assert_eq!(summaries[0].message_count, 0);
    }

    /// `$set.summary` outranks both `update_topic` and the first user message.
    #[test]
    fn jsonl_prefers_metadata_summary_for_title() {
        let base = chat_fixture("summary", "codeg");
        let file_path = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("session-summary.jsonl");
        let content = r#"{"sessionId":"summary-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z"}
{"id":"u1","timestamp":"2026-09-09T06:00:01.000Z","type":"user","content":[{"text":"the raw prompt"}]}
{"id":"a1","timestamp":"2026-09-09T06:00:02.000Z","type":"gemini","content":"ok","toolCalls":[{"id":"ut-1","name":"update_topic","args":{"title":"Topic Title"},"status":"success"}]}
{"$set":{"summary":"Authoritative Session Summary"}}
"#;
        fs::write(&file_path, content).expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(
            summaries[0].title.as_deref(),
            Some("Authoritative Session Summary")
        );
    }

    /// Subagent transcripts live one directory deeper and have no `session-`
    /// prefix. They must be discovered, attributed to the right project, and
    /// marked as children so they never import as root conversations.
    #[test]
    fn discovers_subagent_transcripts_as_children() {
        let base = chat_fixture("subagent", "codeg");
        let nested = base
            .join("tmp")
            .join("codeg")
            .join("chats")
            .join("parent-session-id");
        fs::create_dir_all(&nested).expect("create nested chat dir");
        fs::write(
            nested.join("11111111-2222-3333-4444-555555555555.jsonl"),
            r#"{"sessionId":"sub-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z","kind":"subagent"}
{"$set":{"messages":[{"id":"u1","timestamp":"2026-09-09T06:00:01.000Z","type":"user","content":[{"text":"investigate the auth flow"}]}]}}
"#,
        )
        .expect("write subagent chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        let detail = parser.get_conversation("sub-1").expect("conversation");
        let _ = fs::remove_dir_all(&base);

        let sub = summaries
            .iter()
            .find(|s| s.id == "sub-1")
            .expect("subagent session listed");
        assert_eq!(sub.parent_id.as_deref(), Some("parent-session-id"));
        // The project alias is the directory ABOVE `chats`, not `chats` itself.
        assert_eq!(
            sub.folder_path.as_deref(),
            Some("/Users/test/workspace/demo")
        );
        assert_eq!(message_texts(&detail), vec!["investigate the auth flow"]);
    }

    /// `$set.directories` backs up folder resolution when the alias is unknown
    /// to both `.project_root` and `projects.json`.
    #[test]
    fn falls_back_to_metadata_directories_for_folder_path() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time ok")
            .as_nanos();
        let base: PathBuf = env::temp_dir().join(format!("codeg-gemini-dirs-{nanos}"));
        let chats_dir = base.join("tmp").join("unmapped-alias").join("chats");
        fs::create_dir_all(&chats_dir).expect("create chat dir");
        // Deliberately NO `.project_root` and no `projects.json`.
        fs::write(
            chats_dir.join("session-dirs.jsonl"),
            r#"{"sessionId":"dirs-1","projectHash":"abc","startTime":"2026-09-09T06:00:00.000Z","lastUpdated":"2026-09-09T06:00:00.000Z"}
{"$set":{"directories":["/Users/test/workspace/from-metadata"]}}
{"id":"u1","timestamp":"2026-09-09T06:00:01.000Z","type":"user","content":[{"text":"hi"}]}
"#,
        )
        .expect("write chat file");

        let parser = GeminiParser::with_base_dir(base.clone());
        let summaries = parser.list_conversations().expect("list conversations");
        let _ = fs::remove_dir_all(&base);

        assert_eq!(
            summaries[0].folder_path.as_deref(),
            Some("/Users/test/workspace/from-metadata")
        );
    }

    #[test]
    fn usage_treats_cached_as_a_subset_of_input() {
        let message = serde_json::json!({
            "tokens": {"input": 100, "output": 20, "cached": 40, "thoughts": 7, "tool": 3}
        });
        let usage = GeminiParser::parse_usage(&message).expect("usage");
        // 100 prompt tokens of which 40 were cache hits.
        assert_eq!(usage.input_tokens, 60);
        assert_eq!(usage.cache_read_input_tokens, 40);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        // candidates + thoughts + tool.
        assert_eq!(usage.output_tokens, 30);
    }

    #[test]
    fn usage_derives_output_from_total_when_generated_side_is_absent() {
        let message = serde_json::json!({
            "tokens": {"input": 100, "cached": 40, "total": 130}
        });
        let usage = GeminiParser::parse_usage(&message).expect("usage");
        assert_eq!(usage.input_tokens, 60);
        assert_eq!(usage.cache_read_input_tokens, 40);
        // `total - input`; `input` already carries `cached`, so it is not
        // subtracted twice.
        assert_eq!(usage.output_tokens, 30);
    }

    #[test]
    fn parses_user_inline_image_block() {
        let message = serde_json::json!({
            "content": [
                {"text": "这是什么"},
                {"inlineData": {"mimeType": "image/jpeg", "data": "QUJD"}}
            ]
        });

        let blocks = GeminiParser::parse_user_blocks(&message);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "这是什么"));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Image { data, mime_type, uri }
            if data == "QUJD" && mime_type == "image/jpeg" && uri.is_none()
        ));
    }

    #[test]
    fn parses_user_data_uri_image_block() {
        let message = serde_json::json!({
            "content": [
                {
                    "inlineData": {
                        "data": "data:image/png;base64,QUJD"
                    }
                }
            ]
        });

        let blocks = GeminiParser::parse_user_blocks(&message);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Image { data, mime_type, uri }
            if data == "QUJD" && mime_type == "image/png" && uri.is_none()
        ));
    }
