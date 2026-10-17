    use super::*;
    use serde_json::json;
    use std::io::Write;
    use tempfile::tempdir;

    /// Two providers, same id, different windows — the file pi would resolve
    /// with the session's own provider, not with a tie-break.
    const TWO_PROVIDER_MODELS_JSON: &str = r#"{
      "providers": {
        "openai-compatible": { "models": [
          { "id": "qwen-flash", "contextWindow": 262144, "maxTokens": 32768 }
        ]},
        "other": { "models": [
          { "id": "qwen-flash", "contextWindow": 131072 }
        ]}
      }
    }"#;

    #[test]
    fn declared_context_window_resolves_the_session_s_own_provider() {
        let window = |provider: Option<&str>| {
            pi_declared_context_window_from(TWO_PROVIDER_MODELS_JSON, provider, "qwen-flash")
        };
        // The recorded provider decides. Taking the largest instead would have
        // told a session on `other` it had twice the room it really has.
        assert_eq!(window(Some("openai-compatible")), Some(262_144));
        assert_eq!(window(Some("other")), Some(131_072));
        // A provider this file does not define is a BUILT-IN one, whose
        // catalogue lives inside pi — nothing to read here.
        assert_eq!(window(Some("anthropic")), None);
        // Without a recorded provider, two disagreeing declarations are not an
        // answer; one agreed answer still is.
        assert_eq!(window(None), None);
        assert_eq!(
            pi_declared_context_window_from(
                r#"{"providers":{"a":{"models":[{"id":"m","contextWindow":50}]},
                                  "b":{"models":[{"id":"m","contextWindow":50}]}}}"#,
                None,
                "m"
            ),
            Some(50)
        );
        // An id the file does not list → None, so the name-based guess still runs.
        assert_eq!(
            pi_declared_context_window_from(TWO_PROVIDER_MODELS_JSON, None, "nope"),
            None
        );
    }

    /// pi's `modelFromJson` ends `contextWindow: definition.contextWindow ??
    /// 128000`, and the entry REPLACES any built-in of the same id. So a listed
    /// model that declares nothing is not unknown — it is 128K, including for
    /// an id codeg's own name table would answer differently. This is the shape
    /// codeg's Pi settings panel writes (`apply_pi_custom_model` records `id` +
    /// `name` + reasoning, never a window).
    #[test]
    fn a_listed_model_without_a_window_takes_pi_s_own_default() {
        assert_eq!(
            pi_declared_context_window_from(
                r#"{"providers":{"gs":{"baseUrl":"http://127.0.0.1:8080/v1",
                     "models":[{"id":"gpt-5.5","name":"gpt-5.5"}]}}}"#,
                Some("gs"),
                "gpt-5.5"
            ),
            Some(128_000),
            "the name table's 258K is what pi is NOT running with"
        );
    }

    /// One provider listing an id twice: pi's `applyModelsJson` upserts in file
    /// order, so the second entry overwrites the first and 100K is what pi
    /// runs with. Reading the first instead would report 9× the room.
    #[test]
    fn a_repeated_model_id_resolves_to_its_last_declaration() {
        assert_eq!(
            pi_declared_context_window_from(
                r#"{"providers":{"p":{"models":[
                     {"id":"m","contextWindow":900000},
                     {"id":"m","contextWindow":100000}
                   ]}}}"#,
                Some("p"),
                "m"
            ),
            Some(100_000)
        );
    }

    /// `modelOverrides` is the topmost layer in pi's composer, and it patches:
    /// a window there wins, its absence leaves the layer below in charge.
    #[test]
    fn a_model_override_wins_over_the_models_entry() {
        let raw = r#"{"providers":{"p":{
            "models":[{"id":"m","contextWindow":100}],
            "modelOverrides":{"m":{"contextWindow":900},"other":{"contextWindow":7}}
        }}}"#;
        assert_eq!(
            pi_declared_context_window_from(raw, Some("p"), "m"),
            Some(900)
        );
        assert_eq!(
            pi_declared_context_window_from(
                r#"{"providers":{"p":{"models":[{"id":"m","contextWindow":100}],
                     "modelOverrides":{"m":{"name":"renamed"}}}}}"#,
                Some("p"),
                "m"
            ),
            Some(100),
            "an override that declares no window must not erase the declaration"
        );
    }

    #[test]
    fn declared_context_window_declines_junk_without_panicking() {
        // Unreadable, not JSON, missing fields or a zero window must be None, never a panic.
        assert_eq!(
            pi_declared_context_window_from("", None, "qwen-flash"),
            None
        );
        assert_eq!(
            pi_declared_context_window_from("not json", None, "qwen-flash"),
            None
        );
        assert_eq!(
            pi_declared_context_window_from(r#"{"providers": "wrong"}"#, None, "qwen-flash"),
            None
        );
        // pi throws `invalid contextWindow` for `<= 0` rather than defaulting,
        // so a file it would refuse to load must not be read as a declaration.
        assert_eq!(
            pi_declared_context_window_from(
                r#"{"providers":{"p":{"models":[{"id":"m","contextWindow":0}]}}}"#,
                Some("p"),
                "m"
            ),
            None
        );
        assert_eq!(
            pi_declared_context_window_from(
                r#"{"providers":{"p":{"models":[{"id":"m","contextWindow":"wide"}]}}}"#,
                Some("p"),
                "m"
            ),
            None
        );
    }

    /// The lookup only pays off if `get_conversation` actually consults it —
    /// and only if the declaration OUTRANKS the name table, which is the whole
    /// point (a proxy in front of a known model id serves a window the table
    /// cannot know). `sample_records` runs `claude-sonnet-4-6`, which the table
    /// answers with 200K, so the two readings are distinguishable.
    #[test]
    fn a_declared_window_outranks_the_name_table_through_get_conversation() {
        let dir = tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        let sessions = agent_dir.join("sessions");
        let id = "0f3c1d2e-1111-2222-3333-444455556666";
        write_session(
            &sessions,
            "--Users-demo-my-app--",
            "2026-06-27T10-00-00_0f3c1d2e.jsonl",
            &sample_records(id),
        );

        let window = || {
            PiParser::with_base_dir(sessions.clone())
                .get_conversation(id)
                .expect("detail")
                .session_stats
                .expect("session stats")
                .context_window_max_tokens
        };
        let declare = |json: &str| std::fs::write(agent_dir.join("models.json"), json).unwrap();

        // No models.json at all: unchanged behaviour, the name table answers.
        assert_eq!(window(), Some(200_000));

        // The session runs `anthropic`/`claude-sonnet-4-6` (`model_change`
        // carries the pair). A declaration under a DIFFERENT provider is a
        // different model to pi, so the table keeps the answer.
        declare(
            r#"{"providers":{"proxy":{"baseUrl":"http://127.0.0.1:8080/v1",
                 "models":[{"id":"claude-sonnet-4-6","contextWindow":900000}]}}}"#,
        );
        assert_eq!(window(), Some(200_000));

        // Declared under the provider the session actually used — even though
        // the other provider's window is larger, and even though it is SMALLER
        // than what the name table would have guessed.
        declare(
            r#"{"providers":{
                 "proxy":{"baseUrl":"http://127.0.0.1:8080/v1",
                   "models":[{"id":"claude-sonnet-4-6","contextWindow":900000}]},
                 "anthropic":{"baseUrl":"http://127.0.0.1:9090/v1",
                   "models":[{"id":"claude-sonnet-4-6","contextWindow":150000}]}
               }}"#,
        );
        assert_eq!(window(), Some(150_000));
    }

    /// Same fixture — and same reason — as `acp::file_system_runtime`'s tests: a
    /// unix-shaped `/srv/x` has a root but NO drive prefix on Windows, so
    /// `Path::is_absolute` (the gate in [`session_dir_from_settings`]) calls it
    /// drive-relative and declines it. Hard-coding the unix shape made the
    /// honored-path assertions quietly exercise the FALLBACK there.
    #[cfg(windows)]
    const ABS_PREFIX: &str = "C:";
    #[cfg(not(windows))]
    const ABS_PREFIX: &str = "";

    /// An absolute path for the HOST platform, built from unix-style segments —
    /// as the STRING a settings file or env var carries, which is how every
    /// caller here needs it. Nothing opens these paths, so the synthetic drive
    /// letter needs no counterpart on disk.
    fn absolute_path(segments: &str) -> String {
        format!("{ABS_PREFIX}/{segments}")
    }

    #[test]
    fn resolve_sessions_dir_prefers_session_dir_env() {
        let resolved = resolve_pi_sessions_dir_from(
            Some(OsString::from("/custom/pi/sessions")),
            Some(OsString::from("/custom/pi-home")),
            Some(PathBuf::from("/home/demo")),
        );
        assert_eq!(resolved, PathBuf::from("/custom/pi/sessions"));
    }

    #[test]
    fn resolve_sessions_dir_appends_sessions_to_agent_dir() {
        let resolved = resolve_pi_sessions_dir_from(
            None,
            Some(OsString::from("/custom/pi-home")),
            Some(PathBuf::from("/home/demo")),
        );
        assert_eq!(resolved, PathBuf::from("/custom/pi-home/sessions"));
    }

    #[test]
    fn resolve_sessions_dir_defaults_to_home_dot_pi() {
        let resolved = resolve_pi_sessions_dir_from(None, None, Some(PathBuf::from("/home/demo")));
        assert_eq!(
            resolved,
            PathBuf::from("/home/demo/.pi/agent/sessions")
        );
    }

    #[test]
    fn resolve_sessions_dir_ignores_empty_env() {
        let resolved = resolve_pi_sessions_dir_from(
            Some(OsString::new()),
            Some(OsString::new()),
            Some(PathBuf::from("/home/demo")),
        );
        assert_eq!(
            resolved,
            PathBuf::from("/home/demo/.pi/agent/sessions")
        );
    }

    #[test]
    fn nonexistent_base_dir_lists_nothing() {
        let parser = PiParser::with_base_dir(PathBuf::from("/nonexistent/pi/sessions"));
        assert!(parser
            .list_conversations()
            .expect("list is infallible")
            .is_empty());
    }

    /// Write one session JSONL at
    /// `<base>/--<dashed-cwd>--/<timestamp>_<uuid>.jsonl`.
    fn write_session(base: &Path, dashed_cwd: &str, filename: &str, records: &[Value]) {
        let dir = base.join(dashed_cwd);
        std::fs::create_dir_all(&dir).expect("create session dir");
        let mut file = std::fs::File::create(dir.join(filename)).expect("create jsonl");
        for record in records {
            writeln!(file, "{}", serde_json::to_string(record).expect("serialize"))
                .expect("write line");
        }
    }

    /// A faithful v3 session: every entry links to the one before it, which is
    /// what pi actually writes (`_appendEntry` stamps `parentId = leafId` and
    /// then moves the leaf), and only the first entry has a `null` parent.
    fn sample_records(id: &str) -> Vec<Value> {
        vec![
            json!({"type":"session","version":3,"id":id,"timestamp":"2026-06-27T10:00:00.000Z","cwd":"/Users/demo/my-app"}),
            json!({"type":"session_info","id":"i1","parentId":null,"timestamp":"2026-06-27T10:00:00.100Z","name":"Build the app"}),
            json!({"type":"message","id":"m1","parentId":"i1","timestamp":"2026-06-27T10:00:01.000Z",
                   "message":{"role":"user","content":"run pnpm build"}}),
            json!({"type":"model_change","id":"mc1","parentId":"m1","timestamp":"2026-06-27T10:00:01.500Z",
                   "provider":"anthropic","modelId":"claude-sonnet-4-6"}),
            json!({"type":"message","id":"m2","parentId":"mc1","timestamp":"2026-06-27T10:00:02.000Z",
                   "message":{"role":"assistant","provider":"anthropic","model":"claude-sonnet-4-6","stopReason":"tool_use",
                     "usage":{"input":1200,"output":80,"cacheRead":4000,"cacheWrite":0,"totalTokens":5280,"cost":0.01},
                     "content":[
                       // The REAL on-disk shape: the payload key is `thinking`.
                       {"type":"thinking","thinking":"check the build first","thinkingSignature":"sig"},
                       {"type":"text","text":"Running the build now."},
                       {"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"pnpm build"}}
                     ]}}),
            json!({"type":"message","id":"m3","parentId":"m2","timestamp":"2026-06-27T10:00:09.000Z",
                   "message":{"role":"toolResult","toolCallId":"call_1","toolName":"bash",
                     "content":[{"type":"text","text":"Compiled successfully"}],"isError":false}}),
            json!({"type":"message","id":"m4","parentId":"m3","timestamp":"2026-06-27T10:00:10.000Z",
                   "message":{"role":"assistant","provider":"anthropic","model":"claude-sonnet-4-6","stopReason":"end_turn",
                     "content":[{"type":"text","text":"Build succeeded."}]}}),
        ]
    }

    #[test]
    fn parses_pi_v3_session_shape() {
        let dir = tempdir().expect("tempdir");
        let base = dir.path();
        let id = "0f3c1d2e-1111-2222-3333-444455556666";
        write_session(
            base,
            "--Users-demo-my-app--",
            "2026-06-27T10-00-00_0f3c1d2e.jsonl",
            &sample_records(id),
        );

        let parser = PiParser::with_base_dir(base.to_path_buf());

        // ---- list_conversations -------------------------------------------
        let summaries = parser.list_conversations().expect("list");
        assert_eq!(summaries.len(), 1, "one session listed");
        let summary = &summaries[0];
        assert_eq!(summary.agent_type, AgentType::Pi);
        assert_eq!(summary.id, id, "header id is the external id");
        assert_eq!(
            summary.title.as_deref(),
            Some("Build the app"),
            "session_info.name is the preferred title"
        );
        assert_eq!(
            summary.folder_path.as_deref(),
            Some("/Users/demo/my-app"),
            "cwd is read from the header, not the dashed dir name"
        );
        assert_eq!(summary.folder_name.as_deref(), Some("my-app"));
        assert_eq!(
            summary.model.as_deref(),
            Some("claude-sonnet-4-6"),
            "latest assistant/model_change model"
        );
        assert_eq!(
            summary.message_count, 3,
            "one user + two assistant turns (tool result excluded)"
        );

        // ---- get_conversation ---------------------------------------------
        let detail = parser.get_conversation(id).expect("detail");
        assert_eq!(detail.summary.agent_type, AgentType::Pi);
        assert_eq!(detail.summary.id, id);

        let has_user = detail.turns.iter().any(|t| {
            matches!(t.role, TurnRole::User)
                && t.blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("pnpm build")))
        });
        assert!(has_user, "user message becomes a User turn");

        let has_thinking = detail.turns.iter().any(|t| {
            t.blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Thinking { text } if text.contains("check the build")))
        });
        assert!(has_thinking, "assistant thinking block becomes Thinking");

        let has_assistant_text = detail.turns.iter().any(|t| {
            matches!(t.role, TurnRole::Assistant)
                && t.blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("Running the build")))
        });
        assert!(has_assistant_text, "assistant text block renders");

        // A ToolUse and a matching ToolResult, threaded by call id.
        let tool_use_id = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolUse {
                    tool_name,
                    tool_use_id,
                    input_preview,
                    ..
                } if tool_name == "bash" => {
                    assert!(input_preview
                        .as_deref()
                        .unwrap_or_default()
                        .contains("pnpm build"));
                    tool_use_id.clone()
                }
                _ => None,
            })
            .expect("a bash ToolUse");
        assert_eq!(tool_use_id, "call_1");

        let result = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id: Some(id),
                    output_preview,
                    is_error,
                    ..
                } if id == "call_1" => Some((output_preview.clone(), *is_error)),
                _ => None,
            })
            .expect("a matching ToolResult");
        assert_eq!(
            result.0.as_deref(),
            Some("Compiled successfully"),
            "the MCP block array is flattened to its text, not dumped as JSON"
        );
        assert!(!result.1, "a successful tool result is not an error");

        // The ToolUse and ToolResult must land in the same (assistant) turn.
        let same_turn = detail.turns.iter().any(|t| {
            let has_use = t.blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { tool_use_id: Some(id), .. } if id == "call_1"));
            let has_res = t.blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id: Some(id), .. } if id == "call_1"));
            has_use && has_res
        });
        assert!(same_turn, "ToolUse and ToolResult thread into one turn");

        // Assistant usage is summed into the session stats.
        let usage = detail
            .session_stats
            .as_ref()
            .and_then(|s| s.total_usage.as_ref())
            .expect("usage");
        assert_eq!(usage.input_tokens, 1200);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_input_tokens, 4000);
    }

    /// Regression: a `bash` result is an MCP block array, and serializing it
    /// painted the terminal card with the JSON source string
    /// (`[{"text":"$ next build\n…","type":"text"}]`) instead of the output.
    #[test]
    fn tool_result_content_shapes() {
        assert_eq!(
            tool_result_content_text(&json!([{"type":"text","text":"$ next build\nok\n"}]))
                .as_deref(),
            Some("$ next build\nok\n"),
            "the real on-disk shape: one text block, unwrapped"
        );
        assert_eq!(
            tool_result_content_text(&json!([
                {"type":"text","text":"part one "},
                {"type":"image","data":"…"},
                {"type":"text","text":"part two"}
            ]))
            .as_deref(),
            Some("part one part two"),
            "text blocks join with no separator; non-text blocks are skipped"
        );
        assert_eq!(
            tool_result_content_text(&json!("plain string")).as_deref(),
            Some("plain string"),
            "a bare string is accepted defensively"
        );
        assert_eq!(tool_result_content_text(&json!([])), None, "empty array");
        assert_eq!(
            tool_result_content_text(&json!([{"type":"image","data":"…"}])),
            None,
            "no text block at all — caller decides the fallback"
        );

        // `content_to_text` keeps the JSON fallback for shapes with no text.
        assert_eq!(content_to_text(None), None);
        assert_eq!(content_to_text(Some(&Value::Null)), None);
        assert_eq!(content_to_text(Some(&json!(""))), None);
        assert_eq!(
            content_to_text(Some(&json!({"exitCode": 0}))).as_deref(),
            Some(r#"{"exitCode":0}"#),
            "an unrecognized object still surfaces something"
        );
    }

    #[test]
    fn user_content_array_text_parts_are_joined() {
        let dir = tempdir().expect("tempdir");
        let base = dir.path();
        let id = "array-user-0001";
        write_session(
            base,
            "--Users-demo-app--",
            "ts_array-user-0001.jsonl",
            &[
                json!({"type":"session","version":3,"id":id,"timestamp":"2026-06-27T10:00:00.000Z","cwd":"/Users/demo/app"}),
                json!({"type":"message","id":"m1","timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":[
                         {"type":"text","text":"part one "},
                         {"type":"text","text":"part two"}
                       ]}}),
                json!({"type":"message","id":"m2","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"assistant","model":"claude-sonnet-4-6",
                         "content":[{"type":"text","text":"ok"}]}}),
            ],
        );

        let parser = PiParser::with_base_dir(base.to_path_buf());
        let detail = parser.get_conversation(id).expect("detail");
        let joined = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::Text { text } if text.contains("part one") => Some(text.clone()),
                _ => None,
            })
            .expect("user text");
        assert!(
            joined.contains("part one") && joined.contains("part two"),
            "array text parts are joined, got: {joined}"
        );
        // No session_info.name → falls back to the first user message text.
        assert_eq!(detail.summary.title.as_deref(), Some("part one part two"));
    }

    #[test]
    fn bash_execution_becomes_tool_pair_with_error_on_nonzero_exit() {
        let dir = tempdir().expect("tempdir");
        let base = dir.path();
        let id = "bash-exec-0001";
        write_session(
            base,
            "--Users-demo-app--",
            "ts_bash-exec-0001.jsonl",
            &[
                json!({"type":"session","version":3,"id":id,"timestamp":"2026-06-27T10:00:00.000Z","cwd":"/Users/demo/app"}),
                json!({"type":"message","id":"m1","timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"run it"}}),
                json!({"type":"bashExecution","id":"b1","timestamp":"2026-06-27T10:00:02.000Z",
                       "command":"exit 1","output":"boom","exitCode":1,"cancelled":false,"truncated":false}),
            ],
        );

        let parser = PiParser::with_base_dir(base.to_path_buf());
        let detail = parser.get_conversation(id).expect("detail");

        let has_bash_use = detail.turns.iter().flat_map(|t| &t.blocks).any(|b| {
            matches!(b, ContentBlock::ToolUse { tool_name, input_preview, .. }
                if tool_name == "bash" && input_preview.as_deref() == Some("exit 1"))
        });
        assert!(has_bash_use, "bashExecution becomes a bash ToolUse");

        let errored = detail.turns.iter().flat_map(|t| &t.blocks).any(|b| {
            matches!(b, ContentBlock::ToolResult { is_error, output_preview, .. }
                if *is_error && output_preview.as_deref() == Some("boom"))
        });
        assert!(errored, "a non-zero exitCode marks the bash result an error");
    }

    #[test]
    fn malformed_lines_are_skipped_without_error() {
        let dir = tempdir().expect("tempdir");
        let base = dir.path();
        let id = "robust-0001";
        let session_dir = base.join("--Users-demo-app--");
        std::fs::create_dir_all(&session_dir).expect("dir");
        let mut file =
            std::fs::File::create(session_dir.join("ts_robust-0001.jsonl")).expect("file");
        writeln!(
            file,
            "{}",
            json!({"type":"session","version":3,"id":id,"timestamp":"2026-06-27T10:00:00.000Z","cwd":"/Users/demo/app"})
        )
        .unwrap();
        writeln!(file, "{{ this is not valid json").unwrap();
        writeln!(file).unwrap(); // blank line
        writeln!(
            file,
            "{}",
            json!({"type":"message","id":"m1","timestamp":"2026-06-27T10:00:01.000Z",
                   "message":{"role":"user","content":"hello"}})
        )
        .unwrap();
        // Unknown record type must not error.
        writeln!(
            file,
            "{}",
            json!({"type":"branch_summary","id":"x","timestamp":"2026-06-27T10:00:02.000Z","summary":"noise"})
        )
        .unwrap();

        let parser = PiParser::with_base_dir(base.to_path_buf());
        let summaries = parser.list_conversations().expect("list survives malformed lines");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, id);
        let detail = parser.get_conversation(id).expect("detail survives malformed lines");
        assert!(detail.turns.iter().any(|t| matches!(t.role, TurnRole::User)));
    }

    #[test]
    fn unknown_conversation_is_not_found() {
        let dir = tempdir().expect("tempdir");
        let parser = PiParser::with_base_dir(dir.path().to_path_buf());
        assert!(matches!(
            parser.get_conversation("nope"),
            Err(ParseError::ConversationNotFound(_))
        ));
    }

    /// Collect every `Thinking` block's text across a detail.
    fn thinking_texts(detail: &ConversationDetail) -> Vec<String> {
        detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .filter_map(|b| match b {
                ContentBlock::Thinking { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Collect every `Text` block's text across a detail.
    fn text_blocks(detail: &ConversationDetail) -> Vec<String> {
        detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn parse_one(records: &[Value], id: &str) -> ConversationDetail {
        let dir = tempdir().expect("tempdir");
        write_session(dir.path(), "--Users-demo-app--", "ts_x.jsonl", records);
        PiParser::with_base_dir(dir.path().to_path_buf())
            .get_conversation(id)
            .expect("detail")
    }

    fn header(id: &str) -> Value {
        json!({"type":"session","version":3,"id":id,
               "timestamp":"2026-06-27T10:00:00.000Z","cwd":"/Users/demo/app"})
    }

    /// REGRESSION: pi's thinking block spells its payload `thinking`, not `text`
    /// (`docs/session-format.md`; 21/21 blocks in real local transcripts carry
    /// exactly `["thinking","thinkingSignature","type"]`). Reading `text` matched
    /// nothing, so every reasoning block pi had ever written was dropped on the
    /// floor — silently, because an unmatched block is simply skipped.
    #[test]
    fn thinking_block_reads_pi_field_name() {
        let id = "thinking-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"go"}}),
                json!({"type":"message","id":"m2","parentId":"m1","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"assistant","model":"m","content":[
                         {"type":"thinking","thinking":"weighing the options","thinkingSignature":"abc"},
                         {"type":"text","text":"done"}
                       ]}}),
            ],
            id,
        );
        assert_eq!(
            thinking_texts(&detail),
            vec!["weighing the options".to_string()],
        );
    }

    /// The `text` fallback exists only so a future upstream rename degrades
    /// instead of regressing to the same silent drop.
    #[test]
    fn thinking_block_falls_back_to_text_field() {
        let id = "thinking-0002";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"assistant","model":"m","content":[
                         {"type":"thinking","text":"legacy shape"}
                       ]}}),
            ],
            id,
        );
        assert_eq!(thinking_texts(&detail), vec!["legacy shape".to_string()]);
    }

    /// REGRESSION: a pi session file is a TREE. After the user rewinds and asks
    /// something else (`/tree`, or re-prompting from an earlier entry), the
    /// abandoned reply is STILL in the file — pi branches in place. pi itself
    /// renders only the path from the last entry back to the root
    /// (`_buildIndex` + `buildSessionPath`), so reading the file linearly spliced
    /// a reply the user had thrown away into the middle of the conversation.
    #[test]
    fn abandoned_branch_is_not_rendered() {
        let id = "branch-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"u1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"first question"}}),
                json!({"type":"message","id":"a1","parentId":"u1","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"first answer"}]}}),
                // ── abandoned branch off a1 ──────────────────────────────────
                json!({"type":"message","id":"u2a","parentId":"a1","timestamp":"2026-06-27T10:00:03.000Z",
                       "message":{"role":"user","content":"abandoned question"}}),
                json!({"type":"message","id":"a2a","parentId":"u2a","timestamp":"2026-06-27T10:00:04.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"abandoned answer"}]}}),
                // ── the branch the user kept, also off a1 ────────────────────
                json!({"type":"message","id":"u2b","parentId":"a1","timestamp":"2026-06-27T10:00:05.000Z",
                       "message":{"role":"user","content":"kept question"}}),
                json!({"type":"message","id":"a2b","parentId":"u2b","timestamp":"2026-06-27T10:00:06.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"kept answer"}]}}),
            ],
            id,
        );
        let texts = text_blocks(&detail);
        assert!(
            texts.iter().any(|t| t == "first answer")
                && texts.iter().any(|t| t == "kept question")
                && texts.iter().any(|t| t == "kept answer"),
            "the active path renders whole, got: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("abandoned")),
            "the abandoned branch must not render, got: {texts:?}"
        );
        assert_eq!(
            detail.summary.message_count, 4,
            "counts follow the active branch too"
        );
    }

    /// A v1 (pre-tree) session has no `id`/`parentId` at all. There is no leaf to
    /// walk from, so every record is kept in file order — the pruning must never
    /// be able to empty a conversation it cannot analyze.
    #[test]
    fn session_without_entry_ids_keeps_every_record() {
        let id = "legacy-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"one"}}),
                json!({"type":"message","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"two"}]}}),
            ],
            id,
        );
        let texts = text_blocks(&detail);
        assert!(texts.iter().any(|t| t == "one") && texts.iter().any(|t| t == "two"));
    }

    /// REGRESSION: re-editing the FIRST prompt forks at the virtual root. pi's
    /// `/tree` branches from `targetEntry.parentId`, which for the first user
    /// message is `null`, so it calls `resetLeaf()` and appends the replacement
    /// with `parentId: null` — two explicit roots, no shared parent entry. Fork
    /// detection that only counted resolvable parents saw no fork here and kept
    /// the abandoned first exchange in the transcript.
    #[test]
    fn root_reedit_fork_drops_the_abandoned_first_branch() {
        let id = "rootfork-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"u1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"the typo'd question"}}),
                json!({"type":"message","id":"a1","parentId":"u1","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"answer to the typo"}]}}),
                // The user rewinds to the very first prompt and re-asks:
                // `newLeafId = u1.parentId = null` → `resetLeaf()`.
                json!({"type":"message","id":"u1b","parentId":null,"timestamp":"2026-06-27T10:00:03.000Z",
                       "message":{"role":"user","content":"the fixed question"}}),
                json!({"type":"message","id":"a1b","parentId":"u1b","timestamp":"2026-06-27T10:00:04.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"answer to the fix"}]}}),
            ],
            id,
        );
        let texts = text_blocks(&detail);
        assert!(
            texts.iter().any(|t| t == "the fixed question")
                && texts.iter().any(|t| t == "answer to the fix"),
            "the kept root branch renders whole, got: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("typo")),
            "the abandoned root branch must not render, got: {texts:?}"
        );
        assert_eq!(
            detail.summary.message_count, 2,
            "counts follow the active root branch too"
        );
    }

    /// A file with a BROKEN chain but no fork keeps everything. Pruning exists to
    /// drop an abandoned branch; with no parent holding two children there is no
    /// branch, so an unreachable entry is a defect in the file — and silently
    /// deleting a user's messages over a defect is far worse than showing a
    /// record pi would have skipped.
    #[test]
    fn broken_chain_without_a_fork_keeps_every_record() {
        let id = "broken-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"orphaned but real"}}),
                // Points at an id that is not in the file: the walk from the leaf
                // can never reach `m1`.
                json!({"type":"message","id":"m2","parentId":"missing","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"reply"}]}}),
            ],
            id,
        );
        let texts = text_blocks(&detail);
        assert!(
            texts.iter().any(|t| t == "orphaned but real"),
            "got: {texts:?}"
        );
        assert!(texts.iter().any(|t| t == "reply"), "got: {texts:?}");
    }

    /// A `parentId` chain that loops must terminate rather than hang.
    #[test]
    fn cyclic_parent_chain_terminates() {
        let id = "cycle-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"x","parentId":"y","timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"loop a"}}),
                json!({"type":"message","id":"y","parentId":"x","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"assistant","model":"m","content":[{"type":"text","text":"loop b"}]}}),
            ],
            id,
        );
        let texts = text_blocks(&detail);
        assert!(texts.iter().any(|t| t == "loop b"));
    }

    /// pi's `getSessionName()` scans backwards, so a `/name` rename is the title.
    #[test]
    fn last_session_info_wins() {
        let id = "rename-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"session_info","id":"s1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z","name":"Old name"}),
                json!({"type":"message","id":"m1","parentId":"s1","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"user","content":"hi"}}),
                json!({"type":"session_info","id":"s2","parentId":"m1","timestamp":"2026-06-27T10:00:03.000Z","name":"New name"}),
            ],
            id,
        );
        assert_eq!(detail.summary.title.as_deref(), Some("New name"));
    }

    /// `getSessionName()` scans `getEntries()` — every PHYSICAL entry — backwards
    /// and returns at the first `session_info` it reaches. So a `/name` issued on
    /// a branch the user later abandoned still names the session (the name is
    /// session-wide, not branch-scoped), and a latest entry with an empty name
    /// CLEARS the name rather than letting an older one resurface.
    #[test]
    fn session_name_follows_pi_over_all_physical_entries() {
        let id = "rename-0002";
        // The rename lives on the abandoned branch; the active branch has none.
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"u1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":"first"}}),
                json!({"type":"session_info","id":"s1","parentId":"u1","timestamp":"2026-06-27T10:00:02.000Z","name":"Named on a dead branch"}),
                json!({"type":"message","id":"u2","parentId":"u1","timestamp":"2026-06-27T10:00:03.000Z",
                       "message":{"role":"user","content":"second"}}),
            ],
            id,
        );
        assert_eq!(
            detail.summary.title.as_deref(),
            Some("Named on a dead branch"),
            "the name is session-wide, not branch-scoped"
        );

        // An empty latest name clears it — pi returns at the first hit either way.
        let id = "rename-0003";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"session_info","id":"s1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z","name":"Old name"}),
                json!({"type":"message","id":"m1","parentId":"s1","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"user","content":"a prompt worth titling"}}),
                json!({"type":"session_info","id":"s2","parentId":"m1","timestamp":"2026-06-27T10:00:03.000Z","name":"   "}),
            ],
            id,
        );
        assert_eq!(
            detail.summary.title.as_deref(),
            Some("a prompt worth titling"),
            "an emptied name falls back to the first user text, not to the older name"
        );
    }

    /// pi's `edit` writes a one-line receipt into `content` and the real unified
    /// diff into `details.patch`. Surfacing the receipt made an edit whose LIVE
    /// card renders a full diff collapse to a sentence once reopened.
    #[test]
    fn edit_result_surfaces_the_unified_patch() {
        let id = "patch-0001";
        let patch = "--- src/a.ts\n+++ src/a.ts\n@@ -1,3 +1,3 @@\n ctx\n-old\n+new\n";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"assistant","model":"m","content":[
                         {"type":"toolCall","id":"c1","name":"edit","arguments":{"path":"src/a.ts"}}]}}),
                json!({"type":"message","id":"m2","parentId":"m1","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"toolResult","toolCallId":"c1","toolName":"edit","isError":false,
                         "content":[{"type":"text","text":"Successfully replaced 1 block(s) in src/a.ts."}],
                         "details":{"diff":"-1 old\n+1 new","patch":patch,"firstChangedLine":2}}}),
            ],
            id,
        );
        let output = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolResult { output_preview, .. } => output_preview.clone(),
                _ => None,
            })
            .expect("a tool result");
        assert_eq!(output, patch, "the unified patch replaces pi's receipt");
    }

    /// A `details.patch` that is not a unified diff must NOT displace pi's own
    /// text — the swap is only worth doing when the renderer can parse it.
    #[test]
    fn non_diff_details_patch_leaves_the_text_alone() {
        let id = "patch-0002";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"toolResult","toolCallId":"c1","toolName":"edit","isError":false,
                         "content":[{"type":"text","text":"the real output"}],
                         "details":{"patch":"not a diff at all"}}}),
            ],
            id,
        );
        let output = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolResult { output_preview, .. } => output_preview.clone(),
                _ => None,
            })
            .expect("a tool result");
        assert_eq!(output, "the real output");
    }

    /// `read` of a picture returns `[{type:"text"},{type:"image",data,mimeType}]`,
    /// and a user can attach one the same way. Both used to be dropped, leaving
    /// only pi's "Read image file […]" placeholder.
    #[test]
    fn images_survive_in_prompts_and_tool_results() {
        let id = "image-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"user","content":[
                         {"type":"text","text":"look at this"},
                         {"type":"image","data":"QUJD","mimeType":"image/jpeg"}]}}),
                json!({"type":"message","id":"m2","parentId":"m1","timestamp":"2026-06-27T10:00:02.000Z",
                       "message":{"role":"toolResult","toolCallId":"c1","toolName":"read","isError":false,
                         "content":[
                           {"type":"text","text":"Read image file [image/png]"},
                           {"type":"image","data":"REVG","mimeType":"image/png"}]}}),
            ],
            id,
        );
        let prompt_image = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::Image {
                    data, mime_type, ..
                } => Some((data.clone(), mime_type.clone())),
                _ => None,
            })
            .expect("the attached image");
        assert_eq!(prompt_image, ("QUJD".to_string(), "image/jpeg".to_string()));

        let result_images = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolResult { images, .. } if !images.is_empty() => {
                    Some(images.clone())
                }
                _ => None,
            })
            .expect("the read image");
        assert_eq!(result_images.len(), 1);
        assert_eq!(result_images[0].data, "REVG");
        assert_eq!(result_images[0].mime_type, "image/png");
    }

    /// REGRESSION: `exitCode` is `number | undefined` — an interrupted run never
    /// gets one — so defaulting a missing code to `0` painted a Ctrl-C'd command
    /// as a success. The truncation notice keeps a prefix from reading as the
    /// whole output.
    #[test]
    fn cancelled_and_truncated_bash_executions_are_marked() {
        let id = "bash-0002";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"bashExecution","id":"b1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "command":"sleep 100","output":"partial","cancelled":true,"truncated":true,
                       "fullOutputPath":"/tmp/pi-out.txt"}),
            ],
            id,
        );
        let (output, is_error) = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolResult {
                    output_preview,
                    is_error,
                    ..
                } => Some((output_preview.clone().unwrap_or_default(), *is_error)),
                _ => None,
            })
            .expect("the bash result");
        assert!(is_error, "a cancelled run is not a success");
        assert!(output.contains("partial"), "the output is kept: {output}");
        assert!(output.contains("[cancelled]"), "got: {output}");
        assert!(output.contains("/tmp/pi-out.txt"), "got: {output}");
    }

    /// A clean run stays clean — no notices, no error.
    #[test]
    fn successful_bash_execution_is_untouched() {
        let id = "bash-0003";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"bashExecution","id":"b1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "command":"echo hi","output":"hi","exitCode":0,"cancelled":false,"truncated":false}),
            ],
            id,
        );
        let (output, is_error) = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolResult {
                    output_preview,
                    is_error,
                    ..
                } => Some((output_preview.clone().unwrap_or_default(), *is_error)),
                _ => None,
            })
            .expect("the bash result");
        assert!(!is_error);
        assert_eq!(output, "hi");
    }

    /// `stopReason: "error"` is the only trace a failed provider call leaves; its
    /// content is usually empty, so the turn used to render blank or vanish.
    #[test]
    fn errored_assistant_turn_surfaces_the_error() {
        let id = "error-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "message":{"role":"assistant","model":"m","stopReason":"error",
                         "errorMessage":"429 rate limited","content":[]}}),
            ],
            id,
        );
        let texts = text_blocks(&detail);
        assert!(
            texts.iter().any(|t| t.contains("429 rate limited")),
            "got: {texts:?}"
        );
    }

    /// A compaction renders through the same provider-neutral pair every other
    /// agent's compaction uses (`_meta.contextCompaction` + a settled result).
    #[test]
    fn compaction_becomes_the_shared_divider_pair() {
        let id = "compaction-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"compaction","id":"c1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "summary":"earlier work","tokensBefore":50000}),
            ],
            id,
        );
        let meta = detail
            .turns
            .iter()
            .flat_map(|t| &t.blocks)
            .find_map(|b| match b {
                ContentBlock::ToolUse {
                    tool_name, meta, ..
                } if tool_name == "context_compaction" => meta.clone(),
                _ => None,
            })
            .expect("the compaction ToolUse");
        assert_eq!(meta.pointer("/contextCompaction/version"), Some(&json!(1)));
        assert_eq!(
            meta.pointer("/contextCompaction/preTokens"),
            Some(&json!(50000))
        );
        assert_eq!(
            meta.pointer("/contextCompaction/trigger"),
            Some(&json!("automatic"))
        );
        assert!(
            detail.turns.iter().flat_map(|t| &t.blocks).any(|b| matches!(
                b,
                ContentBlock::ToolResult { tool_use_id: Some(id), .. } if id == "c1"
            )),
            "the pair must settle or the card reads as still running"
        );
    }

    /// `/tree`'s branch recap and a displayed extension message are conversation,
    /// not bookkeeping — both become System turns.
    #[test]
    fn branch_summary_and_custom_message_become_system_turns() {
        let id = "system-0001";
        let detail = parse_one(
            &[
                header(id),
                json!({"type":"branch_summary","id":"b1","parentId":null,"timestamp":"2026-06-27T10:00:01.000Z",
                       "fromId":"x","summary":"explored approach A"}),
                json!({"type":"custom_message","id":"c1","parentId":"b1","timestamp":"2026-06-27T10:00:02.000Z",
                       "customType":"my-ext","content":"injected note","display":true}),
                json!({"type":"custom_message","id":"c2","parentId":"c1","timestamp":"2026-06-27T10:00:03.000Z",
                       "customType":"my-ext","content":"hidden note","display":false}),
            ],
            id,
        );
        let system: Vec<String> = detail
            .turns
            .iter()
            .filter(|t| matches!(t.role, TurnRole::System))
            .flat_map(|t| &t.blocks)
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(system.iter().any(|t| t == "explored approach A"));
        assert!(system.iter().any(|t| t == "injected note"));
        assert!(
            !system.iter().any(|t| t == "hidden note"),
            "`display: false` is the extension asking to stay hidden"
        );
    }

    /// pi-acp resolves `<agentDir>/settings.json` → `sessionDir` before falling
    /// back to `<agentDir>/sessions`, so a user who sets it kept a pi that
    /// resumed fine and a codeg history list that was permanently empty.
    #[test]
    fn settings_json_session_dir_is_honored() {
        let dir = tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let home = PathBuf::from(absolute_path("home/demo"));
        // Serialized, never hand-written: a Windows path's `\` is not a legal
        // JSON escape, and an unparseable file falls through to the default.
        let write_settings = |session_dir: &str| {
            std::fs::write(
                agent_dir.join("settings.json"),
                serde_json::to_string(&json!({ "sessionDir": session_dir })).expect("json"),
            )
            .expect("settings");
        };

        // Absolute.
        write_settings(&absolute_path("srv/pi-sessions"));
        assert_eq!(
            resolve_pi_sessions_dir_from(
                None,
                Some(agent_dir.clone().into_os_string()),
                Some(home.clone()),
            ),
            PathBuf::from(absolute_path("srv/pi-sessions")),
        );

        // `~`-rooted.
        write_settings("~/pi-sessions");
        assert_eq!(
            resolve_pi_sessions_dir_from(
                None,
                Some(agent_dir.clone().into_os_string()),
                Some(home.clone()),
            ),
            home.join("pi-sessions"),
        );

        // The env var still outranks it.
        assert_eq!(
            resolve_pi_sessions_dir_from(
                Some(OsString::from(absolute_path("env/sessions"))),
                Some(agent_dir.clone().into_os_string()),
                Some(home),
            ),
            PathBuf::from(absolute_path("env/sessions")),
        );
    }

    /// REGRESSION: a RELATIVE `sessionDir` must fall through to the default.
    ///
    /// pi's `normalizePath` leaves a relative value relative, and the pi process
    /// runs with the ACP session's WORKSPACE as its cwd (pi-acp's
    /// `PiRpcProcess.spawn` passes `cwd: params.cwd`) — so pi's own documented
    /// example, `{"sessionDir": ".pi/sessions"}`, writes to
    /// `<workspace>/.pi/sessions`, which this parser cannot name. Resolving it
    /// against the agent dir pointed at a directory nobody writes to AND shadowed
    /// the default, costing the user the history pi had already written to
    /// `<agent_dir>/sessions`.
    #[test]
    fn relative_settings_session_dir_falls_back_to_the_default() {
        let dir = tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let default = agent_dir.join("sessions");

        for relative in [".pi/sessions", "custom/sessions", "./sessions", "~other/x"] {
            std::fs::write(
                agent_dir.join("settings.json"),
                serde_json::to_string(&json!({ "sessionDir": relative })).expect("json"),
            )
            .expect("settings");
            assert_eq!(
                resolve_pi_sessions_dir_from(
                    None,
                    Some(agent_dir.clone().into_os_string()),
                    Some(PathBuf::from("/home/demo")),
                ),
                default,
                "relative sessionDir {relative:?} must not shadow the default"
            );
        }
    }

    /// A DRIVE-relative `sessionDir` falls through for the same reason a plain
    /// relative one does: `\srv\x` (and `/srv/x`) carries a root but no drive
    /// prefix, so Windows resolves it against whatever drive the pi process's cwd
    /// sits on — and that cwd is the per-session workspace pi-acp passes. Widening
    /// the gate from `is_absolute` to `has_root` would name a directory codeg
    /// cannot know AND shadow the history in `<agent_dir>\sessions`.
    #[test]
    #[cfg(windows)]
    fn drive_relative_settings_session_dir_falls_back_to_the_default() {
        let dir = tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let default = agent_dir.join("sessions");

        for rooted in [r"\srv\pi-sessions", "/srv/pi-sessions"] {
            std::fs::write(
                agent_dir.join("settings.json"),
                serde_json::to_string(&json!({ "sessionDir": rooted })).expect("json"),
            )
            .expect("settings");
            assert_eq!(
                resolve_pi_sessions_dir_from(
                    None,
                    Some(agent_dir.clone().into_os_string()),
                    Some(PathBuf::from(absolute_path("home/demo"))),
                ),
                default,
                "drive-relative sessionDir {rooted:?} must not shadow the default"
            );
        }
    }

    /// pi expands a tilde only for a bare `~` and a `~/` prefix
    /// (`normalizePath`); `~other/path` is an ordinary relative path to pi, not
    /// the home of a user called `other`. Applied to both env overrides, which pi
    /// reads through `expandTildePath`.
    #[test]
    fn tilde_expansion_matches_pi_normalize_path() {
        let home = PathBuf::from("/home/demo");
        assert_eq!(expand_pi_tilde("~", Some(&home)), home);
        assert_eq!(
            expand_pi_tilde("~/pi-sessions", Some(&home)),
            home.join("pi-sessions")
        );
        assert_eq!(
            expand_pi_tilde("~other/path", Some(&home)),
            PathBuf::from("~other/path"),
            "not a tilde path to pi — left verbatim"
        );
        assert_eq!(
            expand_pi_tilde("/abs/path", Some(&home)),
            PathBuf::from("/abs/path")
        );
        assert_eq!(
            expand_pi_tilde("~/x", None),
            PathBuf::from("~/x"),
            "no home to expand against"
        );

        // The env overrides go through it too.
        assert_eq!(
            resolve_pi_sessions_dir_from(Some(OsString::from("~/env-sessions")), None, Some(home)),
            PathBuf::from("/home/demo/env-sessions"),
        );
    }

    /// A settings file that is missing, unreadable as JSON, or carries no usable
    /// `sessionDir` must fall through to the default rather than to nothing.
    #[test]
    fn unusable_settings_json_falls_back_to_default() {
        let dir = tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let default = agent_dir.join("sessions");

        for contents in ["{ not json", "[]", "{}", r#"{"sessionDir": "  "}"#] {
            std::fs::write(agent_dir.join("settings.json"), contents).expect("settings");
            assert_eq!(
                resolve_pi_sessions_dir_from(
                    None,
                    Some(agent_dir.clone().into_os_string()),
                    Some(PathBuf::from("/home/demo")),
                ),
                default,
                "contents: {contents}"
            );
        }
    }
