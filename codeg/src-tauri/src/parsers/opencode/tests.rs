    use super::{extract_opencode_file_image, resolve_xdg_data_home};
    use crate::models::ContentBlock;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    use std::path::PathBuf;

    #[test]
    fn xdg_data_home_env_overrides_home_fallback() {
        let resolved = resolve_xdg_data_home(
            Some(std::ffi::OsString::from("/tmp/xdg-data")),
            Some(PathBuf::from("/Users/default")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/tmp/xdg-data")));
    }

    #[test]
    fn xdg_data_home_falls_back_to_home_local_share() {
        let resolved = resolve_xdg_data_home(None, Some(PathBuf::from("/Users/default")));
        assert_eq!(resolved, Some(PathBuf::from("/Users/default/.local/share")));
    }

    #[test]
    fn parses_opencode_user_image_file_part_from_data_uri() {
        let value = serde_json::json!({
            "type": "file",
            "mime": "image/jpeg",
            "filename": "avatar.jpg",
            "url": "data:image/jpeg;base64,QUJD"
        });

        let block = extract_opencode_file_image(&value);
        assert!(matches!(
            block,
            Some(ContentBlock::Image { data, mime_type, uri })
            if data == "QUJD" && mime_type == "image/jpeg" && uri.as_deref() == Some("avatar.jpg")
        ));
    }

    // The payloads below are verbatim `part.data` rows captured from a real
    // opencode 1.18.14 run, trimmed only where a field is irrelevant here.

    fn normalized(raw_tool: &str, state: serde_json::Value) -> super::NormalizedToolCall {
        super::normalize_tool_call(raw_tool, Some(&state))
    }

    fn input_of(call: &super::NormalizedToolCall) -> serde_json::Value {
        serde_json::from_str(call.input_preview.as_deref().expect("input preview"))
            .expect("input preview is JSON")
    }

    #[test]
    fn edit_input_becomes_canonical_and_carries_real_start_line() {
        let call = normalized(
            "edit",
            serde_json::json!({
                "status": "completed",
                "input": {
                    "filePath": "src/app.ts",
                    "oldString": "hello ${name}",
                    "newString": "Hello, ${name}!"
                },
                "output": "Edit applied successfully.",
                "metadata": {
                    "diff": "Index: /p/src/app.ts\n===\n--- /p/src/app.ts\n+++ /p/src/app.ts\n@@ -12,5 +12,5 @@\n-  return `hello ${name}`\n+  return `Hello, ${name}!`\n",
                    "filediff": {
                        "file": "/p/src/app.ts",
                        "patch": "…",
                        "additions": 1,
                        "deletions": 1
                    }
                }
            }),
        );

        assert_eq!(call.tool_name, "edit");
        assert!(!call.is_error);
        assert_eq!(
            input_of(&call),
            serde_json::json!({
                "file_path": "src/app.ts",
                "old_string": "hello ${name}",
                "new_string": "Hello, ${name}!",
                "_start_line": 12,
            })
        );
    }

    #[test]
    fn edit_falls_back_to_metadata_file_path_and_v2_path_key() {
        let from_v2 = normalized(
            "edit",
            serde_json::json!({
                "status": "completed",
                "input": { "path": "src/app.ts", "oldString": "a", "newString": "b", "replaceAll": true },
            }),
        );
        assert_eq!(
            input_of(&from_v2),
            serde_json::json!({
                "file_path": "src/app.ts",
                "old_string": "a",
                "new_string": "b",
                "replace_all": true,
            })
        );

        let from_metadata = normalized(
            "edit",
            serde_json::json!({
                "status": "completed",
                "input": { "oldString": "a", "newString": "b" },
                "metadata": { "filediff": { "file": "/abs/src/app.ts" } },
            }),
        );
        assert_eq!(
            input_of(&from_metadata)["file_path"],
            serde_json::json!("/abs/src/app.ts")
        );
    }

    #[test]
    fn edit_strings_stay_byte_exact_including_whitespace() {
        // Indentation-only change: trimming either side would collapse both
        // to "return value" — an identical pair, i.e. an empty diff.
        let call = normalized(
            "edit",
            serde_json::json!({
                "status": "completed",
                "input": {
                    "filePath": "src/app.ts",
                    "oldString": "  return value\n",
                    "newString": "    return value\n"
                },
            }),
        );

        let input = input_of(&call);
        assert_eq!(input["old_string"], serde_json::json!("  return value\n"));
        assert_eq!(input["new_string"], serde_json::json!("    return value\n"));
    }

    #[test]
    fn empty_old_string_is_kept_as_the_create_file_form() {
        let call = normalized(
            "edit",
            serde_json::json!({
                "status": "completed",
                "input": { "filePath": "src/new.ts", "oldString": "", "newString": "body\n" },
            }),
        );

        let input = input_of(&call);
        assert_eq!(input["old_string"], serde_json::json!(""));
        assert_eq!(input["new_string"], serde_json::json!("body\n"));
    }

    #[test]
    fn patch_text_keeps_significant_leading_context_spaces() {
        let patch = " context line\n-old\n+new\n";
        let call = normalized(
            "apply_patch",
            serde_json::json!({
                "status": "completed",
                "input": { "patchText": patch },
            }),
        );

        assert_eq!(call.input_preview.as_deref(), Some(patch));
    }

    #[test]
    fn failed_tool_call_reports_its_error_message() {
        let call = normalized(
            "edit",
            serde_json::json!({
                "status": "error",
                "input": { "filePath": "src/missing.ts", "oldString": "nope", "newString": "yep" },
                "error": "File /p/src/missing.ts not found",
                "time": { "start": 1, "end": 2 }
            }),
        );

        assert!(call.is_error);
        assert_eq!(
            call.output_preview.as_deref(),
            Some("File /p/src/missing.ts not found")
        );
    }

    #[test]
    fn invalid_tool_call_is_flagged_as_an_error() {
        let call = normalized(
            "invalid",
            serde_json::json!({
                "status": "completed",
                "input": { "tool": "edit", "error": "Missing key at [\"filePath\"]" },
                "output": "The arguments provided to the tool are invalid: …",
            }),
        );

        assert!(call.is_error);
        assert_eq!(input_of(&call)["tool"], serde_json::json!("edit"));
    }

    #[test]
    fn write_and_read_inputs_become_canonical() {
        let write = normalized(
            "write",
            serde_json::json!({
                "status": "completed",
                "input": { "filePath": "src/new.ts", "content": "export const A = 1\n" },
                "output": "Wrote file successfully.",
                "metadata": { "filepath": "/p/src/new.ts", "exists": false },
            }),
        );
        assert_eq!(
            input_of(&write),
            serde_json::json!({ "file_path": "src/new.ts", "content": "export const A = 1\n" })
        );

        let read = normalized(
            "read",
            serde_json::json!({
                "status": "completed",
                "input": { "filePath": "src/app.ts" },
                "output": "<path>/p/src/app.ts</path>\n<type>file</type>\n<content>\n1: export const A = 1\n</content>",
                "metadata": {
                    "display": {
                        "type": "file",
                        "path": "/p/src/app.ts",
                        "text": "export const A = 1",
                        "lineStart": 1,
                        "lineEnd": 1
                    }
                },
            }),
        );
        assert_eq!(
            input_of(&read),
            serde_json::json!({ "file_path": "src/app.ts" })
        );
        // The `<path>`/`<content>` envelope and the `N: ` prefixes are gone.
        assert_eq!(
            read.output_preview.as_deref(),
            Some(r#"{"start_line":1,"content":"export const A = 1"}"#)
        );
    }

    #[test]
    fn read_of_a_directory_lists_its_entries() {
        let call = normalized(
            "read",
            serde_json::json!({
                "status": "completed",
                "input": { "filePath": "src" },
                "output": "<path>/p/src</path>\n<type>directory</type>\n<entries>\napp.ts\n</entries>",
                "metadata": {
                    "display": { "type": "directory", "path": "/p/src", "entries": ["app.ts", "new.ts"] }
                },
            }),
        );

        assert_eq!(call.output_preview.as_deref(), Some("app.ts\nnew.ts"));
    }

    #[test]
    fn grep_include_is_renamed_to_glob() {
        let call = normalized(
            "grep",
            serde_json::json!({
                "status": "completed",
                "input": { "pattern": "VERSION", "path": ".", "include": "*.ts" },
            }),
        );

        assert_eq!(
            input_of(&call),
            serde_json::json!({ "pattern": "VERSION", "path": ".", "glob": "*.ts" })
        );
    }

    #[test]
    fn bash_falls_back_to_metadata_output() {
        let call = normalized(
            "bash",
            serde_json::json!({
                "status": "completed",
                "input": { "command": "echo probe", "description": "probe" },
                "metadata": { "output": "probe\n", "exit": 0 },
            }),
        );

        // Trimmed like every other preview (`value_to_preview`).
        assert_eq!(call.output_preview.as_deref(), Some("probe"));
        assert_eq!(input_of(&call)["command"], serde_json::json!("echo probe"));
    }

    #[test]
    fn skill_call_titles_itself_and_drops_the_envelope() {
        let call = normalized(
            "skill",
            serde_json::json!({
                "status": "completed",
                "input": { "name": "demo-skill" },
                "output": "<skill_content name=\"demo-skill\">\n# Skill: demo-skill\n\n# Demo Skill\n\n1. Read the target file.\n\nBase directory for this skill: /c/skills/demo-skill\nRelative paths in this skill (e.g., scripts/) are relative to this base directory.\nNote: file list is sampled.\n\n<skill_files>\n<file>/c/skills/demo-skill/run.sh</file>\n</skill_files>\n</skill_content>",
                "title": "Loaded skill: demo-skill",
                "metadata": { "name": "demo-skill", "dir": "/c/skills/demo-skill" },
            }),
        );

        assert_eq!(input_of(&call)["skill"], serde_json::json!("demo-skill"));
        assert_eq!(
            call.output_preview.as_deref(),
            Some("# Skill: demo-skill\n\n# Demo Skill\n\n1. Read the target file.")
        );
    }

    #[test]
    fn skill_output_without_the_envelope_is_left_alone() {
        assert_eq!(super::unwrap_skill_content("plain body"), "plain body");
    }

    #[test]
    fn apply_patch_input_is_the_patch_text_itself() {
        let call = normalized(
            "patch",
            serde_json::json!({
                "status": "completed",
                "input": { "patchText": "*** Begin Patch\n*** End Patch" },
            }),
        );

        assert_eq!(call.tool_name, "apply_patch");
        assert_eq!(
            call.input_preview.as_deref(),
            Some("*** Begin Patch\n*** End Patch")
        );
    }

    #[test]
    fn canonical_tool_inputs_pass_through_untouched() {
        let call = normalized(
            "todowrite",
            serde_json::json!({
                "status": "completed",
                "input": { "todos": [{ "content": "Probe", "status": "completed" }] },
            }),
        );

        assert_eq!(call.tool_name, "todowrite");
        assert_eq!(
            input_of(&call),
            serde_json::json!({ "todos": [{ "content": "Probe", "status": "completed" }] })
        );
    }

    #[test]
    fn ignores_non_image_file_part_for_image_parsing() {
        let value = serde_json::json!({
            "type": "file",
            "mime": "text/plain",
            "filename": "notes.txt",
            "url": "file:///tmp/notes.txt"
        });

        assert!(extract_opencode_file_image(&value).is_none());
    }

    /// Verbatim `state` of a `question` tool part captured from a real
    /// `~/.local/share/opencode/opencode.db`, trimmed to two questions.
    fn question_state() -> serde_json::Value {
        serde_json::json!({
            "status": "completed",
            "input": {
                "questions": [
                    {
                        "question": "Do you have a Claude Pro/Max subscription?",
                        "header": "Claude Subscription",
                        "options": [
                            { "label": "Yes, I'm on max20", "description": "20x mode" },
                            { "label": "No Claude subscription", "description": "None" }
                        ]
                    },
                    {
                        "question": "Pick the integrations",
                        "header": "Integrations",
                        "multiple": true,
                        "options": [
                            { "label": "Gemini", "description": "" },
                            { "label": "Copilot", "description": "" }
                        ]
                    }
                ]
            },
            "output": "User has answered your questions: \"Do you have a Claude Pro/Max subscription?\"=\"No Claude subscription\", \"Pick the integrations\"=\"Gemini, Copilot\". You can now continue with the user's answers in mind.",
            "title": "Asked 2 questions",
            "metadata": {
                "answers": [["No Claude subscription"], ["Gemini", "Copilot"]],
                "truncated": false
            }
        })
    }

    #[test]
    fn question_output_becomes_the_structured_answer_envelope() {
        // `metadata.answers` is positional against `input.questions`; the
        // one-line `output` sentence the tool also writes is what
        // `parseAskQuestionOutcome` could not read, so every answered question
        // rendered as "no selection".
        let call = normalized("question", question_state());
        assert_eq!(call.tool_name, "question");
        let outcome: serde_json::Value =
            serde_json::from_str(call.output_preview.as_deref().expect("output")).expect("json");
        assert_eq!(
            outcome,
            serde_json::json!({
                "declined": false,
                "answers": [
                    {
                        "header": "Claude Subscription",
                        "question": "Do you have a Claude Pro/Max subscription?",
                        "selected": ["No Claude subscription"],
                    },
                    {
                        "header": "Integrations",
                        "question": "Pick the integrations",
                        "selected": ["Gemini", "Copilot"],
                    },
                ],
            })
        );
    }

    #[test]
    fn question_input_gains_the_multi_select_key_the_card_reads() {
        // OpenCode spells the flag `multiple`; `parseAskQuestionInput` only
        // accepts `multiSelect` / `multi_select`, so a multi-select question
        // rendered as single-select.
        let call = normalized("question", question_state());
        let input = input_of(&call);
        assert_eq!(input["questions"][1]["multiSelect"], serde_json::json!(true));
        // Untouched where the source said nothing, and the rest is verbatim.
        assert!(input["questions"][0].get("multiSelect").is_none());
        assert_eq!(
            input["questions"][0]["options"][0]["label"],
            serde_json::json!("Yes, I'm on max20")
        );
    }

    #[test]
    fn a_dismissed_question_reports_declined() {
        let call = normalized(
            "question",
            serde_json::json!({
                "status": "error",
                "input": { "questions": [{ "question": "Continue?", "header": "Gate" }] },
                "error": "The user dismissed this question",
            }),
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(call.output_preview.as_deref().unwrap())
                .unwrap(),
            serde_json::json!({ "declined": true, "answers": [] })
        );
    }

    #[test]
    fn a_dismissal_from_another_tool_is_not_claimed_as_a_question() {
        // No `questions` in the input → not a question call, so the error text
        // stays the plain failure output the generic card renders.
        assert!(super::structure_question_output(
            Some(&serde_json::json!({ "command": "rm -rf /" })),
            None,
            Some("The user dismissed this question"),
        )
        .is_none());
    }

    #[test]
    fn read_output_is_rebuilt_from_the_display_metadata() {
        // Same `metadata` OpenCode ships under `rawOutput` on the live wire, so
        // this is the shared half of the live/history parity fix.
        let structured = super::structure_read_output(Some(&serde_json::json!({
            "display": {
                "type": "file",
                "path": "/w/notes.txt",
                "text": "hello world\nsecond line",
                "lineStart": 40,
                "lineEnd": 41
            }
        })))
        .expect("structured read output");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&structured).unwrap(),
            serde_json::json!({ "start_line": 40, "content": "hello world\nsecond line" })
        );

        // Only the `read` tool writes `metadata.display`, which is what makes
        // the live path safe to key on the shape alone.
        assert!(super::structure_read_output(Some(&serde_json::json!({ "exit": 0 }))).is_none());
        assert!(super::structure_read_output(None).is_none());
    }

    #[test]
    fn assistant_errors_and_aborts_get_a_visible_marker() {
        // 83 of 2 849 messages in a real 430-session library carry one, and the
        // parts of such a message are empty — so without a marker the bubble is
        // blank.
        assert_eq!(
            super::assistant_error_text(&serde_json::json!({
                "role": "assistant",
                "error": { "name": "APIError", "data": { "message": "Invalid Authentication" } }
            }))
            .as_deref(),
            Some("[opencode APIError] Invalid Authentication")
        );
        // An abort's own message is boilerplate ("The operation was aborted.")
        // that says less than the name does.
        assert_eq!(
            super::assistant_error_text(&serde_json::json!({
                "error": {
                    "name": "MessageAbortedError",
                    "data": { "message": "The operation was aborted." }
                }
            }))
            .as_deref(),
            Some("[opencode aborted]")
        );
        assert_eq!(
            super::assistant_error_text(&serde_json::json!({
                "error": { "name": "UnknownError", "data": {} }
            }))
            .as_deref(),
            Some("[opencode UnknownError]")
        );
        assert!(super::assistant_error_text(&serde_json::json!({ "role": "assistant" })).is_none());

        // 2.x's flat `{type, message}` — read before, it came out as a bare
        // "[opencode UnknownError]" with the provider's message dropped.
        assert_eq!(
            super::assistant_error_text(&serde_json::json!({
                "error": {
                    "type": "provider.invalid-request",
                    "message": "fake provider rejected the request",
                    "status": 400
                }
            }))
            .as_deref(),
            Some("[opencode provider.invalid-request] fake provider rejected the request")
        );
        assert_eq!(
            super::assistant_error_text(&serde_json::json!({
                "error": { "type": "aborted", "message": "The operation was aborted." }
            }))
            .as_deref(),
            Some("[opencode aborted]")
        );
    }

    #[test]
    fn only_a_bare_compaction_message_gets_refiled_as_assistant() {
        let compaction = || ContentBlock::ToolUse {
            tool_use_id: Some("prt_1".into()),
            tool_name: "context_compaction".into(),
            input_preview: None,
            status: None,
            meta: None,
        };
        let result = || ContentBlock::ToolResult {
            tool_use_id: Some("prt_1".into()),
            output_preview: None,
            is_error: false,
            agent_stats: None,
            images: Vec::new(),
        };

        assert!(super::is_compaction_only(&[compaction(), result()]));
        // Anything the user actually said keeps the message theirs.
        assert!(!super::is_compaction_only(&[
            ContentBlock::Text { text: "carry on".into() },
            compaction(),
        ]));
        // A different tool's pair is not a compaction, and neither is nothing.
        assert!(!super::is_compaction_only(&[
            ContentBlock::ToolUse {
                tool_use_id: Some("prt_2".into()),
                tool_name: "read".into(),
                input_preview: None,
                status: None,
                meta: None,
            },
            result(),
        ]));
        assert!(!super::is_compaction_only(&[]));
        assert!(!super::is_compaction_only(&[result()]));
    }

    #[test]
    fn subtask_parts_describe_the_delegation_they_replaced() {
        // A slash command routed to a sub-agent writes a `subtask` part INSTEAD
        // of the expanded prompt text, so the user's turn was blank.
        assert_eq!(
            super::subtask_summary(&serde_json::json!({
                "type": "subtask",
                "agent": "explore",
                "command": "review",
                "description": "review changes",
                "prompt": "Review the uncommitted diff."
            }))
            .as_deref(),
            Some("/review → @explore: review changes\n\nReview the uncommitted diff.")
        );
        // `command` and `description` are both optional in the schema.
        assert_eq!(
            super::subtask_summary(&serde_json::json!({
                "agent": "explore",
                "prompt": "Look around."
            }))
            .as_deref(),
            Some("@explore\n\nLook around.")
        );
        assert!(super::subtask_summary(&serde_json::json!({ "type": "subtask" })).is_none());
    }

    /// The placeholder test has to match the whole shape, not just the prefix:
    /// "New session - " is a legal thing for a person to call a session, and
    /// renaming one to that must not send it back to the fallback.
    #[test]
    fn only_opencodes_own_generated_name_counts_as_untitled() {
        assert!(super::is_default_title("New session - 2026-09-16T03:09:14.543Z"));
        assert!(super::is_default_title(
            "Child session - 2026-09-16T03:09:14.543Z"
        ));

        for kept in [
            "New session - notes",
            "New session - 2026-09-16",
            "New session - 2026-09-16T03:09:14.543Z ",
            "New session - 2026-09-16T03:09:14.543Z extra",
            "New session - 20X6-09-16T03:09:14.543Z",
            "Fix the login flow",
            "",
        ] {
            assert!(!super::is_default_title(kept), "{kept:?} is a real title");
        }
    }

    #[test]
    fn the_fork_marker_is_peeled_off_and_nothing_else_is() {
        assert_eq!(
            super::split_fork_suffix("New session - 2026-09-16T03:09:14.543Z (fork #12)"),
            ("New session - 2026-09-16T03:09:14.543Z", " (fork #12)")
        );
        // Forking a fork nests the count rather than the marker, so only ever
        // one suffix to peel.
        assert_eq!(
            super::split_fork_suffix("Fix login (fork #2)"),
            ("Fix login", " (fork #2)")
        );
        for untouched in [
            "Fix login",
            "Fix login (fork #)",
            "Fix login (fork #2) ",
            "Fix login (fork #two)",
            "(fork #2)",
        ] {
            assert_eq!(
                super::split_fork_suffix(untouched),
                (untouched, ""),
                "{untouched:?} should be left whole"
            );
        }
    }

    /// OpenCode names a session before anyone has spoken and is supposed to
    /// replace that name on the first turn — but the call is forked and its
    /// errors swallowed, so an unreachable small model leaves every session
    /// called "New session - <timestamp>" forever. Stand in for it the same way
    /// OpenCode's own TUI does: with the opening user message.
    #[test]
    fn a_placeholder_title_gives_way_to_what_the_user_actually_said() {
        let derived = super::resolve_title(
            Some("New session - 2026-09-16T03:09:14.543Z".into()),
            Some("执行一下 pnpm build".into()),
        );
        assert_eq!(derived.as_deref(), Some("执行一下 pnpm build"));

        // A real title always wins, even with a first message to fall back to.
        assert_eq!(
            super::resolve_title(Some("Fix the login flow".into()), Some("hi".into())).as_deref(),
            Some("Fix the login flow")
        );

        // A fork of an unnamed session inherits the placeholder plus a marker —
        // a string OpenCode's own `isDefaultTitle` no longer recognises. Keep
        // the marker (it is the only thing telling the two rows apart) and
        // replace the placeholder under it.
        assert_eq!(
            super::resolve_title(
                Some("New session - 2026-09-04T07:11:47.236Z (fork #1)".into()),
                Some("hi".into()),
            )
            .as_deref(),
            Some("hi (fork #1)")
        );

        // Nothing to derive from: report untitled and let the UI use its own
        // localized label rather than showing a machine placeholder.
        assert_eq!(
            super::resolve_title(Some("New session - 2026-09-16T03:09:14.543Z".into()), None),
            None
        );
        assert_eq!(
            super::resolve_title(
                Some("New session - 2026-09-16T03:09:14.543Z".into()),
                Some("   ".into()),
            ),
            None
        );
        assert_eq!(super::resolve_title(None, Some("hi".into())).as_deref(), Some("hi"));
    }

    /// The fallback runs the same folding and capping every other agent's
    /// derived title does, so a session opened with a file mention is named
    /// after the label rather than a truncated `file://` URL.
    #[test]
    fn a_derived_title_is_folded_and_capped_like_every_other_agents() {
        let long = "x".repeat(150);
        assert_eq!(
            super::resolve_title(
                Some("New session - 2026-09-16T03:09:14.543Z".into()),
                Some(long.clone()),
            ),
            Some(super::super::title_from_user_text(&long))
        );
        assert_eq!(
            super::resolve_title(
                Some("New session - 2026-09-16T03:09:14.543Z".into()),
                Some("look at [notes.md](file:///tmp/a/very/long/path/notes.md)".into()),
            )
            .as_deref(),
            Some("look at notes.md")
        );
    }

    /// OpenCode 2 returns a tool's result as `state.content` items and its
    /// failure as `{type, message}`; the shapes below are what 2.0.16 wrote.
    #[test]
    fn a_2x_tool_result_is_read_from_its_content_items() {
        let shell = normalized(
            "shell",
            serde_json::json!({
                "status": "completed",
                "input": { "command": "false" },
                "content": [
                    { "type": "text", "text": "boom\n" },
                    { "type": "text", "text": "Exit code 1" }
                ],
                "metadata": { "status": "completed", "truncated": false, "exit": 1 }
            }),
        );
        assert!(!shell.is_error);
        assert_eq!(shell.output_preview.as_deref(), Some("boom\n\nExit code 1"));

        // File items are not text; only the text beside them is the result.
        let image = normalized(
            "read",
            serde_json::json!({
                "status": "completed",
                "input": { "path": "/p/shot.png" },
                "content": [
                    { "type": "text", "text": "Image read successfully" },
                    { "type": "file", "uri": "data:image/png;base64,iVBORw0KGgo=", "mime": "image/png" }
                ]
            }),
        );
        assert_eq!(
            image.output_preview.as_deref(),
            Some("Image read successfully")
        );
        assert_eq!(input_of(&image)["file_path"], "/p/shot.png");

        let failed = normalized(
            "read",
            serde_json::json!({
                "status": "error",
                "input": { "path": "/p/missing.ts" },
                "error": { "type": "tool.execution", "message": "File not found: /p/missing.ts" }
            }),
        );
        assert!(failed.is_error);
        assert_eq!(
            failed.output_preview.as_deref(),
            Some("File not found: /p/missing.ts")
        );
    }

    /// 2.x reports an edit's diff per file (`metadata.files[].patch`), which
    /// carries the same first-hunk line the 1.x `metadata.diff` did.
    #[test]
    fn a_2x_edit_takes_its_start_line_from_the_per_file_patch() {
        let call = normalized(
            "edit",
            serde_json::json!({
                "status": "completed",
                "input": { "path": "/p/notes.txt", "oldString": "two", "newString": "three" },
                "content": [{ "type": "text", "text": "Edited /p/notes.txt (1 replacement)" }],
                "metadata": {
                    "files": [{
                        "file": "/p/notes.txt",
                        "patch": "--- /p/notes.txt\n+++ /p/notes.txt\n@@ -7,2 +7,2 @@\n one\n-two\n+three\n",
                        "additions": 1,
                        "deletions": 1
                    }]
                }
            }),
        );
        let input = input_of(&call);
        assert_eq!(input["file_path"], "/p/notes.txt");
        assert_eq!(input["old_string"], "two");
        assert_eq!(input["_start_line"], serde_json::json!(7));
    }

    #[test]
    fn sub_agent_launches_are_recognised_under_both_names() {
        // History the 1.x migration carried over verbatim.
        let migrated = serde_json::json!({
            "type": "tool",
            "id": "call_1",
            "name": "task",
            "state": {
                "status": "completed",
                "input": { "subagent_type": "general", "prompt": "p", "description": "d" },
                "metadata": { "sessionId": "ses_child" }
            }
        });
        let call = super::v2_agent_call(&migrated).expect("a migrated task call");
        assert_eq!(call.agent_type, "general");
        assert_eq!(call.session_id, Some("ses_child"));

        // 2.x's own tool.
        let native = serde_json::json!({
            "type": "tool",
            "id": "call_2",
            "name": "subagent",
            "state": {
                "status": "completed",
                "input": { "agent": "explore", "prompt": "p", "description": "d" },
                "metadata": { "sessionID": "ses_other", "status": "completed" }
            }
        });
        let call = super::v2_agent_call(&native).expect("a subagent call");
        assert_eq!(call.agent_type, "explore");
        assert_eq!(call.session_id, Some("ses_other"));

        // Still streaming its input (a JSON fragment), and an ordinary tool.
        let streaming = serde_json::json!({
            "type": "tool",
            "name": "subagent",
            "state": { "status": "streaming", "input": "{\"agent\":\"gen" }
        });
        assert!(super::v2_agent_call(&streaming).is_none());
        let shell = serde_json::json!({
            "type": "tool",
            "name": "shell",
            "state": { "status": "completed", "input": { "command": "ls" } }
        });
        assert!(super::v2_agent_call(&shell).is_none());
    }

    #[test]
    fn a_sub_agent_answer_is_unwrapped_from_either_envelope() {
        assert_eq!(
            super::extract_subagent_result(
                "<subagent sessionID=\"ses_1\" state=\"completed\">\nchild finished\n</subagent>"
            ),
            "child finished"
        );
        assert_eq!(
            super::extract_subagent_result(
                "task_id: ses_1 (for resuming to continue this task if needed)\n\n<task_result>\nfound it\n</task_result>"
            ),
            "found it"
        );
        // A background launch reports where the work went, not an answer.
        let background = "The subagent is working in the background (sessionID: ses_1).";
        assert_eq!(super::extract_subagent_result(background), background);
    }

    /// A scratch `opencode.db`, built from the statements queued on it.
    struct DbFixture {
        statements: Vec<Statement>,
    }

    impl DbFixture {
        fn new(schema: &[&str]) -> Self {
            let mut fixture = Self {
                statements: Vec::new(),
            };
            for ddl in schema {
                fixture.exec(ddl, []);
            }
            fixture
        }

        fn exec(&mut self, sql: &str, values: impl IntoIterator<Item = sea_orm::Value>) {
            self.statements.push(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                sql,
                values,
            ));
        }

        fn legacy_session(&mut self, id: &str, title: &str, created: i64, updated: i64) {
            self.exec(
                "INSERT INTO session (id, directory, title, time_created, time_updated) \
                 VALUES (?, '/work/app', ?, ?, ?)",
                [id.into(), title.into(), created.into(), updated.into()],
            );
        }

        /// A legacy message with one text part.
        fn legacy_text(&mut self, session: &str, id: &str, role: &str, created: i64, text: &str) {
            let data = serde_json::json!({ "role": role, "time": { "created": created } });
            self.exec(
                "INSERT INTO message (id, session_id, time_created, data) VALUES (?, ?, ?, ?)",
                [
                    id.into(),
                    session.into(),
                    created.into(),
                    data.to_string().into(),
                ],
            );
            let part = serde_json::json!({ "type": "text", "text": text });
            self.exec(
                "INSERT INTO part (id, message_id, time_created, data) VALUES (?, ?, ?, ?)",
                [
                    format!("prt_{id}").into(),
                    id.into(),
                    created.into(),
                    part.to_string().into(),
                ],
            );
        }

        fn v2_session(
            &mut self,
            id: &str,
            parent: Option<&str>,
            title: Option<&str>,
            created: i64,
            updated: i64,
        ) {
            self.exec(
                "INSERT INTO session_v2 (id, parent_id, directory, title, time_created, time_updated) \
                 VALUES (?, ?, '/work/app', ?, ?, ?)",
                [
                    id.into(),
                    parent.map(str::to_string).into(),
                    title.map(str::to_string).into(),
                    created.into(),
                    updated.into(),
                ],
            );
        }

        fn v2_message(
            &mut self,
            session: &str,
            id: &str,
            seq: i64,
            kind: &str,
            created: i64,
            data: serde_json::Value,
        ) {
            self.exec(
                "INSERT INTO session_message \
                 (id, session_id, type, seq, time_created, time_updated, data) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                [
                    id.into(),
                    session.into(),
                    kind.into(),
                    seq.into(),
                    created.into(),
                    created.into(),
                    data.to_string().into(),
                ],
            );
        }

        fn build(self) -> (tempfile::TempDir, super::OpenCodeParser) {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("opencode.db");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async {
                let conn =
                    sea_orm::Database::connect(format!("sqlite:{}?mode=rwc", db_path.display()))
                        .await
                        .expect("open sqlite");
                for statement in self.statements {
                    conn.execute(statement).await.expect("fixture statement");
                }
                conn.close().await.expect("close sqlite");
            });
            let parser = super::OpenCodeParser::with_base_dir(dir.path().to_path_buf());
            (dir, parser)
        }
    }

    const LEGACY_TABLES: [&str; 3] = [
        "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, \
         title TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL)",
        "CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, \
         time_created INTEGER NOT NULL, data TEXT NOT NULL)",
        "CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, \
         time_created INTEGER NOT NULL, data TEXT NOT NULL)",
    ];

    const V2_TABLES: [&str; 2] = [
        "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL, \
         title TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL)",
        "CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, \
         type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL, \
         time_updated INTEGER NOT NULL, data TEXT NOT NULL)",
    ];

    fn text_blocks(blocks: &[ContentBlock]) -> Vec<&str> {
        blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// A fresh 2.x install has no legacy tables at all — the database every
    /// read used to fail on with `no such table: session`. The rows mirror
    /// what OpenCode 2.0.16 wrote for a real run: an unnamed session (its
    /// title request failed), a `shell` call, a `subagent` launch whose child
    /// ran a `shell` of its own, a rejected request, and the bookkeeping rows
    /// around them.
    #[test]
    fn a_2x_only_database_lists_and_reads_its_sessions() {
        use crate::models::TurnRole;
        use crate::parsers::AgentParser;

        let t0: i64 = 1_790_612_421_000;
        let model = serde_json::json!({ "id": "test-model", "providerID": "test" });
        let tokens = serde_json::json!({
            "input": 100, "output": 20, "reasoning": 0, "cache": { "read": 0, "write": 0 }
        });

        let mut fx = DbFixture::new(&V2_TABLES);
        fx.v2_session("ses_main", None, None, t0, t0 + 9_000);
        fx.v2_session(
            "ses_child",
            Some("ses_main"),
            Some("Probe child"),
            t0 + 110,
            t0 + 190,
        );

        fx.v2_message(
            "ses_main",
            "msg_01",
            4,
            "user",
            t0 + 10,
            serde_json::json!({
                "time": { "created": t0 + 10 },
                "text": "exercise the tools",
                "files": [{
                    "data": "iVBORw0KGgo=",
                    "mime": "image/png",
                    "source": { "type": "inline" },
                    "name": "shot.png"
                }]
            }),
        );
        fx.v2_message(
            "ses_main",
            "msg_02",
            5,
            "synthetic",
            t0 + 11,
            serde_json::json!({
                "time": { "created": t0 + 11 },
                "text": "Text written for the model, not by the user."
            }),
        );
        fx.v2_message(
            "ses_main",
            "msg_03",
            6,
            "assistant",
            t0 + 20,
            serde_json::json!({
                "time": { "created": t0 + 20, "completed": t0 + 90 },
                "agent": "build",
                "model": model,
                "tokens": tokens,
                "content": [
                    { "type": "reasoning", "text": "Run it first." },
                    { "type": "text", "text": "Running it." },
                    {
                        "type": "tool",
                        "id": "call_main_1",
                        "name": "shell",
                        "state": {
                            "status": "completed",
                            "input": { "command": "echo hi" },
                            "content": [
                                { "type": "text", "text": "hi\n" },
                                { "type": "text", "text": "Exit code 1" }
                            ],
                            "metadata": { "status": "completed", "truncated": false, "exit": 1 }
                        },
                        "time": { "created": t0 + 30, "ran": t0 + 31, "completed": t0 + 80 }
                    }
                ]
            }),
        );
        fx.v2_message("ses_main", "msg_04", 7, "assistant", t0 + 100, serde_json::json!({
            "time": { "created": t0 + 100, "completed": t0 + 210 },
            "agent": "build",
            "model": model,
            "tokens": tokens,
            "content": [{
                "type": "tool",
                "id": "call_main_2",
                "name": "subagent",
                "state": {
                    "status": "completed",
                    "input": { "agent": "general", "description": "Probe child", "prompt": "run echo" },
                    "content": [{
                        "type": "text",
                        "text": "<subagent sessionID=\"ses_child\" state=\"completed\">\nchild finished\n</subagent>"
                    }],
                    "metadata": { "sessionID": "ses_child", "status": "completed", "truncated": false }
                },
                "time": { "created": t0 + 101, "ran": t0 + 110, "completed": t0 + 200 }
            }]
        }));
        fx.v2_message(
            "ses_main",
            "msg_05",
            8,
            "system",
            t0 + 205,
            serde_json::json!({
                "time": { "created": t0 + 205 },
                "text": "The available tools have changed."
            }),
        );
        fx.v2_message(
            "ses_main",
            "msg_06",
            9,
            "idle",
            t0 + 211,
            serde_json::json!({
                "time": { "created": t0 + 211 },
                "outcome": "succeeded"
            }),
        );
        // Same millisecond, and ids that sort the other way round: only `seq`
        // puts the prompt before the reply it got.
        fx.v2_message(
            "ses_main",
            "msg_zz",
            10,
            "user",
            t0 + 8_000,
            serde_json::json!({
                "time": { "created": t0 + 8_000 },
                "text": "please fail now"
            }),
        );
        fx.v2_message(
            "ses_main",
            "msg_aa",
            11,
            "assistant",
            t0 + 8_000,
            serde_json::json!({
                "time": { "created": t0 + 8_000, "completed": t0 + 8_005 },
                "agent": "build",
                "model": model,
                "finish": "error",
                "content": [],
                "error": {
                    "type": "provider.invalid-request",
                    "message": "fake provider rejected the request",
                    "status": 400
                }
            }),
        );
        fx.v2_message(
            "ses_main",
            "msg_ab",
            12,
            "assistant",
            t0 + 8_010,
            serde_json::json!({
                "time": { "created": t0 + 8_010, "completed": t0 + 8_020 },
                "agent": "build",
                "model": model,
                "content": [],
                "error": { "type": "aborted", "message": "The operation was aborted." }
            }),
        );

        fx.v2_message(
            "ses_child",
            "msg_c1",
            0,
            "user",
            t0 + 111,
            serde_json::json!({
                "time": { "created": t0 + 111 },
                "text": "You are a subagent spawned by another session.\nrun echo"
            }),
        );
        fx.v2_message(
            "ses_child",
            "msg_c2",
            1,
            "assistant",
            t0 + 120,
            serde_json::json!({
                "time": { "created": t0 + 120, "completed": t0 + 160 },
                "agent": "general",
                "model": model,
                "content": [{
                    "type": "tool",
                    "id": "call_child_1",
                    "name": "shell",
                    "state": {
                        "status": "completed",
                        "input": { "command": "echo child-output" },
                        "content": [{ "type": "text", "text": "child-output\n" }]
                    },
                    "time": { "created": t0 + 121, "ran": t0 + 122, "completed": t0 + 150 }
                }]
            }),
        );
        fx.v2_message(
            "ses_child",
            "msg_c3",
            2,
            "assistant",
            t0 + 170,
            serde_json::json!({
                "time": { "created": t0 + 170, "completed": t0 + 180 },
                "agent": "general",
                "model": model,
                "content": [{ "type": "text", "text": "child finished" }]
            }),
        );
        let (_dir, parser) = fx.build();

        let listed = parser.list_conversations().expect("list");
        let ids: Vec<&str> = listed.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["ses_child", "ses_main"], "newest first");
        let main = &listed[1];
        // 2.x leaves a session it could not name untitled; the opening prompt
        // stands in, as it does for 1.x's placeholder title.
        assert_eq!(main.title.as_deref(), Some("exercise the tools"));
        // The user's and the assistant's messages only — not the synthetic,
        // system and idle rows between them.
        assert_eq!(main.message_count, 6);
        assert_eq!(main.model.as_deref(), Some("test-model"));
        assert_eq!(listed[0].parent_id.as_deref(), Some("ses_main"));
        assert_eq!(listed[0].title.as_deref(), Some("Probe child"));

        let detail = parser.get_conversation("ses_main").expect("detail");
        assert_eq!(detail.summary.title.as_deref(), Some("exercise the tools"));
        let roles: Vec<&str> = detail
            .turns
            .iter()
            .map(|t| match t.role {
                TurnRole::User => "user",
                TurnRole::Assistant => "assistant",
                TurnRole::System => "system",
            })
            .collect();
        assert_eq!(
            roles,
            [
                "user",
                "assistant",
                "assistant",
                "user",
                "assistant",
                "assistant"
            ],
            "no turn for the synthetic, system or idle rows"
        );

        match detail.turns[0].blocks.as_slice() {
            [ContentBlock::Text { text }, ContentBlock::Image { mime_type, uri, .. }] => {
                assert_eq!(text, "exercise the tools");
                assert_eq!(mime_type, "image/png");
                assert_eq!(uri.as_deref(), Some("shot.png"));
            }
            other => panic!("unexpected user blocks: {other:?}"),
        }

        let first = &detail.turns[1];
        assert_eq!(first.model.as_deref(), Some("test-model"));
        assert_eq!(first.usage.as_ref().map(|u| u.input_tokens), Some(100));
        assert_eq!(first.duration_ms, Some(70));
        match first.blocks.as_slice() {
            [ContentBlock::Thinking { text: thinking }, ContentBlock::Text { text }, ContentBlock::ToolUse {
                tool_name,
                input_preview,
                ..
            }, ContentBlock::ToolResult {
                output_preview,
                is_error,
                ..
            }] => {
                assert_eq!(thinking, "Run it first.");
                assert_eq!(text, "Running it.");
                assert_eq!(tool_name, "shell");
                assert_eq!(input_preview.as_deref(), Some(r#"{"command":"echo hi"}"#));
                assert_eq!(output_preview.as_deref(), Some("hi\n\nExit code 1"));
                assert!(!is_error);
            }
            other => panic!("unexpected assistant blocks: {other:?}"),
        }

        match detail.turns[2].blocks.as_slice() {
            [ContentBlock::ToolUse {
                tool_name,
                input_preview,
                ..
            }, ContentBlock::ToolResult {
                output_preview,
                agent_stats: Some(stats),
                is_error,
                ..
            }] => {
                assert_eq!(tool_name, "Agent");
                let input: serde_json::Value =
                    serde_json::from_str(input_preview.as_deref().unwrap()).unwrap();
                assert_eq!(input["subagent_type"], "general");
                assert_eq!(input["description"], "Probe child");
                assert_eq!(input["prompt"], "run echo");
                assert_eq!(output_preview.as_deref(), Some("child finished"));
                assert!(!is_error);
                assert_eq!(stats.agent_type.as_deref(), Some("general"));
                assert_eq!(stats.total_duration_ms, Some(90));
                assert_eq!(stats.tool_calls.len(), 1, "the child's own tool call");
                assert_eq!(stats.tool_calls[0].tool_name, "shell");
                assert_eq!(
                    stats.tool_calls[0].output_preview.as_deref(),
                    Some("child-output")
                );
            }
            other => panic!("unexpected agent blocks: {other:?}"),
        }

        assert_eq!(text_blocks(&detail.turns[3].blocks), ["please fail now"]);
        assert_eq!(
            text_blocks(&detail.turns[4].blocks),
            ["[opencode provider.invalid-request] fake provider rejected the request"]
        );
        assert_eq!(text_blocks(&detail.turns[5].blocks), ["[opencode aborted]"]);
    }

    /// History the 1.x migration carried into `session_message` keeps its 1.x
    /// tool calls verbatim — `task`, `bash`, camelCase inputs — wrapped in the
    /// 2.x tool shape. Shapes as a real migrated database holds them.
    #[test]
    fn a_migrated_task_call_keeps_its_sub_agent_rows() {
        use crate::parsers::AgentParser;

        let mut fx = DbFixture::new(&V2_TABLES);
        fx.v2_session("ses_parent", None, Some("Delegate a search"), 1_000, 40_000);
        fx.v2_session(
            "ses_kid",
            Some("ses_parent"),
            Some("Search docs"),
            1_100,
            30_000,
        );
        fx.v2_message(
            "ses_parent",
            "msg_p1",
            0,
            "user",
            1_000,
            serde_json::json!({
                "time": { "created": 1_000 },
                "text": "delegate it"
            }),
        );
        fx.v2_message("ses_parent", "msg_p2", 1, "assistant", 1_050, serde_json::json!({
            "time": { "created": 1_050, "completed": 31_000 },
            "agent": "build",
            "model": { "id": "big-pickle", "providerID": "opencode", "variant": "default" },
            "content": [{
                "type": "tool",
                "id": "call_task",
                "name": "task",
                "state": {
                    "status": "completed",
                    "input": {
                        "description": "Search docs",
                        "prompt": "find it",
                        "subagent_type": "general"
                    },
                    "content": [{
                        "type": "text",
                        "text": "task_id: ses_kid (for resuming to continue this task if needed)\n\n<task_result>\nfound it\n</task_result>"
                    }],
                    "metadata": {
                        "sessionId": "ses_kid",
                        "model": { "modelID": "big-pickle", "providerID": "opencode" },
                        "truncated": false
                    }
                },
                "time": { "created": 1_000, "completed": 29_805 }
            }]
        }));
        fx.v2_message(
            "ses_kid",
            "msg_k1",
            0,
            "assistant",
            1_200,
            serde_json::json!({
                "time": { "created": 1_200, "completed": 2_000 },
                "agent": "general",
                "model": { "id": "big-pickle", "providerID": "opencode" },
                "content": [
                    {
                        "type": "tool",
                        "id": "call_k1",
                        "name": "bash",
                        "state": {
                            "status": "completed",
                            "input": { "command": "ls", "description": "List files" },
                            "content": [{ "type": "text", "text": "a\nb" }],
                            "metadata": { "output": "a\nb", "exit": 0 }
                        },
                        "time": { "created": 1_300, "completed": 1_400 }
                    },
                    {
                        "type": "tool",
                        "id": "call_k2",
                        "name": "read",
                        "state": {
                            "status": "error",
                            "input": { "filePath": "/work/app/missing.md" },
                            "error": { "type": "tool.execution", "message": "File not found" }
                        },
                        "time": { "created": 1_500, "completed": 1_600 }
                    }
                ]
            }),
        );
        let (_dir, parser) = fx.build();

        let detail = parser.get_conversation("ses_parent").expect("detail");
        let blocks = &detail.turns[1].blocks;
        let (input, output, stats) = match blocks.as_slice() {
            [ContentBlock::ToolUse {
                tool_name,
                input_preview,
                ..
            }, ContentBlock::ToolResult {
                output_preview,
                agent_stats: Some(stats),
                ..
            }] if tool_name == "Agent" => (input_preview, output_preview, stats),
            other => panic!("expected an Agent card, got {other:?}"),
        };
        let input: serde_json::Value = serde_json::from_str(input.as_deref().unwrap()).unwrap();
        assert_eq!(input["subagent_type"], "general");
        assert_eq!(input["description"], "Search docs");
        assert_eq!(input["model"], "big-pickle");
        assert_eq!(output.as_deref(), Some("found it"));
        assert_eq!(stats.total_duration_ms, Some(28_805));

        let rows: Vec<(&str, bool)> = stats
            .tool_calls
            .iter()
            .map(|call| (call.tool_name.as_str(), call.is_error))
            .collect();
        assert_eq!(rows, [("bash", false), ("read", true)]);
        assert_eq!(
            stats.tool_calls[0].input_preview.as_deref(),
            Some(r#"{"command":"ls","description":"List files"}"#)
        );
        assert_eq!(
            stats.tool_calls[1].output_preview.as_deref(),
            Some("File not found")
        );
    }

    /// A database 2.x upgraded keeps both stores, with most sessions in each.
    /// Every session is listed once, from the copy written last; a copy
    /// neither side touched since the migration is read from the legacy
    /// tables, which the migrated copy is a lossy projection of.
    #[test]
    fn an_upgraded_database_reads_each_session_from_its_live_copy() {
        use crate::parsers::AgentParser;

        let mut fx = DbFixture::new(&[&LEGACY_TABLES[..], &V2_TABLES[..]].concat());
        let v2_text = |fx: &mut DbFixture, session: &str, seq: i64, created: i64, text: &str| {
            let (kind, data) = if seq % 2 == 0 {
                (
                    "user",
                    serde_json::json!({ "time": { "created": created }, "text": text }),
                )
            } else {
                (
                    "assistant",
                    serde_json::json!({
                        "time": { "created": created },
                        "model": { "id": "m2", "providerID": "p" },
                        "content": [{ "type": "text", "text": text }]
                    }),
                )
            };
            fx.v2_message(
                session,
                &format!("msg_{session}_{seq}"),
                seq,
                kind,
                created,
                data,
            );
        };

        // Untouched since the migration: the legacy copy is read.
        fx.legacy_session("ses_a", "Untouched (legacy copy)", 1_000, 2_000);
        fx.legacy_text("ses_a", "m_a1", "user", 1_000, "question");
        fx.legacy_text("ses_a", "m_a2", "assistant", 1_500, "answer from 1.x");
        fx.v2_session(
            "ses_a",
            None,
            Some("Untouched (migrated copy)"),
            1_000,
            2_000,
        );
        v2_text(&mut fx, "ses_a", 0, 1_000, "question");
        v2_text(&mut fx, "ses_a", 1, 1_500, "answer as migrated");

        // Continued in 2.x after the migration.
        fx.legacy_session("ses_b", "Before 2.x", 3_000, 4_000);
        fx.legacy_text("ses_b", "m_b1", "user", 3_000, "question");
        fx.v2_session("ses_b", None, Some("Continued in 2.x"), 3_000, 9_000);
        v2_text(&mut fx, "ses_b", 0, 3_000, "question");
        v2_text(&mut fx, "ses_b", 1, 8_500, "answer from 2.x");

        // Continued by a 1.x binary after the migration, which only writes
        // the legacy tables.
        fx.legacy_session("ses_c", "Continued in 1.x", 5_000, 9_500);
        fx.legacy_text("ses_c", "m_c1", "user", 5_000, "question");
        fx.legacy_text("ses_c", "m_c2", "assistant", 9_000, "later answer from 1.x");
        fx.v2_session("ses_c", None, Some("Stale migrated copy"), 5_000, 6_000);
        v2_text(&mut fx, "ses_c", 0, 5_000, "question");

        // A migrated copy that kept none of the conversation must not hide
        // the legacy one.
        fx.legacy_session("ses_d", "Only the legacy copy has messages", 6_000, 7_000);
        fx.legacy_text("ses_d", "m_d1", "user", 6_000, "question");
        fx.v2_session("ses_d", None, Some("Empty migrated copy"), 6_000, 7_000);

        // But a copy that is empty because 2.x emptied it since is the live
        // one, and an empty session is not listed.
        fx.legacy_session("ses_g", "Emptied in 2.x", 500, 600);
        fx.legacy_text("ses_g", "m_g1", "user", 500, "question");
        fx.v2_session("ses_g", None, Some("Emptied in 2.x"), 500, 9_900);

        // Only in one store.
        fx.legacy_session("ses_e", "Created by 1.x after the migration", 7_000, 7_500);
        fx.legacy_text("ses_e", "m_e1", "user", 7_000, "question");
        fx.v2_session("ses_f", None, Some("Created by 2.x"), 8_000, 8_500);
        v2_text(&mut fx, "ses_f", 0, 8_000, "question");
        let (_dir, parser) = fx.build();

        let listed = parser.list_conversations().expect("list");
        let titles: Vec<(&str, &str)> = listed
            .iter()
            .map(|s| (s.id.as_str(), s.title.as_deref().unwrap_or("")))
            .collect();
        assert_eq!(
            titles,
            [
                ("ses_f", "Created by 2.x"),
                ("ses_e", "Created by 1.x after the migration"),
                ("ses_d", "Only the legacy copy has messages"),
                ("ses_c", "Continued in 1.x"),
                ("ses_b", "Continued in 2.x"),
                ("ses_a", "Untouched (legacy copy)"),
            ]
        );

        let last_text = |id: &str| {
            let detail = parser.get_conversation(id).expect("detail");
            let last = detail.turns.last().expect("a turn");
            (
                detail.summary.title.clone().unwrap_or_default(),
                text_blocks(&last.blocks).join(""),
            )
        };
        assert_eq!(
            last_text("ses_a"),
            ("Untouched (legacy copy)".into(), "answer from 1.x".into())
        );
        assert_eq!(
            last_text("ses_b"),
            ("Continued in 2.x".into(), "answer from 2.x".into())
        );
        assert_eq!(
            last_text("ses_c"),
            ("Continued in 1.x".into(), "later answer from 1.x".into())
        );
        assert_eq!(last_text("ses_f").0, "Created by 2.x");
    }

    /// A legacy message row whose `data` is given whole, plus one text part.
    fn legacy_message(
        fx: &mut DbFixture,
        session: &str,
        id: &str,
        created: i64,
        data: serde_json::Value,
    ) {
        fx.exec(
            "INSERT INTO message (id, session_id, time_created, data) VALUES (?, ?, ?, ?)",
            [
                id.into(),
                session.into(),
                created.into(),
                data.to_string().into(),
            ],
        );
        let part = serde_json::json!({ "type": "text", "text": format!("text of {id}") });
        fx.exec(
            "INSERT INTO part (id, message_id, time_created, data) VALUES (?, ?, ?, ?)",
            [
                format!("prt_{id}").into(),
                id.into(),
                created.into(),
                part.to_string().into(),
            ],
        );
    }

    /// OpenCode's own models.dev cache, reduced to the models these tests use.
    /// Project config stays off: the fixture sessions' `/work/app` is a real
    /// path on the machine running the suite.
    fn catalog_sources(root: &std::path::Path) -> super::ModelLimitSources {
        let catalog = root.join("models.json");
        std::fs::write(
            &catalog,
            serde_json::json!({
                "opencode": { "models": {
                    "big-pickle": { "limit": { "context": 200000, "input": 160000, "output": 32000 } },
                    "small": { "limit": { "context": 32000, "output": 4096 } }
                } }
            })
            .to_string(),
        )
        .expect("write catalog");
        super::ModelLimitSources {
            catalog_file: Some(catalog),
            ..Default::default()
        }
    }

    /// The case a reopened session lost its gauge over: a model only
    /// OpenCode's catalog knows (OpenCode Zen's `big-pickle`), whose window the
    /// live view got from OpenCode's own `usage_update`.
    #[test]
    fn a_reopened_session_is_sized_like_opencode_sizes_it_live() {
        use crate::parsers::AgentParser;

        let t0: i64 = 1_790_612_421_000;
        let mut fx = DbFixture::new(&LEGACY_TABLES);
        fx.legacy_session("ses_zen", "Friendly greeting", t0, t0 + 9_000);
        fx.legacy_text("ses_zen", "msg_u1", "user", t0 + 10, "hi");
        legacy_message(
            &mut fx,
            "ses_zen",
            "msg_a1",
            t0 + 20,
            serde_json::json!({
                "role": "assistant",
                "time": { "created": t0 + 20, "completed": t0 + 90 },
                "providerID": "opencode",
                "modelID": "small",
                "tokens": { "input": 1000, "output": 50, "reasoning": 0, "cache": { "read": 0, "write": 0 } }
            }),
        );
        fx.legacy_text("ses_zen", "msg_u2", "user", t0 + 100, "and again");
        legacy_message(
            &mut fx,
            "ses_zen",
            "msg_a2",
            t0 + 110,
            serde_json::json!({
                "role": "assistant",
                "time": { "created": t0 + 110, "completed": t0 + 190 },
                "providerID": "opencode",
                "modelID": "big-pickle",
                "tokens": {
                    "total": 9348, "input": 7399, "output": 10, "reasoning": 0,
                    "cache": { "read": 1939, "write": 0 }
                }
            }),
        );
        let (dir, mut parser) = fx.build();

        // Nothing to look the window up in: `big-pickle` means nothing to the
        // name guess, so only the occupancy is known.
        let stats = parser
            .get_conversation("ses_zen")
            .expect("detail")
            .session_stats
            .expect("stats");
        assert_eq!(stats.context_window_max_tokens, None);
        // `input + cache.read + cache.write` of the latest reply: its output is
        // not resident in the window, and earlier replies are not added in.
        assert_eq!(stats.context_window_used_tokens, Some(9_338));

        parser.model_limits = catalog_sources(dir.path());
        let stats = parser
            .get_conversation("ses_zen")
            .expect("detail")
            .session_stats
            .expect("stats");
        // The latest reply's model decides, not the session's first.
        assert_eq!(stats.context_window_max_tokens, Some(200_000));
        assert_eq!(stats.context_window_used_tokens, Some(9_338));
        let percent = stats.context_window_usage_percent.expect("percent");
        assert!((percent - 4.669).abs() < 0.001, "percent was {percent}");
    }

    #[test]
    fn a_2x_session_is_sized_by_the_model_its_latest_reply_named() {
        use crate::parsers::AgentParser;

        let t0: i64 = 1_790_612_421_000;
        let mut fx = DbFixture::new(&V2_TABLES);
        fx.v2_session("ses_v2", None, Some("Sized"), t0, t0 + 9_000);
        fx.v2_message(
            "ses_v2",
            "msg_01",
            1,
            "user",
            t0 + 10,
            serde_json::json!({ "time": { "created": t0 + 10 }, "text": "hi" }),
        );
        fx.v2_message(
            "ses_v2",
            "msg_02",
            2,
            "assistant",
            t0 + 20,
            serde_json::json!({
                "time": { "created": t0 + 20, "completed": t0 + 90 },
                "model": { "id": "big-pickle", "providerID": "opencode", "variant": null },
                "tokens": { "input": 3000, "output": 400, "reasoning": 0, "cache": { "read": 1000, "write": 500 } },
                "content": [{ "type": "text", "text": "hello" }]
            }),
        );
        let (dir, mut parser) = fx.build();
        parser.model_limits = catalog_sources(dir.path());

        let stats = parser
            .get_conversation("ses_v2")
            .expect("detail")
            .session_stats
            .expect("stats");
        assert_eq!(stats.context_window_max_tokens, Some(200_000));
        assert_eq!(stats.context_window_used_tokens, Some(4_500));
    }

    /// A custom provider's window lives only in the config it was declared
    /// in, and a project's config is found from the session's own directory.
    #[test]
    fn a_custom_model_is_sized_by_the_config_of_the_sessions_project() {
        use crate::parsers::AgentParser;

        let t0: i64 = 1_790_612_421_000;
        let project = tempfile::tempdir().expect("project dir");
        std::fs::create_dir(project.path().join(".git")).expect("git dir");
        std::fs::write(
            project.path().join("opencode.jsonc"),
            r#"{
                // Declared by the project, not by the catalog.
                "provider": { "X": { "models": { "xx": { "limit": { "context": 32768, }, }, }, }, },
            }"#,
        )
        .expect("project config");

        let mut fx = DbFixture::new(&LEGACY_TABLES);
        fx.exec(
            "INSERT INTO session (id, directory, title, time_created, time_updated) \
             VALUES (?, ?, 'Custom', ?, ?)",
            [
                "ses_custom".into(),
                project.path().to_string_lossy().into_owned().into(),
                t0.into(),
                (t0 + 9_000).into(),
            ],
        );
        fx.legacy_text("ses_custom", "msg_u1", "user", t0 + 10, "hi");
        legacy_message(
            &mut fx,
            "ses_custom",
            "msg_a1",
            t0 + 20,
            serde_json::json!({
                "role": "assistant",
                "time": { "created": t0 + 20, "completed": t0 + 90 },
                "providerID": "X",
                "modelID": "xx",
                "tokens": { "input": 2000, "output": 30, "reasoning": 0, "cache": { "read": 0, "write": 0 } }
            }),
        );
        let (dir, mut parser) = fx.build();
        parser.model_limits = super::ModelLimitSources {
            project_config: true,
            ..catalog_sources(dir.path())
        };

        let stats = parser
            .get_conversation("ses_custom")
            .expect("detail")
            .session_stats
            .expect("stats");
        assert_eq!(stats.context_window_max_tokens, Some(32_768));
        assert_eq!(stats.context_window_used_tokens, Some(2_000));
    }
