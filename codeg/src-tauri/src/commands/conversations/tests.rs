    use super::*;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};

    /// Serializes every test that touches the process-global [`IMPORT_GUARD`].
    ///
    /// The harness runs `#[tokio::test]`s on parallel threads of one process, so
    /// a test that *holds* the guard and a test that *calls* a guard-taking
    /// import flip each other's expected outcome: the caller sees a spurious
    /// "already in progress" instead of its real error, and the holder's
    /// `try_lock().expect(...)` panics. Both are timing-dependent, so the suite
    /// passes locally and fails on a loaded CI runner.
    ///
    /// Always take this *before* `IMPORT_GUARD` (never the reverse) so the two
    /// locks can't deadlock. Held for the whole test body.
    static IMPORT_GUARD_SERIALIZER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    // ──────────────────────────────────────────────────────────────────────
    // Delegation meta injection for historical reload. Parsers always emit
    // `ContentBlock::ToolUse { meta: None }`; without this helper, a
    // conversation reloaded from JSONL has no way to surface its
    // sub-agent children to the parent UI's read-only viewer.
    // ──────────────────────────────────────────────────────────────────────

    fn summary_child(id: i32, parent_tool_use_id: &str, status: &str) -> DbConversationSummary {
        let now = chrono::Utc::now();
        DbConversationSummary {
            id,
            folder_id: 1,
            title: None,
            title_locked: false,
            agent_type: AgentType::Codex,
            status: status.into(),
            kind: conversation::ConversationKind::Delegate,
            model: None,
            git_branch: None,
            external_id: None,
            message_count: 0,
            child_count: 0,
            created_at: now,
            updated_at: now,
            pinned_at: None,
            parent_id: Some(1),
            parent_tool_use_id: Some(parent_tool_use_id.into()),
            delegation_call_id: Some("call-1".into()),
            origin_cwd: None,
            team: None,
            tag_ids: Vec::new(),
        }
    }

    fn tool_use_turn(tool_use_id: Option<&str>, tool_name: &str) -> MessageTurn {
        tool_use_turn_with_input(tool_use_id, tool_name, None)
    }

    fn tool_use_turn_with_input(
        tool_use_id: Option<&str>,
        tool_name: &str,
        input_preview: Option<&str>,
    ) -> MessageTurn {
        MessageTurn {
            id: "t1".into(),
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::ToolUse {
                tool_use_id: tool_use_id.map(String::from),
                tool_name: tool_name.into(),
                input_preview: input_preview.map(String::from),
                status: None,
                meta: None,
            }],
            timestamp: chrono::Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
        agent_message_id: None,
        }
    }

    fn first_block_meta(turn: &MessageTurn) -> Option<&serde_json::Value> {
        turn.blocks.first().and_then(|b| match b {
            ContentBlock::ToolUse { meta, .. } => meta.as_ref(),
            _ => None,
        })
    }

    // ──────────────────────────────────────────────────────────────────────
    // In-flight user-turn stamping (cross-client viewer dedup). See
    // `apply_in_flight_message_id`.
    // ──────────────────────────────────────────────────────────────────────

    // A fixed reference instant for the in-flight turn's start, and a helper for
    // building turn timestamps relative to it (positive = after the turn began,
    // negative = a turn that started earlier).
    fn turn_started() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-05-28T00:01:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn at(offset_secs: i64) -> chrono::DateTime<chrono::Utc> {
        turn_started() + chrono::Duration::seconds(offset_secs)
    }

    fn user_text_turn(id: &str, text: &str, ts: chrono::DateTime<chrono::Utc>) -> MessageTurn {
        MessageTurn {
            id: id.into(),
            role: TurnRole::User,
            blocks: vec![ContentBlock::Text { text: text.into() }],
            timestamp: ts,
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
        agent_message_id: None,
        }
    }

    fn assistant_text_turn(
        id: &str,
        text: &str,
        ts: chrono::DateTime<chrono::Utc>,
        completed: bool,
    ) -> MessageTurn {
        MessageTurn {
            id: id.into(),
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::Text { text: text.into() }],
            timestamp: ts,
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: completed.then_some(ts),
        agent_message_id: None,
        }
    }

    fn pending_text(message_id: &str, text: &str) -> crate::acp::session_state::PendingUserMessage {
        crate::acp::session_state::PendingUserMessage {
            message_id: message_id.into(),
            blocks: vec![crate::acp::types::UserMessageBlock::Text { text: text.into() }],
        }
    }

    #[test]
    fn stamps_trailing_user_turn() {
        // Claude/Codex mid-stream: the transcript ends exactly at the in-flight
        // prompt (the assistant turn is written only on completion).
        let mut turns = vec![
            user_text_turn("turn-0", "first", at(-30)),
            assistant_text_turn("turn-1", "reply", at(-29), true),
            user_text_turn("turn-2", "hello", at(1)),
        ];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped.as_deref(), Some("msg-live"), "reports the stamped id");
        assert_eq!(turns[2].id, "msg-live");
        assert_eq!(turns[0].id, "turn-0", "earlier identical-position turn intact");
        assert_eq!(turns[1].id, "turn-1");
    }

    #[test]
    fn stamps_user_turn_before_partial_trailing_assistant_regardless_of_completion() {
        // OpenCode/Gemini mid-stream: a partial assistant turn is persisted, so
        // the tail is [user X, partial assistant Y]. The recency of the user turn
        // — not the assistant's completion flag — is what identifies the prompt,
        // so it stamps even when the trailing assistant carries a completion time
        // (as Gemini's partial always does). The partial reply is left in place
        // and its id reported: dropping it on the backend could hide a
        // just-completed reply in the end-of-turn race, so the frontend hides the
        // duplicate at render time (keyed off the reported id) while the live
        // stream is in hand instead.
        let mut turns = vec![
            user_text_turn("turn-0", "hello", at(1)),
            assistant_text_turn("turn-1", "partial...", at(2), true),
        ];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped.as_deref(), Some("msg-live"));
        assert_eq!(turns[0].id, "msg-live");
        assert_eq!(turns.len(), 2, "the partial reply is preserved (not dropped)");
        assert_eq!(turns[1].id, "turn-1", "the partial reply is untouched");
    }

    #[test]
    fn does_not_stamp_when_content_differs() {
        let mut turns = vec![
            user_text_turn("turn-0", "hello", at(1)),
            assistant_text_turn("turn-1", "partial...", at(2), false),
        ];
        let stamped = apply_in_flight_message_id(
            &mut turns,
            &pending_text("msg-live", "something else"),
            Some(turn_started()),
        );
        assert_eq!(stamped, None, "no match → nothing reported");
        assert_eq!(turns[0].id, "turn-0", "no match → left untouched");
    }

    #[test]
    fn does_not_stamp_when_message_id_collides_with_another_turn() {
        // Defense in depth: an (untrusted) broadcast id equal to an existing
        // parser turn id must not be stamped onto the in-flight prompt — two turns
        // sharing an id could let the frontend's id-keyed dedup hide one. Here the
        // broadcast id "turn-0" already names the first turn, so the in-flight
        // prompt is left under its parser id and nothing is reported.
        let mut turns = vec![
            user_text_turn("turn-0", "earlier", at(-30)),
            assistant_text_turn("turn-1", "reply", at(-29), true),
            user_text_turn("turn-2", "hello", at(1)),
        ];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("turn-0", "hello"), Some(turn_started()));
        assert_eq!(stamped, None, "colliding broadcast id → no stamp");
        assert_eq!(turns[2].id, "turn-2", "the in-flight prompt keeps its parser id");
        assert_eq!(turns[0].id, "turn-0", "the colliding turn is untouched");
    }

    #[test]
    fn does_not_reach_back_into_an_earlier_round() {
        // The matching prompt sits buried before another full user/assistant
        // round. The walk is bounded by the recency gate, not by a turn count:
        // it stops at the first turn older than this turn's start, which is the
        // completed reply above — so the identical prompt behind it is out of
        // reach however short the transcript is.
        let mut turns = vec![
            user_text_turn("turn-0", "hello", at(-30)),
            assistant_text_turn("turn-1", "a", at(-29), true),
            user_text_turn("turn-2", "ok", at(1)),
            assistant_text_turn("turn-3", "b", at(2), false),
        ];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped, None, "nothing in the running turn matches");
        assert_eq!(turns[0].id, "turn-0");
        assert_eq!(turns[2].id, "turn-2", "non-matching tail user turn untouched");
    }

    #[test]
    fn stamps_nothing_when_the_clocks_cannot_say_which_continue_this_is() {
        // `cline.rs` and `antigravity.rs` fall back to the PARSE INSTANT when a
        // record carries no usable time, and a parse instant is by construction
        // at or after `started_at` — so for those the recency gate never fires
        // and NOTHING here says where this turn began. Two rounds of "continue"
        // are then indistinguishable from one round the user steered with the
        // same word, and stamping the older one would make
        // `visiblePersistedTurns` hide every assistant turn after it.
        let parsed_at = at(1);
        let mut turns = vec![
            user_text_turn("turn-0", "continue", parsed_at),
            assistant_text_turn("turn-1", "done", parsed_at, true),
            user_text_turn("turn-2", "continue", parsed_at),
        ];
        let stamped = apply_in_flight_message_id(
            &mut turns,
            &pending_text("msg-live", "continue"),
            Some(turn_started()),
        );
        assert_eq!(stamped, None, "ambiguous and unprovable → stamp nothing");
        assert_eq!(turns[0].id, "turn-0", "untouched");
        assert_eq!(turns[2].id, "turn-2", "untouched");
    }

    #[test]
    fn stamps_an_unambiguous_match_even_with_no_proof_of_where_the_turn_began() {
        // Same parse-instant transcript, but the prompt's text occurs once. A
        // single candidate in the WHOLE transcript cannot be an earlier round's
        // prompt confused with this one — the text is unique to it — so the
        // clocks have nothing left to disambiguate and the stamp is safe.
        //
        // This is also the ordinary first turn of a conversation, where there is
        // simply nothing older than `started_at` for the gate to find.
        let parsed_at = at(1);
        let mut turns = vec![
            user_text_turn("turn-0", "run the tests", parsed_at),
            assistant_text_turn("turn-1", "first half", parsed_at, false),
            user_text_turn("turn-2", "also lint", parsed_at),
        ];
        let stamped = apply_in_flight_message_id(
            &mut turns,
            &pending_text("msg-live", "run the tests"),
            Some(turn_started()),
        );
        assert_eq!(stamped.as_deref(), Some("msg-live"));
        assert_eq!(turns[0].id, "msg-live");
    }

    #[test]
    fn a_long_reply_never_costs_the_stamp() {
        // The bound counts USER turns, not turns. Every mid-turn-persisting
        // parser emits one assistant turn per assistant record, so a real
        // Claude round runs to a median of 44 of them and a p90 of 317 — a
        // bound on turns walked would void the stamp part-way through most
        // ordinary rounds, mid-turn, which is the failure this walk removes.
        let mut turns = vec![
            user_text_turn("old", "earlier", at(-30)),
            assistant_text_turn("old-a", "done", at(-29), true),
            user_text_turn("turn-0", "hello", at(1)),
        ];
        for i in 0..400 {
            turns.push(assistant_text_turn(
                &format!("a-{i}"),
                "step",
                at(2),
                false,
            ));
        }
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped.as_deref(), Some("msg-live"));
        assert_eq!(turns[2].id, "msg-live");
    }

    #[test]
    fn stops_comparing_prompts_once_the_walk_is_plainly_not_in_one_turn() {
        // The cost bound. Only reachable when the gate never fires, since a real
        // window holds the prompt plus whatever was sent mid-turn; here every
        // turn is a user turn with a parse instant, so the walk would otherwise
        // rebuild a content signature for every prompt ever sent, on every
        // detail fetch. Refusing is the same safe direction as ambiguity.
        let parsed_at = at(1);
        let mut turns = vec![user_text_turn("wanted", "hello", parsed_at)];
        for i in 0..MAX_IN_FLIGHT_WALK_USER_TURNS {
            turns.push(user_text_turn(&format!("u-{i}"), "filler", parsed_at));
        }
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped, None, "past the cost bound, stamp nothing");
        assert_eq!(turns[0].id, "wanted", "untouched");

        // One fewer and the same transcript is inside the bound, so it is the
        // bound that refused above and not some other gate.
        turns.pop();
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped.as_deref(), Some("msg-live"));
    }

    #[test]
    fn stamps_the_prompt_behind_a_reply_the_parser_split() {
        // Previously refused: the rule was "the tail, or the user before a
        // SINGLE trailing assistant", so a deeper assistant tail bailed.
        //
        // That bound predates the recency gate and is redundant beside it. A
        // user turn at or after this turn's start was persisted DURING it, and
        // its content is the pending prompt's — there is nothing else it could
        // be, whatever the agent has written since. Refusing here instead cost
        // the stamp for the whole of every OpenCode/Gemini turn whose partial
        // reply the parser split in two, which is exactly the shape the
        // frontend's partial suppression exists for.
        let mut turns = vec![
            user_text_turn("turn-0", "hello", at(1)),
            assistant_text_turn("turn-1", "a", at(2), false),
            assistant_text_turn("turn-2", "b", at(3), false),
        ];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped.as_deref(), Some("msg-live"));
        assert_eq!(turns[0].id, "msg-live");
    }

    #[test]
    fn stamps_the_prompt_behind_a_message_sent_mid_turn() {
        // The steering shape. The agent writes a message the user sent DURING
        // the turn into its own transcript, so the tail is that message and its
        // content is not the prompt's. The old tail rule reported nothing here,
        // in the middle of the turn, and every consumer reads that as "settled".
        let mut turns = vec![
            user_text_turn("turn-0", "hello", at(1)),
            assistant_text_turn("turn-1", "first half", at(2), false),
            user_text_turn("turn-2", "also check the tests", at(3)),
        ];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped.as_deref(), Some("msg-live"));
        assert_eq!(turns[0].id, "msg-live", "the prompt keeps the stamp");
        assert_eq!(turns[2].id, "turn-2", "the mid-turn message is untouched");
    }

    #[test]
    fn stamps_the_prompt_not_a_mid_turn_message_repeating_its_words() {
        // "continue" is the most repeatable thing a person steers with, and the
        // prompt itself may have been the same word. Both copies match by
        // content and both are inside the turn, so recency cannot separate
        // them — ORDER does: the agent writes the prompt before anything it
        // produces, so the earliest copy in the turn is the prompt. Stamping
        // the later one would move the anchor past the reply's first half and
        // leave it beside the live copy of itself.
        //
        // An earlier round sits in front, which is what lets the recency gate
        // fire and prove where this turn begins; without that proof the two
        // copies are ambiguous and the walk refuses instead (see below).
        let mut turns = vec![
            user_text_turn("old", "hello", at(-30)),
            assistant_text_turn("old-a", "hi", at(-29), true),
            user_text_turn("turn-0", "continue", at(1)),
            assistant_text_turn("turn-1", "working", at(2), false),
            user_text_turn("turn-2", "continue", at(3)),
        ];
        let stamped = apply_in_flight_message_id(
            &mut turns,
            &pending_text("msg-live", "continue"),
            Some(turn_started()),
        );
        assert_eq!(stamped.as_deref(), Some("msg-live"));
        assert_eq!(turns[2].id, "msg-live", "the earliest copy is the prompt");
        assert_eq!(turns[4].id, "turn-2", "the mid-turn repeat is untouched");
        assert_eq!(turns[0].id, "old", "the earlier round is untouched");
    }

    #[test]
    fn stamps_image_user_turn_only_on_exact_match() {
        let image_turn = |id: &str, data: &str| MessageTurn {
            id: id.into(),
            role: TurnRole::User,
            blocks: vec![ContentBlock::Image {
                data: data.into(),
                mime_type: "image/png".into(),
                uri: Some("file:///shot.png".into()),
            }],
            timestamp: at(1),
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
        agent_message_id: None,
        };
        let pending_image = |message_id: &str, data: &str| {
            crate::acp::session_state::PendingUserMessage {
                message_id: message_id.into(),
                blocks: vec![crate::acp::types::UserMessageBlock::Image {
                    data: data.into(),
                    mime_type: "image/png".into(),
                }],
            }
        };

        let mut turns = vec![image_turn("turn-0", "AAAA")];
        apply_in_flight_message_id(&mut turns, &pending_image("msg-live", "AAAA"), Some(turn_started()));
        assert_eq!(turns[0].id, "msg-live", "uri difference is ignored, data matches");

        let mut turns = vec![image_turn("turn-0", "AAAA")];
        apply_in_flight_message_id(&mut turns, &pending_image("msg-live", "BBBB"), Some(turn_started()));
        assert_eq!(turns[0].id, "turn-0", "different image bytes → no stamp");
    }

    #[test]
    fn empty_turns_is_a_noop() {
        let mut turns: Vec<MessageTurn> = vec![];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(stamped, None);
        assert!(turns.is_empty());
    }

    #[test]
    fn does_not_stamp_a_prior_identical_prompt_by_recency() {
        // The repeated-identical-prompt case: a prior 'continue' is already
        // answered, and a new identical 'continue' is in flight but not yet
        // persisted. The prior prompt predates the turn start, so the recency
        // gate refuses to stamp it — otherwise the new prompt (whose optimistic
        // copy shares the broadcast id) would be hidden by the frontend's
        // keep-first user dedup. A completed trailing reply makes no difference;
        // recency, not completion, is the signal.
        let mut turns = vec![
            user_text_turn("turn-0", "continue", at(-60)),
            assistant_text_turn("turn-1", "done", at(-58), true),
        ];
        apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "continue"), Some(turn_started()));
        assert_eq!(turns[0].id, "turn-0", "older identical prompt → untouched");
    }

    #[test]
    fn does_not_stamp_when_started_at_is_unknown() {
        // Without a turn-start reference the recency gate can't run, so nothing
        // is stamped (keep-visible default).
        let mut turns = vec![user_text_turn("turn-0", "hello", at(1))];
        apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), None);
        assert_eq!(turns[0].id, "turn-0");
    }

    #[test]
    fn stamps_user_turn_persisted_at_turn_start() {
        // The in-flight prompt is persisted at/after the recorded turn start (the
        // backend broadcasts `UserMessage` before issuing the agent request), so
        // a turn exactly at the start qualifies — the boundary is inclusive.
        let mut turns = vec![user_text_turn("turn-0", "hello", at(0))];
        apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(turns[0].id, "msg-live", "persisted exactly at the start is in-flight");
    }

    #[test]
    fn does_not_stamp_user_turn_persisted_before_turn_start() {
        // Strict gate, no backward tolerance: a turn even one second before the
        // start belongs to an earlier turn, never the in-flight prompt.
        let mut turns = vec![user_text_turn("turn-0", "hello", at(-1))];
        apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "hello"), Some(turn_started()));
        assert_eq!(turns[0].id, "turn-0", "one second before the start is not in-flight");
    }

    #[test]
    fn does_not_stamp_fast_prior_prompt_before_completed_trailing_reply() {
        // The dangerous repeated-prompt race: a prior 'continue' completed within
        // a second, the user re-sends 'continue', and a refetch lands before the
        // new copy is persisted — so the tail is [prior user, completed assistant]
        // (the OpenCode/Gemini n-2 shape). The prior user turn predates the turn
        // start, so it is left alone; stamping it would let the frontend's
        // keep-first dedup hide the genuinely new prompt. A backward tolerance
        // would reopen exactly this hole.
        let mut turns = vec![
            user_text_turn("turn-0", "continue", at(-1)),
            assistant_text_turn("turn-1", "done", at(0), true),
        ];
        let stamped =
            apply_in_flight_message_id(&mut turns, &pending_text("msg-live", "continue"), Some(turn_started()));
        assert_eq!(stamped, None, "fast prior identical prompt → nothing reported");
        assert_eq!(turns[0].id, "turn-0", "fast prior identical prompt → untouched");
        assert_eq!(turns.len(), 2, "the prior completed reply is preserved");
    }

    #[test]
    fn inject_delegation_meta_populates_completed_child() {
        let mut turns = vec![tool_use_turn(
            Some("tu-1"),
            "mcp__codeg-mcp__delegate_to_agent",
        )];
        let children = vec![summary_child(42, "tu-1", "completed")];
        inject_delegation_meta(&mut turns, &children);
        let meta = first_block_meta(&turns[0]).expect("meta should be set");
        let inner = meta.get("codeg.delegation").expect("codeg.delegation key");
        assert_eq!(inner["status"], "completed");
        assert_eq!(inner["child_conversation_id"], 42);
        assert!(
            inner.get("error_code").is_none(),
            "completed has no error_code"
        );
    }

    fn tool_result_turn(tool_use_id: &str, output: &str) -> MessageTurn {
        MessageTurn {
            id: "t2".into(),
            // Tool results are folded into the assistant turn that owns them.
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::ToolResult {
                tool_use_id: Some(tool_use_id.into()),
                output_preview: Some(output.into()),
                is_error: false,
                agent_stats: None,
                images: Vec::new(),
            }],
            timestamp: chrono::Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
        agent_message_id: None,
        }
    }

    /// codex's rollout names the call `call_<id>` while the broker recorded the
    /// ACP-side `exec-<uuid>` — the two never meet, so the card lost its
    /// `child_conversation_id` (and the "查看会话" affordance) entirely. The
    /// broker's task id, echoed in the ack the model received, is the bridge.
    #[test]
    fn inject_delegation_meta_falls_back_to_the_task_id() {
        let mut turns = vec![
            tool_use_turn(Some("call_73UFK2"), "mcp__codeg_mcp__delegate_to_agent"),
            tool_result_turn(
                "call_73UFK2",
                "Delegation successful. task_id=8ff4c14c-740c-4482-b758-8f2091f97063. \
                 Call get_delegation_status with this id in the task_ids array.",
            ),
        ];
        let mut child = summary_child(2890, "exec-0fb6db94-3042-4cc4-b492-2edd1804c1fa", "completed");
        child.delegation_call_id = Some("8ff4c14c-740c-4482-b758-8f2091f97063".into());

        inject_delegation_meta(&mut turns, &[child]);

        let inner = first_block_meta(&turns[0])
            .and_then(|m| m.get("codeg.delegation").cloned())
            .expect("meta should be set");
        assert_eq!(inner["child_conversation_id"], 2890);
    }

    #[test]
    fn inject_delegation_meta_does_not_bind_a_foreign_task_id() {
        let mut turns = vec![
            tool_use_turn(Some("call_a"), "delegate_to_agent"),
            tool_result_turn("call_a", "Delegation successful. task_id=aaaa."),
        ];
        let mut child = summary_child(1, "exec-zzz", "completed");
        child.delegation_call_id = Some("bbbb".into());

        inject_delegation_meta(&mut turns, &[child]);

        assert!(
            first_block_meta(&turns[0]).is_none(),
            "a different task's child must not be bound"
        );
    }

    /// `resume_delegation` names its task in its own ARGUMENTS, and owns no
    /// `parent_tool_use_id` (it re-binds to the original delegate call's id,
    /// which lives in an earlier block). Without this injection the resumed
    /// card would be stuck on the `running` its ack reported, because the
    /// child's real outcome only ever landed on the DB row.
    #[test]
    fn inject_delegation_meta_binds_a_resume_call_by_its_task_id_argument() {
        let mut turns = vec![tool_use_turn_with_input(
            Some("tu-resume"),
            "mcp__codeg-mcp__resume_delegation",
            Some(r#"{"task_id":"b0858712-9257","reason":"the app was killed"}"#),
        )];
        let mut child = summary_child(9, "tu-original-delegate", "completed");
        child.delegation_call_id = Some("b0858712-9257".into());
        child.title = Some("Build the /test4 sandbox page".into());

        inject_delegation_meta(&mut turns, &[child]);

        let inner = first_block_meta(&turns[0])
            .and_then(|m| m.get("codeg.delegation").cloned())
            .expect("meta should be set");
        // The CHILD's real status, not the `running` the resume ack froze.
        assert_eq!(inner["status"], "completed");
        assert_eq!(inner["child_conversation_id"], 9);
        assert_eq!(inner["task_id"], "b0858712-9257");
        assert_eq!(inner["agent_type"], "codex");
        assert_eq!(inner["task_preview"], "Build the /test4 sandbox page");
    }

    #[test]
    fn inject_delegation_meta_does_not_bind_a_resume_call_to_a_foreign_task() {
        let mut turns = vec![tool_use_turn_with_input(
            Some("tu-resume"),
            "resume_delegation",
            Some(r#"{"task_id":"aaaa"}"#),
        )];
        let mut child = summary_child(9, "tu-x", "completed");
        child.delegation_call_id = Some("bbbb".into());

        inject_delegation_meta(&mut turns, &[child]);

        assert!(
            first_block_meta(&turns[0]).is_none(),
            "a different task's child must not be bound to this resume"
        );
    }

    #[test]
    fn parse_resume_task_id_reads_the_argument_object() {
        assert_eq!(
            parse_resume_task_id(r#"{"task_id":"abc-123","reason":"crashed"}"#).as_deref(),
            Some("abc-123")
        );
        assert_eq!(parse_resume_task_id(r#"{"task_id":"  "}"#), None);
        assert_eq!(parse_resume_task_id(r#"{"reason":"crashed"}"#), None);
        assert_eq!(parse_resume_task_id("not json"), None);
    }

    /// Hosts don't all persist the bare argument object, and a preview is
    /// allowed to be cut off. Every shape here reaches `inject_delegation_meta`
    /// in practice, and each one that fails to yield an id leaves the reloaded
    /// resume card frozen on its own ack with no task text.
    #[test]
    fn parse_resume_task_id_peels_host_wrappers_and_survives_truncation() {
        // CodeBuddy's DeferExecuteTool wrapper — `parsers::codebuddy` leaves
        // `params` on `input_preview` on purpose, for readers to peel.
        assert_eq!(
            parse_resume_task_id(
                r#"{"toolName":"mcp__codeg-mcp__resume_delegation","params":{"task_id":"abc-123"}}"#
            )
            .as_deref(),
            Some("abc-123")
        );
        // Antigravity's `{"arguments": {...}}`.
        assert_eq!(
            parse_resume_task_id(r#"{"arguments":{"task_id":"abc-123","reason":"x"}}"#).as_deref(),
            Some("abc-123")
        );
        // Cursor's `{providerIdentifier, toolName, args}`.
        assert_eq!(
            parse_resume_task_id(
                r#"{"providerIdentifier":"codeg-mcp","toolName":"resume_delegation","args":{"task_id":"abc-123"}}"#
            )
            .as_deref(),
            Some("abc-123")
        );
        // …and the same wrapper with the arguments stringified.
        assert_eq!(
            parse_resume_task_id(r#"{"arguments":"{\"task_id\":\"abc-123\"}"}"#).as_deref(),
            Some("abc-123")
        );
        // A long `reason` pushes past the parsers' preview cap, so the JSON
        // never closes — but the id, written first, survived.
        assert_eq!(
            parse_resume_task_id(r#"{"task_id":"abc-123","reason":"the app was ki"#).as_deref(),
            Some("abc-123")
        );
        // A wrapper key present but carrying something unreadable must not
        // shadow a usable top-level id.
        assert_eq!(
            parse_resume_task_id(r#"{"params":"not json","task_id":"abc-123"}"#).as_deref(),
            Some("abc-123")
        );
    }

    #[test]
    fn parse_delegate_task_id_reads_both_ack_shapes() {
        assert_eq!(
            parse_delegate_task_id("Delegation successful. task_id=8ff4c14c-740c. Call …")
                .as_deref(),
            Some("8ff4c14c-740c")
        );
        assert_eq!(
            parse_delegate_task_id(r#"{"task_id":"abc-123","status":"running"}"#).as_deref(),
            Some("abc-123")
        );
        assert_eq!(parse_delegate_task_id("no id here"), None);
        assert_eq!(parse_delegate_task_id("task_id="), None);
    }

    #[test]
    fn inject_delegation_meta_maps_in_progress_to_running() {
        let mut turns = vec![tool_use_turn(Some("tu-1"), "delegate_to_agent")];
        let children = vec![summary_child(7, "tu-1", "in_progress")];
        inject_delegation_meta(&mut turns, &children);
        let inner = first_block_meta(&turns[0])
            .unwrap()
            .get("codeg.delegation")
            .unwrap();
        assert_eq!(inner["status"], "running");
        assert_eq!(inner["child_conversation_id"], 7);
    }

    #[test]
    fn inject_delegation_meta_maps_pending_review_to_completed() {
        // `pending_review` is the DB status written after a successful
        // `TurnComplete { stop_reason: "end_turn" }` (see acp/lifecycle.rs).
        // The live broker maps that same child outcome to delegation meta
        // `status: "completed"` (see broker.rs Ok arm). Historical reload
        // must agree, otherwise a finished sub-agent shows a stale
        // "running" badge until the user reloads again.
        let mut turns = vec![tool_use_turn(Some("tu-1"), "delegate_to_agent")];
        let children = vec![summary_child(11, "tu-1", "pending_review")];
        inject_delegation_meta(&mut turns, &children);
        let inner = first_block_meta(&turns[0])
            .unwrap()
            .get("codeg.delegation")
            .unwrap();
        assert_eq!(inner["status"], "completed");
        assert_eq!(inner["child_conversation_id"], 11);
    }

    #[test]
    fn inject_delegation_meta_maps_cancelled_to_failed_without_error_code() {
        // `Cancelled` covers both user-cancel and turn-failure outcomes
        // (refusal, max_tokens, max_turn_requests, empty, unknown — see
        // acp/lifecycle.rs TurnComplete branch). The DB does not persist
        // the broker's distinct `error_code` per failure mode, so a
        // hard-coded `"canceled"` would mislabel every non-cancel failure
        // as user-cancel. Emit `failed` without a code instead.
        let mut turns = vec![tool_use_turn(Some("tu-1"), "delegate_to_agent")];
        let children = vec![summary_child(9, "tu-1", "cancelled")];
        inject_delegation_meta(&mut turns, &children);
        let inner = first_block_meta(&turns[0])
            .unwrap()
            .get("codeg.delegation")
            .unwrap();
        assert_eq!(inner["status"], "failed");
        assert!(
            inner.get("error_code").is_none(),
            "DB cannot distinguish cancel from other failures, must not claim 'canceled'"
        );
    }

    #[test]
    fn inject_delegation_meta_skips_non_delegation_tool_calls() {
        let mut turns = vec![tool_use_turn(Some("tu-1"), "bash")];
        let children = vec![summary_child(42, "tu-1", "completed")];
        inject_delegation_meta(&mut turns, &children);
        assert!(
            first_block_meta(&turns[0]).is_none(),
            "non-delegation tool_name must not get meta even on tool_use_id match"
        );
    }

    #[test]
    fn inject_delegation_meta_skips_blocks_without_tool_use_id() {
        let mut turns = vec![tool_use_turn(None, "delegate_to_agent")];
        let children = vec![summary_child(42, "tu-1", "completed")];
        inject_delegation_meta(&mut turns, &children);
        assert!(first_block_meta(&turns[0]).is_none());
    }

    #[test]
    fn inject_delegation_meta_preserves_live_broker_meta() {
        // Defensive: even though parsers always emit `meta: None`, a future
        // snapshot path could carry a live broker write. Don't clobber it.
        let pre_existing = serde_json::json!({ "codeg.delegation": { "status": "running", "child_conversation_id": 999 } });
        let mut turns = vec![MessageTurn {
            id: "t1".into(),
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::ToolUse {
                tool_use_id: Some("tu-1".into()),
                tool_name: "delegate_to_agent".into(),
                input_preview: None,
                status: None,
                meta: Some(pre_existing.clone()),
            }],
            timestamp: chrono::Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            completed_at: None,
        agent_message_id: None,
        }];
        let children = vec![summary_child(42, "tu-1", "completed")];
        inject_delegation_meta(&mut turns, &children);
        // The 999 (broker-written) survives — DB-derived 42 is not used here.
        let inner = first_block_meta(&turns[0])
            .unwrap()
            .get("codeg.delegation")
            .unwrap();
        assert_eq!(inner["child_conversation_id"], 999);
        assert_eq!(inner["status"], "running");
    }

    #[test]
    fn inject_delegation_meta_no_op_when_children_empty() {
        let mut turns = vec![tool_use_turn(Some("tu-1"), "delegate_to_agent")];
        inject_delegation_meta(&mut turns, &[]);
        assert!(first_block_meta(&turns[0]).is_none());
    }

    #[test]
    fn inject_delegation_meta_unmatched_tool_use_id_left_alone() {
        let mut turns = vec![tool_use_turn(Some("tu-other"), "delegate_to_agent")];
        let children = vec![summary_child(42, "tu-1", "completed")];
        inject_delegation_meta(&mut turns, &children);
        assert!(first_block_meta(&turns[0]).is_none());
    }

    #[tokio::test]
    async fn get_folder_conversation_core_injects_meta_for_real_child() {
        // Seed a parent and a delegation child; the parent has no external_id
        // (no JSONL on disk), so `turns` returns empty — but we still want to
        // exercise the children-fetch + injection short-circuit cleanly.
        // The richer end-to-end (with parser turns) is covered by the unit
        // tests above; here we just verify the wiring inside the _core fn
        // doesn't error on the join path.
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-inject-test").await;
        let parent_id = create_conversation_core(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .expect("parent");
        // Attach a child to this parent via the delegation-link path.
        let link = crate::acp::delegation::spawner::DelegationLink {
            parent_conversation_id: parent_id,
            parent_tool_use_id: "tu-historical".into(),
            delegation_call_id: "call-historical".into(),
            member_role_id: None,
        };
        conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(link),
        )
        .await
        .expect("child");
        // Parent has no external_id → no JSONL → no turns to inject into.
        // The call must still succeed without error.
        let (detail, _parsed_title) = get_folder_conversation_core(&db.conn, parent_id)
            .await
            .expect("load");
        assert_eq!(detail.summary.id, parent_id);
        assert!(detail.turns.is_empty());
    }

    #[tokio::test]
    async fn create_conversation_core_happy_path() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-conv-test-1").await;
        let id = create_conversation_core(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("hello".into()),
            None,
        )
        .await
        .expect("create");
        assert!(id > 0, "expected positive conversation id, got {id}");

        let summary = conversation_service::get_by_id(&db.conn, id)
            .await
            .expect("read back");
        assert_eq!(summary.folder_id, folder_id);
        assert_eq!(summary.agent_type, AgentType::ClaudeCode);
    }

    #[tokio::test]
    async fn create_conversation_core_non_git_path_yields_no_branch() {
        let db = fresh_in_memory_db().await;
        // Use a tempdir that's guaranteed not a git repo (no .git).
        let temp = tempfile::tempdir().expect("tempdir");
        let folder_id = seed_folder(&db, &temp.path().to_string_lossy()).await;
        let id = create_conversation_core(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("create succeeds even without git");
        let summary = conversation_service::get_by_id(&db.conn, id)
            .await
            .expect("read back");
        assert!(
            summary.git_branch.is_none(),
            "non-git path should produce no branch, got: {:?}",
            summary.git_branch
        );
    }

    #[tokio::test]
    async fn create_conversation_core_missing_folder_still_creates() {
        // FK on folder_id is not enforced (no FK constraint in schema/PRAGMA),
        // so creating a conversation against an unknown folder_id should not
        // panic. detect_git_branch is skipped because folder lookup returns None.
        let db = fresh_in_memory_db().await;
        let result = create_conversation_core(&db.conn, 999_999, AgentType::Gemini, None, None).await;
        // Behavior contract: either success (current FK-loose behavior) or a
        // database error — never panic. Accept both.
        match result {
            Ok(id) => assert!(id > 0),
            Err(err) => {
                let msg = format!("{err:?}");
                assert!(
                    msg.to_lowercase().contains("foreign")
                        || msg.to_lowercase().contains("constraint")
                        || msg.to_lowercase().contains("999999"),
                    "unexpected error shape: {msg}"
                );
            }
        }
    }

    #[tokio::test]
    async fn create_chat_conversation_core_creates_dir_folder_and_conversation() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let result = create_chat_conversation_core(
            &db.conn,
            data_dir.path(),
            AgentType::ClaudeCode,
            Some("hello chat".into()),
            None,
        )
        .await
        .expect("create chat conversation");

        // The backing folder is a hidden, top-level chat folder.
        assert_eq!(
            result.folder.kind,
            FolderKind::Chat,
            "folder must be a chat folder"
        );
        assert_eq!(result.folder.parent_id, None);
        assert_eq!(result.folder_id, result.folder.id);
        assert!(
            result
                .folder
                .path
                .starts_with(&*data_dir.path().to_string_lossy()),
            "scratch path under data dir: {}",
            result.folder.path
        );
        // The dated scratch dir exists on disk.
        assert!(
            std::path::Path::new(&result.folder.path).is_dir(),
            "scratch dir created"
        );

        // The conversation points at the hidden folder, with no git branch.
        let summary = conversation_service::get_by_id(&db.conn, result.conversation_id)
            .await
            .expect("read back");
        assert_eq!(summary.folder_id, result.folder_id);
        assert_eq!(summary.agent_type, AgentType::ClaudeCode);
        assert!(summary.git_branch.is_none());

        // It surfaces in the default sidebar query (active-folder scope).
        let rows = list_all_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            ListAllConversationsOptions::default(),
        )
        .await
        .expect("list");
        assert!(rows.iter().any(|c| c.id == result.conversation_id));
    }

    #[tokio::test]
    async fn create_chat_dir_core_creates_dated_dir_without_db_rows() {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let path = create_chat_dir_core(data_dir.path()).expect("create chat dir");

        assert!(std::path::Path::new(&path).is_dir(), "scratch dir exists");
        assert!(
            path.starts_with(&*data_dir.path().to_string_lossy()),
            "under data dir: {path}"
        );
        assert!(
            path.contains("chat-sessions"),
            "date-grouped under chat-sessions: {path}"
        );
        // Two calls mint distinct directories (uuid segment).
        let other = create_chat_dir_core(data_dir.path()).expect("second chat dir");
        assert_ne!(path, other, "each prepare gets its own dir");
    }

    #[tokio::test]
    async fn create_chat_conversation_core_reuses_existing_dir() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        // Eager step: mint the scratch dir first (as the frontend does on select).
        let prepared = create_chat_dir_core(data_dir.path()).expect("prepare dir");

        let result = create_chat_conversation_core(
            &db.conn,
            data_dir.path(),
            AgentType::ClaudeCode,
            None,
            Some(prepared.as_str()),
        )
        .await
        .expect("create chat conversation reusing dir");

        // The conversation's hidden folder points at the SAME pre-created dir —
        // no second directory was minted, so the ACP cwd never moved.
        assert_eq!(
            result.folder.path, prepared,
            "reuses the eagerly-created scratch dir"
        );

        // Exactly one uuid dir exists under that date bucket.
        let date_dir = std::path::Path::new(&prepared)
            .parent()
            .expect("date dir")
            .to_path_buf();
        let count = std::fs::read_dir(&date_dir)
            .expect("read date dir")
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .count();
        assert_eq!(count, 1, "no duplicate scratch dir created");
    }

    #[tokio::test]
    async fn cleanup_chat_folder_soft_deletes_hidden_folder() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let res =
            create_chat_conversation_core(&db.conn, data_dir.path(), AgentType::Codex, None, None)
                .await
                .expect("create");

        // Before cleanup the hidden folder is active.
        assert!(folder_service::get_folder_by_id(&db.conn, res.folder_id)
            .await
            .unwrap()
            .is_some());

        delete_conversation_core(&db.conn, res.conversation_id)
            .await
            .expect("delete conversation");
        cleanup_chat_folder_for_deleted_conversation(&db.conn, res.folder_id).await;

        // After cleanup the hidden folder is soft-deleted (no longer returned),
        // so it stops counting toward the active-folder scope. The on-disk dir is
        // intentionally left in place.
        assert!(folder_service::get_folder_by_id(&db.conn, res.folder_id)
            .await
            .unwrap()
            .is_none());
        assert!(
            std::path::Path::new(&res.folder.path).is_dir(),
            "scratch dir is intentionally retained on delete"
        );
    }

    // ── Orphan chat scratch-dir GC ────────────────────────────────────────────
    // The GC walks the real `chat-sessions` tree under a tempdir; the in-memory
    // DB only supplies the live-chat-folder path set (matching the chat tests
    // above). `Duration::ZERO` forces "always stale" so removal is deterministic.

    #[tokio::test]
    async fn gc_removes_pre_send_orphan_scratch_dir() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        // Eager pre-send dir: minted, but never bound to a conversation/folder.
        let orphan = create_chat_dir_core(data_dir.path()).expect("prepare dir");
        assert!(std::path::Path::new(&orphan).is_dir());

        let removed = gc_orphan_chat_dirs_core_with_threshold(
            &db.conn,
            data_dir.path(),
            std::time::Duration::ZERO,
        )
        .await
        .expect("gc");

        assert_eq!(removed, 1, "the unbound pre-send dir is reclaimed");
        assert!(
            !std::path::Path::new(&orphan).exists(),
            "orphan scratch dir removed"
        );
        // Emptied date bucket is cleaned up too.
        let date_dir = std::path::Path::new(&orphan).parent().expect("date dir");
        assert!(!date_dir.exists(), "emptied date bucket removed");
    }

    #[tokio::test]
    async fn gc_spares_live_chat_dir() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let res =
            create_chat_conversation_core(&db.conn, data_dir.path(), AgentType::Codex, None, None)
                .await
                .expect("create");

        let removed = gc_orphan_chat_dirs_core_with_threshold(
            &db.conn,
            data_dir.path(),
            std::time::Duration::ZERO,
        )
        .await
        .expect("gc");

        assert_eq!(removed, 0, "a dir bound to a live chat folder is spared");
        assert!(
            std::path::Path::new(&res.folder.path).is_dir(),
            "live chat dir retained"
        );
    }

    #[tokio::test]
    async fn gc_reclaims_soft_deleted_chat_dir() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let res =
            create_chat_conversation_core(&db.conn, data_dir.path(), AgentType::Codex, None, None)
                .await
                .expect("create");
        delete_conversation_core(&db.conn, res.conversation_id)
            .await
            .expect("delete conversation");
        cleanup_chat_folder_for_deleted_conversation(&db.conn, res.folder_id).await;
        // Cleanup soft-deletes the folder row but intentionally leaves the dir.
        assert!(std::path::Path::new(&res.folder.path).is_dir());

        let removed = gc_orphan_chat_dirs_core_with_threshold(
            &db.conn,
            data_dir.path(),
            std::time::Duration::ZERO,
        )
        .await
        .expect("gc");

        assert_eq!(removed, 1, "the soft-deleted (not live) dir is reclaimed");
        assert!(
            !std::path::Path::new(&res.folder.path).exists(),
            "post-delete scratch dir removed"
        );
    }

    #[tokio::test]
    async fn gc_spares_fresh_dir_below_threshold() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let fresh = create_chat_dir_core(data_dir.path()).expect("prepare dir");

        // A 10-minute threshold spares a dir an in-flight draft just minted.
        let removed = gc_orphan_chat_dirs_core_with_threshold(
            &db.conn,
            data_dir.path(),
            std::time::Duration::from_secs(600),
        )
        .await
        .expect("gc");

        assert_eq!(removed, 0, "a fresh dir below the staleness threshold is spared");
        assert!(
            std::path::Path::new(&fresh).is_dir(),
            "fresh dir retained (anti-race)"
        );
    }

    #[tokio::test]
    async fn gc_missing_root_is_noop() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        // No `chat-sessions` dir exists at all.
        let removed = gc_orphan_chat_dirs_core_with_threshold(
            &db.conn,
            data_dir.path(),
            std::time::Duration::ZERO,
        )
        .await
        .expect("gc");

        assert_eq!(removed, 0, "absent chat-sessions root is a no-op");
    }

    #[tokio::test]
    async fn gc_removes_orphan_but_spares_live_dir_in_same_bucket() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        // A live chat conversation — its scratch path is recorded in the DB via
        // the real create path (`add_chat_folder`), the exact string the GC
        // compares against ...
        let live =
            create_chat_conversation_core(&db.conn, data_dir.path(), AgentType::Codex, None, None)
                .await
                .expect("create live");
        // ... alongside an unbound orphan dir in the same `chat-sessions` tree
        // (same day → same date bucket).
        let orphan = create_chat_dir_core(data_dir.path()).expect("orphan dir");
        assert_ne!(live.folder.path, orphan);

        let removed = gc_orphan_chat_dirs_core_with_threshold(
            &db.conn,
            data_dir.path(),
            std::time::Duration::ZERO,
        )
        .await
        .expect("gc");

        // The predicate discriminates by exact stored path: only the orphan goes.
        assert_eq!(removed, 1, "only the orphan is reclaimed");
        assert!(
            std::path::Path::new(&live.folder.path).is_dir(),
            "the live chat dir is spared even with an orphan beside it"
        );
        assert!(
            !std::path::Path::new(&orphan).exists(),
            "the orphan is removed"
        );
    }

    // A live dir must survive even when this GC run's data_dir is a different
    // *spelling* (here a symlink) of the storage that created it — full-path
    // matching would misclassify it as an orphan and delete it (data loss). The
    // layout-invariant `(<date>, <uuid>)` keying is what prevents that.
    #[cfg(unix)]
    #[tokio::test]
    async fn gc_spares_live_dir_under_aliased_data_dir() {
        use std::os::unix::fs::symlink;
        let db = fresh_in_memory_db().await;
        let real = tempfile::tempdir().expect("tempdir");
        // DB records the live path under the REAL data_dir spelling.
        let live =
            create_chat_conversation_core(&db.conn, real.path(), AgentType::Codex, None, None)
                .await
                .expect("create live");
        // A second spelling of the same storage: a symlink pointing at it.
        let link_parent = tempfile::tempdir().expect("link parent");
        let link = link_parent.path().join("data-link");
        symlink(real.path(), &link).expect("symlink");

        // GC runs under the symlinked spelling; the live dir must still be spared.
        let removed = gc_orphan_chat_dirs_core_with_threshold(
            &db.conn,
            &link,
            std::time::Duration::ZERO,
        )
        .await
        .expect("gc");

        assert_eq!(
            removed, 0,
            "live dir spared despite an aliased data_dir spelling"
        );
        assert!(
            std::path::Path::new(&live.folder.path).is_dir(),
            "live chat dir retained under data_dir aliasing"
        );
    }

    #[tokio::test]
    async fn cleanup_chat_folder_keeps_folder_with_remaining_conversations() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let res =
            create_chat_conversation_core(&db.conn, data_dir.path(), AgentType::Codex, None, None)
                .await
                .expect("create");
        // Simulate a second conversation that happens to share the hidden folder.
        let second =
            conversation_service::create(&db.conn, res.folder_id, AgentType::Codex, None, None)
                .await
                .expect("second conversation");

        // Deleting the first must NOT retire the folder — the second remains.
        delete_conversation_core(&db.conn, res.conversation_id)
            .await
            .expect("delete first");
        cleanup_chat_folder_for_deleted_conversation(&db.conn, res.folder_id).await;
        assert!(
            folder_service::get_folder_by_id(&db.conn, res.folder_id)
                .await
                .unwrap()
                .is_some(),
            "folder retained while a sibling conversation remains"
        );

        // Deleting the last one retires the now-empty folder.
        delete_conversation_core(&db.conn, second.id)
            .await
            .expect("delete second");
        cleanup_chat_folder_for_deleted_conversation(&db.conn, res.folder_id).await;
        assert!(
            folder_service::get_folder_by_id(&db.conn, res.folder_id)
                .await
                .unwrap()
                .is_none(),
            "folder retired once empty"
        );
    }

    #[tokio::test]
    async fn chat_folders_excluded_from_user_facing_lists_but_in_all_details() {
        let db = fresh_in_memory_db().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let normal_id = seed_folder(&db, "/tmp/codeg-chat-list-test").await;
        let chat_id =
            create_chat_conversation_core(&db.conn, data_dir.path(), AgentType::Codex, None, None)
                .await
                .expect("chat")
                .folder_id;

        // Folder history excludes the hidden chat folder, keeps the normal one.
        let history = folder_service::list_folders(&db.conn).await.unwrap();
        assert!(history.iter().any(|f| f.id == normal_id));
        assert!(!history.iter().any(|f| f.id == chat_id));

        // Open-folder surfaces exclude it too.
        let open_details = folder_service::list_open_folder_details(&db.conn)
            .await
            .unwrap();
        assert!(!open_details.iter().any(|f| f.id == chat_id));
        let open_entries = folder_service::list_open_folders(&db.conn).await.unwrap();
        assert!(!open_entries.iter().any(|f| f.id == chat_id));

        // But the full set keeps it (internal cwd / active-folder resolution).
        let all = folder_service::list_all_folder_details(&db.conn)
            .await
            .unwrap();
        assert!(all
            .iter()
            .any(|f| f.id == chat_id && f.kind == FolderKind::Chat));
    }

    #[tokio::test]
    async fn get_folder_conversation_core_missing_id_errors() {
        let db = fresh_in_memory_db().await;
        let err = get_folder_conversation_core(&db.conn, 999_999)
            .await
            .expect_err("missing conversation must error, not panic");
        let msg = format!("{err:?}");
        assert!(
            msg.to_lowercase().contains("not found") || msg.to_lowercase().contains("999999"),
            "expected not-found-shaped error, got: {msg}"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Phase 8 — _core wrappers around DB-only service calls. These were
    // extracted from the web handlers so HTTP and Tauri callers share one
    // implementation. Tests pin the boundary contract: empty-state shape,
    // roundtrip behavior, and how the wrappers surface error conditions.
    // ──────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_all_conversations_core_empty_db_returns_empty() {
        let db = fresh_in_memory_db().await;
        let rows = list_all_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            ListAllConversationsOptions::default(),
        )
        .await
        .expect("list");
        assert!(rows.is_empty(), "fresh db must have zero conversations");
    }

    #[tokio::test]
    async fn list_all_conversations_core_syncs_codex_index_title_before_search() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-list-codex-index").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("Makefile 文件的作用".into()),
            None,
        )
        .await
        .expect("create conversation");
        conversation_service::bind_external_id(
            &db.conn,
            row.id,
            "01a00496-1418-7273-a06f-dc4fae5cfa64",
            &[],
        )
        .await
        .expect("set external id");
        let before = conversation_service::get_by_id(&db.conn, row.id)
            .await
            .expect("get before");
        let titles = HashMap::from([(
            "01a00496-1418-7273-a06f-dc4fae5cfa64".to_string(),
            "解释 Makefile 文件作用".to_string(),
        )]);
        let (broadcaster, emitter) = sync_test_emitter();
        let mut events = broadcaster.subscribe();
        let (chat_channel_manager, title_edits) = title_sync_test_manager(&db, row.id).await;

        let rows = list_all_conversations_core_with_codex_titles(
            &db.conn,
            &emitter,
            &chat_channel_manager,
            ListAllConversationsOptions {
                agent_type: Some(AgentType::Codex),
                search: Some("解释 Makefile".into()),
                ..Default::default()
            },
            &titles,
        )
        .await
        .expect("list");

        assert_eq!(
            rows.len(),
            1,
            "the new title must satisfy this same call's search"
        );
        assert_eq!(rows[0].id, row.id);
        assert_eq!(rows[0].title.as_deref(), Some("解释 Makefile 文件作用"));
        let stored = conversation_service::get_by_id(&db.conn, row.id)
            .await
            .expect("get stored");
        assert_eq!(stored.title.as_deref(), Some("解释 Makefile 文件作用"));
        assert_eq!(stored.updated_at, before.updated_at);
        let event = events.try_recv().expect("title refresh must broadcast");
        assert_eq!(event.channel, CONVERSATION_CHANGED_EVENT);
        assert_eq!(event.payload["summary"]["id"], row.id);
        assert_eq!(event.payload["summary"]["title"], "解释 Makefile 文件作用");
        assert_eq!(
            title_edits.titles.lock().await.as_slice(),
            [format!("#{} 解释 Makefile 文件作用", row.id)],
            "the same external title refresh must propagate to a bound chat thread"
        );
    }

    #[tokio::test]
    async fn list_all_conversations_core_preserves_locked_codex_title() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-list-codex-locked").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("initial".into()),
            None,
        )
        .await
        .expect("create conversation");
        conversation_service::bind_external_id(&db.conn, row.id, "locked-session", &[])
            .await
            .expect("set external id");
        conversation_service::update_title(&db.conn, row.id, "我的手动标题".into())
            .await
            .expect("manual rename");
        let titles = HashMap::from([("locked-session".to_string(), "Codex 自动标题".to_string())]);

        let rows = list_all_conversations_core_with_codex_titles(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            ListAllConversationsOptions::default(),
            &titles,
        )
        .await
        .expect("list");

        let listed = rows
            .iter()
            .find(|item| item.id == row.id)
            .expect("listed row");
        assert_eq!(listed.title.as_deref(), Some("我的手动标题"));
        assert!(listed.title_locked);
        let stored = conversation_service::get_by_id(&db.conn, row.id)
            .await
            .expect("get stored");
        assert_eq!(stored.title.as_deref(), Some("我的手动标题"));
        assert!(stored.title_locked);
    }

    #[tokio::test]
    async fn list_all_conversations_core_keeps_title_when_codex_index_is_missing() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-list-codex-no-index").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("数据库原标题".into()),
            None,
        )
        .await
        .expect("create conversation");
        conversation_service::bind_external_id(&db.conn, row.id, "missing-index-session", &[])
            .await
            .expect("set external id");
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let titles = CodexParser::with_base_dir(temp_dir.path().join("missing-sessions"))
            .load_thread_name_index();
        assert!(
            titles.is_empty(),
            "a missing index must produce no title updates"
        );

        let rows = list_all_conversations_core_with_codex_titles(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            ListAllConversationsOptions::default(),
            &titles,
        )
        .await
        .expect("list");

        let listed = rows
            .iter()
            .find(|item| item.id == row.id)
            .expect("listed row");
        assert_eq!(listed.title.as_deref(), Some("数据库原标题"));
        let stored = conversation_service::get_by_id(&db.conn, row.id)
            .await
            .expect("get stored");
        assert_eq!(stored.title.as_deref(), Some("数据库原标题"));
    }

    #[tokio::test]
    async fn list_all_conversations_core_returns_persisted_rows_when_title_sync_fails() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-list-codex-sync-failure").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("persisted title".into()),
            None,
        )
        .await
        .expect("create conversation");
        conversation_service::bind_external_id(&db.conn, row.id, "failing-session", &[])
            .await
            .expect("set external id");
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    r#"CREATE TRIGGER fail_codex_title_sync
                       BEFORE UPDATE OF title ON conversation
                       WHEN OLD.id = {}
                       BEGIN
                         SELECT RAISE(FAIL, 'injected title sync failure');
                       END"#,
                    row.id
                ),
            ))
            .await
            .expect("install title failure trigger");
        let titles =
            HashMap::from([("failing-session".to_string(), "new Codex title".to_string())]);

        let rows = list_all_conversations_core_with_codex_titles(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            ListAllConversationsOptions::default(),
            &titles,
        )
        .await
        .expect("list must degrade to persisted rows");

        let listed = rows
            .iter()
            .find(|item| item.id == row.id)
            .expect("persisted row remains visible");
        assert_eq!(listed.title.as_deref(), Some("persisted title"));
    }

    #[tokio::test]
    async fn scan_importable_sessions_syncs_title_and_notifies_clients() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-scan-codex-title-sync").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("first prompt".into()),
            None,
        )
        .await
        .expect("create imported conversation");
        conversation_service::bind_external_id(&db.conn, row.id, "scan-session", &[])
            .await
            .expect("set external id");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut events = broadcaster.subscribe();
        let (chat_channel_manager, title_edits) = title_sync_test_manager(&db, row.id).await;
        let mut summary = scan_summary(
            "scan-session",
            AgentType::Codex,
            Some("/tmp/codeg-scan-codex-title-sync"),
            at(0),
        );
        summary.1.title = Some("Codex index title".into());

        let result = scan_importable_sessions_from_summaries(
            &db.conn,
            &emitter,
            &chat_channel_manager,
            vec![summary],
        )
        .await
        .expect("scan summaries");

        assert_eq!(result.total_sessions, 1);
        assert_eq!(result.importable_count, 0);
        assert_eq!(
            result.folders[0].sessions[0].status,
            ScanSessionStatus::Imported
        );
        let stored = conversation_service::get_by_id(&db.conn, row.id)
            .await
            .expect("get refreshed conversation");
        assert_eq!(stored.title.as_deref(), Some("Codex index title"));
        let event = events
            .try_recv()
            .expect("scan title refresh must broadcast");
        assert_eq!(event.channel, CONVERSATION_CHANGED_EVENT);
        assert_eq!(event.payload["kind"], "upsert");
        assert_eq!(event.payload["summary"]["id"], row.id);
        assert_eq!(event.payload["summary"]["title"], "Codex index title");
        assert_eq!(
            title_edits.wait_for_edits(1).await.as_slice(),
            [format!("#{} Codex index title", row.id)],
            "scan-discovered titles must propagate to a bound chat thread"
        );
    }

    /// The channel half of a title notification must not be on the caller's
    /// critical path: `edit_thread_title` reaches Telegram with a 60s per-call
    /// timeout, and `list_all_conversations` is the sidebar's primary read.
    #[tokio::test]
    async fn notify_conversation_title_updates_detaches_channel_sync_from_the_caller() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-notify-detached").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("detached title".into()),
            None,
        )
        .await
        .expect("create conversation");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut events = broadcaster.subscribe();
        let (chat_channel_manager, title_edits) =
            title_sync_test_manager_with(&db, row.id, TitleEditRecorder::blocked()).await;

        let handle = notify_conversation_title_updates(
            &db.conn,
            &emitter,
            &chat_channel_manager,
            vec![row.id],
        )
        .await;

        // Returned while the backend is still parked: the sidebar upsert is
        // already out, the outbound edit has not even been attempted.
        let event = events.try_recv().expect("upsert must be emitted inline");
        assert_eq!(event.payload["summary"]["title"], "detached title");
        assert!(
            title_edits.recorded().await.is_empty(),
            "caller must not wait on the chat backend"
        );

        title_edits.unblock(1);
        handle.await.expect("detached title sync task");
        assert_eq!(
            title_edits.recorded().await.as_slice(),
            [format!("#{} detached title", row.id)],
            "the detached task still propagates the title"
        );
    }

    /// A detached edit can land after a rename that happened while it was in
    /// flight. The last value the provider (and the binding's `display_title`)
    /// ends up with must be the conversation's CURRENT title, not the one the
    /// stalled sync started with — nothing retries afterwards.
    #[tokio::test]
    async fn detached_title_sync_converges_on_a_rename_that_lands_mid_flight() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-notify-late-rename").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("auto title".into()),
            None,
        )
        .await
        .expect("create conversation");
        let (_broadcaster, emitter) = sync_test_emitter();
        let (chat_channel_manager, title_edits) =
            title_sync_test_manager_with(&db, row.id, TitleEditRecorder::blocked()).await;

        let handle = notify_conversation_title_updates(
            &db.conn,
            &emitter,
            &chat_channel_manager,
            vec![row.id],
        )
        .await;

        // The auto-title edit is parked mid-flight; the user renames underneath it.
        conversation_service::update_title(&db.conn, row.id, "manual rename".into())
            .await
            .expect("manual rename");

        title_edits.unblock(8);
        handle.await.expect("detached title sync task");

        let recorded = title_edits.recorded().await;
        assert_eq!(
            recorded.last().map(String::as_str),
            Some(format!("#{} manual rename", row.id).as_str()),
            "the provider must end on the newest title, not the stalled one: {recorded:?}"
        );
        let bindings =
            crate::db::service::thread_binding_service::list_by_conversation(&db.conn, row.id)
                .await
                .expect("list bindings");
        assert_eq!(
            bindings[0].display_title.as_deref(),
            Some(format!("#{} manual rename", row.id).as_str()),
            "the persisted display title must match what the provider was last told"
        );
    }

    /// The convergence loop must not be defeated by a RUN of renames that each
    /// land mid-flight. Any fixed retry cap exits stale on a long enough run —
    /// this drives more consecutive mid-flight renames than any such cap.
    #[tokio::test]
    async fn detached_title_sync_converges_after_a_run_of_mid_flight_renames() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-notify-rename-run").await;
        let row = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("title 1".into()),
            None,
        )
        .await
        .expect("create conversation");
        let (_broadcaster, emitter) = sync_test_emitter();
        // Each provider edit is overtaken by the next rename before it returns.
        let (chat_channel_manager, title_edits) = title_sync_test_manager_with(
            &db,
            row.id,
            TitleEditRecorder::renaming_mid_edit(
                &db,
                row.id,
                &["title 2", "title 3", "title 4", "title 5", "title 6"],
            ),
        )
        .await;

        notify_conversation_title_updates(
            &db.conn,
            &emitter,
            &chat_channel_manager,
            vec![row.id],
        )
        .await
        .await
        .expect("detached title sync task");

        let recorded = title_edits.recorded().await;
        let current = conversation_service::get_by_id(&db.conn, row.id)
            .await
            .expect("read conversation")
            .title
            .expect("conversation has a title");
        // The invariant, stated against whatever the row actually ended on: the
        // last thing the provider was told IS the conversation's current title.
        assert_eq!(
            recorded.last().map(String::as_str),
            Some(format!("#{} {current}", row.id).as_str()),
            "provider must end on the row's current title however long the run: {recorded:?}"
        );
        assert_eq!(
            current, "title 6",
            "fixture must exhaust every queued rename, or it is not testing a long run"
        );
        let bindings =
            crate::db::service::thread_binding_service::list_by_conversation(&db.conn, row.id)
                .await
                .expect("list bindings");
        assert_eq!(
            bindings[0].display_title.as_deref(),
            Some(format!("#{} title 6", row.id).as_str())
        );
    }

    #[tokio::test]
    async fn list_opened_tabs_core_empty_db_returns_empty() {
        let db = fresh_in_memory_db().await;
        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert!(snap.items.is_empty());
        assert_eq!(snap.version, 0, "fresh db starts at version 0");
    }

    fn conv_tab(folder_id: i32, conversation_id: i32, agent_type: AgentType) -> OpenedTab {
        OpenedTab {
            id: 0,
            folder_id,
            conversation_id: Some(conversation_id),
            agent_type,
            position: 0,
            is_active: false,
            is_pinned: true,
        }
    }

    #[tokio::test]
    async fn save_opened_tabs_core_persists_only_conversation_tabs_and_bumps_version() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-tabs-test").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        let c2 = create_conversation_core(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("c2");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();

        let items = vec![
            conv_tab(folder_id, c1, AgentType::ClaudeCode),
            conv_tab(folder_id, c2, AgentType::Codex),
            // A draft (conversation_id == None) — must NOT persist.
            OpenedTab {
                id: 0,
                folder_id,
                conversation_id: None,
                agent_type: AgentType::Gemini,
                position: 2,
                is_active: true,
                is_pinned: true,
            },
        ];
        let outcome = save_opened_tabs_core(&db.conn, &emitter, items, 0, "win-a".into())
            .await
            .expect("save");
        assert!(outcome.accepted);
        assert_eq!(outcome.version, 1);
        assert_eq!(outcome.tabs.len(), 2, "draft tab must be stripped");

        let evt = rx.try_recv().expect("accepted save should broadcast");
        assert_eq!(evt.channel, TABS_CHANGED_EVENT);
        assert_eq!(evt.payload["version"], 1);
        assert_eq!(evt.payload["origin"], "win-a");
        assert_eq!(evt.payload["tabs"].as_array().unwrap().len(), 2);

        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert_eq!(snap.items.len(), 2);
        assert_eq!(snap.version, 1);
    }

    #[tokio::test]
    async fn save_opened_tabs_core_rejects_stale_version_without_emitting() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-tabs-stale").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");

        // First save at v0 → v1.
        let first = save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![conv_tab(folder_id, c1, AgentType::ClaudeCode)],
            0,
            "a".into(),
        )
        .await
        .expect("first save");
        assert!(first.accepted);
        assert_eq!(first.version, 1);

        // Second save built from the now-stale v0 must be rejected, no emit.
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        let stale = save_opened_tabs_core(
            &db.conn,
            &emitter,
            vec![], // would have cleared all tabs — must NOT take effect
            0,
            "b".into(),
        )
        .await
        .expect("stale save returns Ok with accepted=false");
        assert!(!stale.accepted);
        assert_eq!(stale.version, 1, "rejected save reports current version");
        assert!(
            rx.try_recv().is_err(),
            "a stale (rejected) save must not broadcast"
        );

        // The original tab survived — the stale empty save did not clobber it.
        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.version, 1);
    }

    #[tokio::test]
    async fn cleanup_tabs_for_deleted_conversation_removes_tab_and_emits() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-tab-conv-del").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![conv_tab(folder_id, c1, AgentType::ClaudeCode)],
            0,
            "a".into(),
        )
        .await
        .expect("save");

        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        delete_conversation_core(&db.conn, c1).await.expect("delete");
        cleanup_tabs_for_deleted_conversation(&emitter, &db.conn, c1).await;

        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert!(
            snap.items.is_empty(),
            "tab for a soft-deleted conversation must be removed (no ghost tab)"
        );
        let evt = rx.try_recv().expect("cleanup should broadcast");
        assert_eq!(evt.channel, TABS_CHANGED_EVENT);
        assert_eq!(evt.payload["origin"], "server");
        assert_eq!(evt.payload["tabs"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn cleanup_tabs_for_deleted_conversation_bumps_barrier_without_emitting_when_no_open_tab() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-tab-conv-del-noop").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        let before = list_opened_tabs_core(&db.conn).await.expect("list").version;
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        cleanup_tabs_for_deleted_conversation(&emitter, &db.conn, c1).await;
        assert!(
            rx.try_recv().is_err(),
            "no persisted tab → no broadcast (in-flight savers reconcile via rejected CAS)"
        );
        let after = list_opened_tabs_core(&db.conn).await.expect("list").version;
        assert_eq!(
            after,
            before + 1,
            "deletion still advances the version as a barrier against stale saves"
        );
    }

    #[tokio::test]
    async fn remove_folder_from_workspace_cleans_tabs_and_emits() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-folder-remove-tabs").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![conv_tab(folder_id, c1, AgentType::ClaudeCode)],
            0,
            "a".into(),
        )
        .await
        .expect("save");

        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        crate::commands::folders::remove_folder_from_workspace_core(&emitter, &db, folder_id)
            .await
            .expect("remove folder");

        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert!(snap.items.is_empty(), "folder removal must drop its tabs");
        let evt = rx
            .try_recv()
            .expect("folder removal should broadcast a tab change");
        assert_eq!(evt.channel, TABS_CHANGED_EVENT);
        assert_eq!(evt.payload["origin"], "server");
    }

    #[tokio::test]
    async fn stale_save_after_conversation_cleanup_is_rejected_no_resurrection() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-tab-cleanup-race").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        let c2 = create_conversation_core(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("c2");

        // Both tabs open at v0 → v1.
        let saved = save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![
                conv_tab(folder_id, c1, AgentType::ClaudeCode),
                conv_tab(folder_id, c2, AgentType::Codex),
            ],
            0,
            "a".into(),
        )
        .await
        .expect("save");
        assert_eq!(saved.version, 1);

        // Server deletes c1 and atomically cleans its tab → v2 (only c2 remains).
        delete_conversation_core(&db.conn, c1).await.expect("delete c1");
        cleanup_tabs_for_deleted_conversation(&EventEmitter::Noop, &db.conn, c1).await;

        // A client still on the pre-cleanup version re-saves the OLD set (with c1
        // present). The version bump must reject it — and c1 must NOT resurrect.
        let stale = save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![
                conv_tab(folder_id, c1, AgentType::ClaudeCode),
                conv_tab(folder_id, c2, AgentType::Codex),
            ],
            1,
            "b".into(),
        )
        .await
        .expect("stale save returns Ok");
        assert!(
            !stale.accepted,
            "a save built on the pre-cleanup version must be rejected"
        );
        assert_eq!(stale.version, 2);

        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert_eq!(snap.items.len(), 1, "c1 must not be resurrected");
        assert_eq!(snap.items[0].conversation_id, Some(c2));
        assert_eq!(snap.version, 2);
    }

    #[tokio::test]
    async fn stale_save_after_folder_removal_is_rejected_no_resurrection() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-folder-remove-race").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        let saved = save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![conv_tab(folder_id, c1, AgentType::ClaudeCode)],
            0,
            "a".into(),
        )
        .await
        .expect("save");
        assert_eq!(saved.version, 1);

        // Removing the folder atomically drops its tabs + bumps to v2.
        crate::commands::folders::remove_folder_from_workspace_core(
            &EventEmitter::Noop,
            &db,
            folder_id,
        )
        .await
        .expect("remove folder");

        // A stale re-add of the folder's tab (still on v1) must be rejected.
        let stale = save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![conv_tab(folder_id, c1, AgentType::ClaudeCode)],
            1,
            "b".into(),
        )
        .await
        .expect("stale save returns Ok");
        assert!(!stale.accepted, "save on the pre-removal version must be rejected");

        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert!(
            snap.items.is_empty(),
            "folder removal's version bump must block the stale re-add"
        );
    }

    #[tokio::test]
    async fn stale_save_referencing_deleted_conversation_is_rejected_no_ghost() {
        // The zero-row cleanup race: client A opened c1 but its save is still
        // debouncing (no persisted c1 tab yet). c1 is deleted — cleanup removes
        // zero rows but still advances the version barrier. A's in-flight save
        // (built on the pre-deletion version, still listing c1) is then rejected,
        // so a tab for the soft-deleted conversation is never persisted.
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-tab-zero-row-race").await;
        let c1 = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        let c2 = create_conversation_core(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("c2");

        // Only c2 is persisted as a tab (v0 → v1); c1 is open on A but unsaved.
        let saved = save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![conv_tab(folder_id, c2, AgentType::Codex)],
            0,
            "init".into(),
        )
        .await
        .expect("save");
        assert_eq!(saved.version, 1);

        // c1 deleted with no persisted c1 tab → zero rows removed, but the
        // version barrier still advances (v1 → v2) and nothing is broadcast.
        delete_conversation_core(&db.conn, c1).await.expect("delete c1");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        cleanup_tabs_for_deleted_conversation(&emitter, &db.conn, c1).await;
        assert!(rx.try_recv().is_err(), "zero-row cleanup must not broadcast");

        // A's debounced save (built on v1, still including the now-deleted c1) is
        // rejected by the barrier — c1 must not be persisted as a ghost.
        let stale = save_opened_tabs_core(
            &db.conn,
            &EventEmitter::Noop,
            vec![
                conv_tab(folder_id, c1, AgentType::ClaudeCode),
                conv_tab(folder_id, c2, AgentType::Codex),
            ],
            1,
            "a".into(),
        )
        .await
        .expect("stale save returns Ok");
        assert!(
            !stale.accepted,
            "a save built before the deletion barrier must be rejected"
        );
        assert_eq!(stale.version, 2);

        let snap = list_opened_tabs_core(&db.conn).await.expect("list");
        assert_eq!(snap.items.len(), 1, "no ghost tab for the deleted c1");
        assert_eq!(snap.items[0].conversation_id, Some(c2));
    }

    #[tokio::test]
    async fn import_local_conversations_core_missing_folder_errors() {
        // Takes IMPORT_GUARD internally — must not overlap a test holding it,
        // or the guard error masks the not-found error asserted below.
        let _serialized = IMPORT_GUARD_SERIALIZER.lock().await;
        let db = fresh_in_memory_db().await;
        let err = import_local_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            999_999,
        )
            .await
            .expect_err("missing folder must surface as error");
        let msg = format!("{err:?}");
        assert!(
            msg.to_lowercase().contains("not found") || msg.to_lowercase().contains("999999"),
            "expected not-found-shaped error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn update_conversation_status_core_invalid_string_errors() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-status-test").await;
        let conv_id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let err =
            update_conversation_status_core(&db.conn, conv_id, "not-a-real-status".to_string())
                .await
                .expect_err("garbage status must error before touching the DB");
        let msg = format!("{err:?}");
        assert!(
            msg.to_lowercase().contains("invalid"),
            "expected invalid-input error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn update_conversation_title_core_roundtrip() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-title-test").await;
        let conv_id = create_conversation_core(&db.conn, folder_id, AgentType::Gemini, None, None)
            .await
            .expect("create");
        update_conversation_title_core(&db.conn, conv_id, "Renamed".into())
            .await
            .expect("update");
        let summary = conversation_service::get_by_id(&db.conn, conv_id)
            .await
            .expect("read back");
        assert_eq!(summary.title.as_deref(), Some("Renamed"));
    }

    #[tokio::test]
    async fn delete_conversation_core_soft_deletes() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-delete-test").await;
        let conv_id = create_conversation_core(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("create");
        delete_conversation_core(&db.conn, conv_id)
            .await
            .expect("delete");
        // After soft delete the row should no longer show up in list_all.
        let remaining = list_all_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            ListAllConversationsOptions::default(),
        )
        .await
        .expect("list");
        assert!(
            remaining.iter().all(|c| c.id != conv_id),
            "soft-deleted conversation must not appear in list_all"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Phase 7 — delegation list filter + child lookup wrappers.
    // ──────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_child_conversations_core_returns_empty_for_no_parent() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-list-children-empty").await;
        let parent_id = create_conversation_core(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("create parent");
        let rows = list_child_conversations_core(&db.conn, parent_id)
            .await
            .expect("list");
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn list_child_conversations_core_returns_only_matching_children() {
        use crate::acp::delegation::spawner::DelegationLink;
        use crate::db::service::conversation_service;

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-list-children-match").await;
        let parent_id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("create parent");

        // Two delegation children — both should come back, newest-first.
        let mut child_ids = Vec::new();
        for (i, tool_use) in ["tu-A", "tu-B"].iter().enumerate() {
            let link = DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: (*tool_use).into(),
                delegation_call_id: format!("call-{i}"),
            member_role_id: None,
            };
            let child = conversation_service::create_with_delegation(
                &db.conn,
                folder_id,
                AgentType::Codex,
                Some(format!("child-{i}")),
                None,
                Some(link),
            )
            .await
            .expect("create child");
            child_ids.push(child.id);
        }
        // Sibling root conversation that must NOT appear.
        let _other = create_conversation_core(&db.conn, folder_id, AgentType::Gemini, None, None)
            .await
            .expect("unrelated root");

        let rows = list_child_conversations_core(&db.conn, parent_id)
            .await
            .expect("list");
        assert_eq!(rows.len(), 2, "expected 2 children, got {}", rows.len());
        assert!(rows.iter().all(|r| r.parent_id == Some(parent_id)));
        // Newest-first (created_at DESC): the later-created child leads, matching
        // the sidebar's newest-on-top sub-session ordering.
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![child_ids[1], child_ids[0]],
            "children must be newest-first"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Phase 1 — cross-client list/status sync. The wrapper-layer emit helpers
    // broadcast `conversation://changed` so every client's sidebar stays in
    // sync regardless of which transport made the change. Drive the helpers
    // directly against a test broadcaster and assert the emitted JSON.
    // ──────────────────────────────────────────────────────────────────────

    fn sync_test_emitter() -> (
        std::sync::Arc<crate::web::event_bridge::WebEventBroadcaster>,
        EventEmitter,
    ) {
        let broadcaster = std::sync::Arc::new(crate::web::event_bridge::WebEventBroadcaster::new());
        let emitter = EventEmitter::test_web_only(broadcaster.clone());
        (broadcaster, emitter)
    }

    /// Applies one queued rename per provider edit, so the rename provably
    /// lands while that edit is still in flight — the exact interleaving a
    /// detached sync has to survive.
    #[derive(Clone)]
    struct RenameDuringEdit {
        conn: sea_orm::DatabaseConnection,
        conversation_id: i32,
        pending: std::sync::Arc<tokio::sync::Mutex<std::collections::VecDeque<String>>>,
    }

    #[derive(Clone)]
    struct TitleEditRecorder {
        titles: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
        /// Stands in for Telegram's latency. Open by default; `blocked()` parks
        /// every `edit_thread_title` until the test hands out permits, which is
        /// how the detached-channel-sync tests prove a caller did not wait.
        gate: std::sync::Arc<tokio::sync::Semaphore>,
        rename_during_edit: Option<RenameDuringEdit>,
    }

    impl Default for TitleEditRecorder {
        fn default() -> Self {
            Self {
                titles: Default::default(),
                gate: std::sync::Arc::new(tokio::sync::Semaphore::new(
                    tokio::sync::Semaphore::MAX_PERMITS,
                )),
                rename_during_edit: None,
            }
        }
    }

    impl TitleEditRecorder {
        fn blocked() -> Self {
            Self {
                gate: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
                ..Default::default()
            }
        }

        /// Open gate, but every edit is overtaken by the next queued rename.
        fn renaming_mid_edit(
            db: &crate::db::AppDatabase,
            conversation_id: i32,
            renames: &[&str],
        ) -> Self {
            Self {
                rename_during_edit: Some(RenameDuringEdit {
                    conn: db.conn.clone(),
                    conversation_id,
                    pending: std::sync::Arc::new(tokio::sync::Mutex::new(
                        renames.iter().map(|t| (*t).to_string()).collect(),
                    )),
                }),
                ..Default::default()
            }
        }

        fn unblock(&self, edits: usize) {
            self.gate.add_permits(edits);
        }

        async fn recorded(&self) -> Vec<String> {
            self.titles.lock().await.clone()
        }

        /// Await a detached channel sync. Bounded so a wiring regression fails
        /// the test instead of hanging it.
        async fn wait_for_edits(&self, count: usize) -> Vec<String> {
            for _ in 0..500 {
                let titles = self.recorded().await;
                if titles.len() >= count {
                    return titles;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("timed out waiting for {count} chat-thread title edit(s)");
        }
    }

    struct RecordingTitleBackend {
        recorder: TitleEditRecorder,
    }

    #[async_trait::async_trait]
    impl crate::chat_channel::traits::ChatChannelBackend for RecordingTitleBackend {
        fn channel_type(&self) -> crate::chat_channel::types::ChannelType {
            crate::chat_channel::types::ChannelType::Telegram
        }

        async fn start(
            &self,
            _command_tx: tokio::sync::mpsc::Sender<crate::chat_channel::types::IncomingCommand>,
        ) -> Result<(), crate::chat_channel::error::ChatChannelError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), crate::chat_channel::error::ChatChannelError> {
            Ok(())
        }

        async fn status(&self) -> crate::chat_channel::types::ChannelConnectionStatus {
            crate::chat_channel::types::ChannelConnectionStatus::Connected
        }

        async fn send_message(
            &self,
            _text: &str,
        ) -> Result<
            crate::chat_channel::types::SentMessageId,
            crate::chat_channel::error::ChatChannelError,
        > {
            Ok(crate::chat_channel::types::SentMessageId("sent".into()))
        }

        async fn send_rich_message(
            &self,
            _message: &crate::chat_channel::types::RichMessage,
        ) -> Result<
            crate::chat_channel::types::SentMessageId,
            crate::chat_channel::error::ChatChannelError,
        > {
            Ok(crate::chat_channel::types::SentMessageId("sent".into()))
        }

        async fn edit_thread_title(
            &self,
            _target: &crate::chat_channel::types::ChannelMessageTarget,
            title: &str,
        ) -> Result<(), crate::chat_channel::error::ChatChannelError> {
            self.recorder
                .gate
                .acquire()
                .await
                .expect("title edit gate closed")
                .forget();
            self.recorder.titles.lock().await.push(title.to_string());
            if let Some(hook) = &self.recorder.rename_during_edit {
                let next = hook.pending.lock().await.pop_front();
                if let Some(next) = next {
                    conversation_service::update_title(&hook.conn, hook.conversation_id, next)
                        .await
                        .expect("rename during in-flight edit");
                }
            }
            Ok(())
        }

        async fn test_connection(
            &self,
        ) -> Result<(), crate::chat_channel::error::ChatChannelError> {
            Ok(())
        }
    }

    async fn title_sync_test_manager(
        db: &crate::db::AppDatabase,
        conversation_id: i32,
    ) -> (
        crate::chat_channel::manager::ChatChannelManager,
        TitleEditRecorder,
    ) {
        title_sync_test_manager_with(db, conversation_id, TitleEditRecorder::default()).await
    }

    async fn title_sync_test_manager_with(
        db: &crate::db::AppDatabase,
        conversation_id: i32,
        recorder: TitleEditRecorder,
    ) -> (
        crate::chat_channel::manager::ChatChannelManager,
        TitleEditRecorder,
    ) {
        let channel = crate::db::service::chat_channel_service::create(
            &db.conn,
            "title sync test".into(),
            "telegram".into(),
            "{}".into(),
            true,
            false,
            None,
        )
        .await
        .expect("create chat channel");
        let manager = crate::chat_channel::manager::ChatChannelManager::new();
        manager
            .add_channel(
                channel.id,
                channel.name,
                crate::chat_channel::types::ChannelType::Telegram,
                Box::new(RecordingTitleBackend {
                    recorder: recorder.clone(),
                }),
            )
            .await
            .expect("connect recording channel");
        let target = crate::chat_channel::types::ChannelMessageTarget::telegram_forum_topic(
            channel.id, "chat-1", "topic-1",
        );
        crate::db::service::thread_binding_service::upsert_for_target(
            &db.conn,
            &target,
            "telegram",
            conversation_id,
            None,
            "test-user",
            Some("old topic title".into()),
        )
        .await
        .expect("bind conversation thread");
        (manager, recorder)
    }

    #[tokio::test]
    async fn emit_conversation_upsert_broadcasts_full_root_summary() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-sync-upsert").await;
        let id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        emit_conversation_upsert(&emitter, &db.conn, id).await;
        let evt = rx.try_recv().expect("upsert should broadcast");
        let p = &*evt.payload;
        assert_eq!(evt.channel, CONVERSATION_CHANGED_EVENT);
        assert_eq!(p["kind"], "upsert");
        assert_eq!(p["summary"]["id"], id);
        // Root conversation → parent_id omitted (serde skip_serializing_if), so
        // the frontend keeps it in the sidebar.
        assert!(
            p["summary"].get("parent_id").is_none(),
            "root summary must omit parent_id"
        );
    }

    #[tokio::test]
    async fn emit_conversation_deleted_broadcasts_id_only() {
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        emit_conversation_deleted(&emitter, 4242);
        let evt = rx.try_recv().expect("deleted should broadcast");
        let p = &*evt.payload;
        assert_eq!(evt.channel, CONVERSATION_CHANGED_EVENT);
        assert_eq!(p["kind"], "deleted");
        assert_eq!(p["id"], 4242);
    }

    #[tokio::test]
    async fn emit_conversation_upsert_carries_new_status_after_update() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-sync-status").await;
        let id = create_conversation_core(&db.conn, folder_id, AgentType::Codex, None, None)
            .await
            .expect("create");
        update_conversation_status_core(&db.conn, id, "pending_review".to_string())
            .await
            .expect("status update");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        emit_conversation_upsert(&emitter, &db.conn, id).await;
        let evt = rx.try_recv().expect("upsert should broadcast");
        assert_eq!(evt.payload["summary"]["status"], "pending_review");
    }

    #[tokio::test]
    async fn emit_conversation_upsert_on_soft_deleted_row_is_silent() {
        // Anti-resurrection: get_by_id filters deleted_at, so an upsert that
        // races a delete emits nothing instead of re-inserting a tombstone.
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-sync-deleted-silent").await;
        let id = create_conversation_core(&db.conn, folder_id, AgentType::Gemini, None, None)
            .await
            .expect("create");
        delete_conversation_core(&db.conn, id)
            .await
            .expect("delete");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        emit_conversation_upsert(&emitter, &db.conn, id).await;
        assert!(
            rx.try_recv().is_err(),
            "upsert for a soft-deleted row must not broadcast (no resurrection)"
        );
    }

    #[tokio::test]
    async fn emit_conversation_upsert_broadcasts_delegation_child_with_parent() {
        // Delegation children now broadcast too: a dedicated frontend subscriber
        // routes them into their parent's expanded sub-session subtree by
        // `parent_id`. The payload must therefore carry `parent_id` (the routing
        // key) and a fresh `child_count` (so a grandchild bumps the nested
        // chevron), unlike a root whose `parent_id` is omitted.
        use crate::acp::delegation::spawner::DelegationLink;
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-sync-child-broadcast").await;
        let parent_id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("parent");
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            member_role_id: None,
            }),
        )
        .await
        .expect("child");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        emit_conversation_upsert(&emitter, &db.conn, child.id).await;
        let evt = rx
            .try_recv()
            .expect("delegation child should broadcast an upsert");
        let p = &*evt.payload;
        assert_eq!(p["kind"], "upsert");
        assert_eq!(p["summary"]["id"], child.id);
        assert_eq!(
            p["summary"]["parent_id"], parent_id,
            "child summary must carry parent_id so the frontend can route it"
        );
        assert_eq!(
            p["summary"]["child_count"], 0,
            "leaf child carries child_count 0"
        );
    }

    #[tokio::test]
    async fn delete_child_re_emits_parent_for_child_count_convergence() {
        // Deleting a delegation child must re-broadcast its parent so every
        // client's child_count (and chevron) converges from the DB aggregate —
        // symmetric with the create-time parent re-emit.
        use crate::acp::delegation::spawner::DelegationLink;
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/codeg-delete-child-reemit").await;
        let parent_id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("parent");
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            member_role_id: None,
            }),
        )
        .await
        .expect("child");
        let (broadcaster, emitter) = sync_test_emitter();
        let mut rx = broadcaster.subscribe();
        delete_conversation_with_cleanup_core(&emitter, &db.conn, child.id)
            .await
            .expect("delete child");
        let mut saw_deleted = false;
        let mut saw_parent_upsert = false;
        while let Ok(evt) = rx.try_recv() {
            if evt.channel != CONVERSATION_CHANGED_EVENT {
                continue;
            }
            let p = &*evt.payload;
            if p["kind"] == "deleted" && p["id"] == child.id {
                saw_deleted = true;
            }
            if p["kind"] == "upsert" && p["summary"]["id"] == parent_id {
                saw_parent_upsert = true;
                assert_eq!(
                    p["summary"]["child_count"], 0,
                    "parent count drops to 0 once its only child is gone"
                );
            }
        }
        assert!(saw_deleted, "child deletion must broadcast a Deleted");
        assert!(
            saw_parent_upsert,
            "parent must re-broadcast an Upsert for child_count convergence"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Import-picker scan reconciliation (`build_scan_result`) and batch
    // import (`import_selected_from_summaries`).
    // ──────────────────────────────────────────────────────────────────────

    fn scan_summary(
        id: &str,
        agent: AgentType,
        cwd: Option<&str>,
        ts: chrono::DateTime<chrono::Utc>,
    ) -> (AgentType, ConversationSummary) {
        (
            agent,
            ConversationSummary {
                id: id.into(),
                agent_type: agent,
                folder_path: cwd.map(String::from),
                folder_name: cwd.map(folder_name_from_path),
                title: Some(format!("title-{id}")),
                started_at: ts,
                ended_at: None,
                message_count: 1,
                model: None,
                git_branch: None,
                parent_id: None,
                parent_tool_use_id: None,
                delegation_call_id: None,
            },
        )
    }

    fn key_of(agent: AgentType, id: &str) -> SelectedSessionKey {
        SelectedSessionKey {
            agent_type: agent,
            external_id: id.into(),
        }
    }

    #[test]
    fn scan_groups_normalized_path_variants_into_one_folder() {
        // A trailing-slash cwd variant must land in the same group as the bare
        // path — otherwise the picker shows one folder twice and an import
        // could mint a near-duplicate folder row.
        let summaries = vec![
            scan_summary("s1", AgentType::ClaudeCode, Some("/tmp/proj"), at(0)),
            scan_summary("s2", AgentType::Codex, Some("/tmp/proj/"), at(10)),
        ];
        let result = build_scan_result(summaries, &HashMap::new(), &[]);

        assert_eq!(result.folders.len(), 1);
        let folder = &result.folders[0];
        assert_eq!(folder.path, "/tmp/proj");
        assert!(!folder.exists_in_codeg);
        assert_eq!(folder.folder_id, None);
        assert_eq!(
            folder.agent_types,
            vec![AgentType::ClaudeCode, AgentType::Codex]
        );
        // Sessions sort newest-first inside the group.
        assert_eq!(folder.sessions[0].external_id, "s2");
        assert_eq!(result.total_sessions, 2);
        assert_eq!(result.importable_count, 2);
    }

    #[test]
    fn scan_marks_status_new_imported_deleted() {
        let summaries = vec![
            scan_summary("new", AgentType::ClaudeCode, Some("/tmp/p"), at(0)),
            scan_summary("live", AgentType::ClaudeCode, Some("/tmp/p"), at(1)),
            scan_summary("gone", AgentType::ClaudeCode, Some("/tmp/p"), at(2)),
        ];
        let mut imported_index = HashMap::new();
        imported_index.insert(("claude_code".to_string(), "live".to_string()), true);
        imported_index.insert(("claude_code".to_string(), "gone".to_string()), false);

        let result = build_scan_result(summaries, &imported_index, &[]);
        let by_id: HashMap<&str, ScanSessionStatus> = result.folders[0]
            .sessions
            .iter()
            .map(|s| (s.external_id.as_str(), s.status))
            .collect();

        assert_eq!(by_id["new"], ScanSessionStatus::New);
        assert_eq!(by_id["live"], ScanSessionStatus::Imported);
        assert_eq!(by_id["gone"], ScanSessionStatus::Deleted);
        assert_eq!(result.total_sessions, 3);
        assert_eq!(result.importable_count, 1, "only New counts as importable");
    }

    #[test]
    fn scan_counts_sessions_without_folder_path_instead_of_listing_them() {
        let summaries = vec![
            scan_summary("has", AgentType::Codex, Some("/tmp/p"), at(0)),
            scan_summary("none", AgentType::Codex, None, at(1)),
            scan_summary("blank", AgentType::Codex, Some("   "), at(2)),
        ];
        let result = build_scan_result(summaries, &HashMap::new(), &[]);

        assert_eq!(result.folders.len(), 1);
        assert_eq!(result.no_folder_count, 2);
        assert_eq!(result.total_sessions, 1);
    }

    #[test]
    fn scan_prefers_stored_row_path_and_live_row_over_deleted_variant() {
        // The DB stores raw strings (UNIQUE on the exact bytes), so a live and
        // a soft-deleted variant of the same normalized path can coexist. The
        // scan must surface the LIVE row's exact path, or a later add_folder
        // would resurrect the deleted variant instead.
        let rows = vec![
            ScanFolderRow {
                id: 1,
                path: "/tmp/proj/".into(),
                name: "proj-deleted".into(),
                deleted: true,
                parent_id: None,
            },
            ScanFolderRow {
                id: 2,
                path: "/tmp/proj".into(),
                name: "proj".into(),
                deleted: false,
                parent_id: None,
            },
        ];
        let summaries = vec![scan_summary(
            "s1",
            AgentType::ClaudeCode,
            Some("/tmp/proj///"),
            at(0),
        )];
        let result = build_scan_result(summaries, &HashMap::new(), &rows);

        let folder = &result.folders[0];
        assert_eq!(folder.path, "/tmp/proj", "live row's stored path wins");
        assert_eq!(folder.name, "proj");
        assert!(folder.exists_in_codeg);
        assert_eq!(folder.folder_id, Some(2));
    }

    #[test]
    fn scan_soft_deleted_folder_reports_not_exists_but_keeps_id() {
        let rows = vec![ScanFolderRow {
            id: 9,
            path: "/tmp/gone".into(),
            name: "gone".into(),
            deleted: true,
            parent_id: None,
        }];
        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some("/tmp/gone"),
            at(0),
        )];
        let result = build_scan_result(summaries, &HashMap::new(), &rows);

        let folder = &result.folders[0];
        assert!(
            !folder.exists_in_codeg,
            "a soft-deleted row is not a live folder — import will reopen it"
        );
        assert_eq!(folder.folder_id, Some(9));
    }

    #[test]
    fn scan_sorts_folders_by_importable_count_then_path() {
        let mut imported_index = HashMap::new();
        imported_index.insert(("codex".to_string(), "b1".to_string()), true);
        let summaries = vec![
            scan_summary("b1", AgentType::Codex, Some("/tmp/b"), at(0)),
            scan_summary("a1", AgentType::Codex, Some("/tmp/a"), at(1)),
            scan_summary("a2", AgentType::Codex, Some("/tmp/a"), at(2)),
        ];
        let result = build_scan_result(summaries, &imported_index, &[]);

        let paths: Vec<&str> = result.folders.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["/tmp/a", "/tmp/b"], "2 importable before 0");
    }

    #[tokio::test]
    async fn batch_import_creates_missing_folder_and_imports() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;

        let summaries = vec![
            scan_summary("s1", AgentType::ClaudeCode, Some("/tmp/proj-a"), at(0)),
            scan_summary("s2", AgentType::Codex, Some("/tmp/proj-a"), at(1)),
        ];
        let result = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![
                key_of(AgentType::ClaudeCode, "s1"),
                key_of(AgentType::Codex, "s2"),
            ],
        )
        .await
        .expect("batch import");

        assert_eq!(result.imported, 2);
        assert_eq!(result.created_folders, 1);
        assert_eq!(result.failed, 0);
        assert_eq!(result.folders.len(), 1);
        assert!(result.folders[0].created);

        let folder_rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(folder_rows.len(), 1);
        assert_eq!(folder_rows[0].path, "/tmp/proj-a");
        assert!(folder_rows[0].is_open, "created folder must open in sidebar");

        let convs = conversation::Entity::find().all(&db.conn).await.unwrap();
        assert_eq!(convs.len(), 2);
        assert!(convs.iter().all(|c| c.folder_id == folder_rows[0].id));
    }

    // A session whose cwd is a linked git worktree of an open repo belongs
    // UNDER that repo. `parent_id` is exactly the sidebar's test for "worktree
    // of" versus "one more top-level folder", and the branch-label backfill
    // selects on it too, so leaving it NULL strands the folder twice. #552.
    #[tokio::test]
    async fn batch_import_nests_a_worktree_cwd_under_its_repo() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);
        let repo_id = seed_folder(&db, &repo.to_string_lossy()).await;

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some(&worktree.to_string_lossy()),
            at(0),
        )];
        let result = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        assert_eq!(result.imported, 1);
        let rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        let worktree_row = rows
            .iter()
            .find(|r| r.id != repo_id)
            .expect("worktree folder row");
        assert_eq!(
            worktree_row.parent_id,
            Some(repo_id),
            "imported worktree must group under the repo it belongs to"
        );
    }

    // Selecting a repo's sessions and one of its worktrees' in the SAME import
    // is the first-time-import shape, and the repo folder it needs exists by the
    // time the worktree is written — as long as the two are not imported in
    // whatever order their paths happen to sort in.
    #[tokio::test]
    async fn batch_import_nests_a_worktree_under_a_repo_created_in_the_same_run() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);

        let summaries = vec![
            scan_summary("s0", AgentType::Codex, Some(&repo.to_string_lossy()), at(0)),
            scan_summary(
                "s1",
                AgentType::Codex,
                Some(&worktree.to_string_lossy()),
                at(1),
            ),
        ];
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![
                key_of(AgentType::Codex, "s0"),
                key_of(AgentType::Codex, "s1"),
            ],
        )
        .await
        .expect("batch import");

        let rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        let repo_row = rows
            .iter()
            .find(|r| path_eq_for_matching(&r.path, &repo.to_string_lossy()))
            .expect("repo folder row");
        let worktree_row = rows
            .iter()
            .find(|r| path_eq_for_matching(&r.path, &worktree.to_string_lossy()))
            .expect("worktree folder row");
        assert_eq!(repo_row.parent_id, None);
        assert_eq!(
            worktree_row.parent_id,
            Some(repo_row.id),
            "a repo created earlier in the same run is still the worktree's repo"
        );
    }

    // The repo folder a worktree resolves to can itself be recorded as somebody
    // else's worktree child (`open_worktree_folder_core` writes that whenever the
    // branch switcher adopts an unregistered checkout). Hanging off it directly
    // would build a two-level chain, and the sidebar's merge is single-level: the
    // grandchild's conversations would bucket under a folder that is itself
    // merged away, and render nowhere. Flatten, exactly as the worktree open does.
    #[tokio::test]
    async fn batch_import_flattens_onto_the_repos_own_root() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);
        let outer_id = seed_folder(&db, "/tmp/outer-root").await;
        let repo_id = folder_service::add_folder_with_parent(
            &db.conn,
            &repo.to_string_lossy(),
            Some(outer_id),
        )
        .await
        .expect("seed repo folder")
        .id;

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some(&worktree.to_string_lossy()),
            at(0),
        )];
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        let rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        let worktree_row = rows
            .iter()
            .find(|r| r.id != outer_id && r.id != repo_id)
            .expect("worktree folder row");
        assert_eq!(worktree_row.parent_id, Some(outer_id));
    }

    // Before imported worktrees were grouped, one could exist as a top-level
    // folder while its main working tree was still unregistered. Navigating
    // from that worktree to a branch checked out in the main tree then recorded
    // the main-tree row under the worktree. Flattening through that row points
    // straight back at the import target's own id; refuse it or the sidebar
    // filters the self-parented folder out completely.
    #[tokio::test]
    async fn batch_import_refuses_a_parent_that_flattens_to_the_target() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);
        let worktree_id = seed_folder(&db, &worktree.to_string_lossy()).await;
        let repo_id = folder_service::add_folder_with_parent(
            &db.conn,
            &repo.to_string_lossy(),
            Some(worktree_id),
        )
        .await
        .expect("seed inverted main-tree folder")
        .id;

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some(&worktree.to_string_lossy()),
            at(0),
        )];
        let result = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        assert_eq!(result.imported, 1);
        let worktree_row = crate::db::entities::folder::Entity::find_by_id(worktree_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(worktree_row.parent_id, None);
        let repo_row = crate::db::entities::folder::Entity::find_by_id(repo_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repo_row.parent_id, Some(worktree_id));
    }

    // Historical top-level worktrees can also leave a longer inverted chain.
    // If the main-tree row points at a folder whose own parent is the import
    // target, using that one-hop "root" would create a two-node cycle. Only an
    // actual live top-level row is safe to write as a new parent.
    #[tokio::test]
    async fn batch_import_refuses_a_flattened_parent_that_is_itself_a_child() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);
        let worktree_id = seed_folder(&db, &worktree.to_string_lossy()).await;
        let middle_id = folder_service::add_folder_with_parent(
            &db.conn,
            &root.join("middle").to_string_lossy(),
            Some(worktree_id),
        )
        .await
        .expect("seed middle folder")
        .id;
        let repo_id = folder_service::add_folder_with_parent(
            &db.conn,
            &repo.to_string_lossy(),
            Some(middle_id),
        )
        .await
        .expect("seed chained main-tree folder")
        .id;

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some(&worktree.to_string_lossy()),
            at(0),
        )];
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        let worktree_row = crate::db::entities::folder::Entity::find_by_id(worktree_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(worktree_row.parent_id, None);
        let middle_row = crate::db::entities::folder::Entity::find_by_id(middle_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(middle_row.parent_id, Some(worktree_id));
        let repo_row = crate::db::entities::folder::Entity::find_by_id(repo_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repo_row.parent_id, Some(middle_id));
    }

    // A worktree imported before grouping existed may already be acting as the
    // root for worktrees created from it. Moving that row under the real repo
    // without moving its children would create a two-level chain and hide the
    // children's conversations in the single-level sidebar merge.
    #[tokio::test]
    async fn batch_import_does_not_reparent_a_folder_that_already_has_children() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);
        let repo_id = seed_folder(&db, &repo.to_string_lossy()).await;
        let worktree_id = seed_folder(&db, &worktree.to_string_lossy()).await;
        let child_id = folder_service::add_folder_with_parent(
            &db.conn,
            &root.join("child").to_string_lossy(),
            Some(worktree_id),
        )
        .await
        .expect("seed existing child")
        .id;

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some(&worktree.to_string_lossy()),
            at(0),
        )];
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        let worktree_row = crate::db::entities::folder::Entity::find_by_id(worktree_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(worktree_row.parent_id, None);
        let child_row = crate::db::entities::folder::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child_row.parent_id, Some(worktree_id));
        let repo_row = crate::db::entities::folder::Entity::find_by_id(repo_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repo_row.parent_id, None);
    }

    // Grouping under a repo codeg has never opened would invent a workspace row
    // the user did not ask for, so an unknown repo leaves the folder top-level.
    #[tokio::test]
    async fn batch_import_leaves_a_worktree_top_level_when_its_repo_is_unopened() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (_repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some(&worktree.to_string_lossy()),
            at(0),
        )];
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        let rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].parent_id, None);
    }

    // A soft-deleted repo renders nowhere, so a child of it would render
    // nowhere either. Top-level is the better of the two.
    #[tokio::test]
    async fn batch_import_leaves_a_worktree_top_level_when_its_repo_is_deleted() {
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
        let db = fresh_in_memory_db().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let (repo, worktree) = crate::git_repo::fixture_linked_worktree(&root);
        let repo_id = seed_folder(&db, &repo.to_string_lossy()).await;

        let row = crate::db::entities::folder::Entity::find_by_id(repo_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = row.into_active_model();
        active.deleted_at = Set(Some(chrono::Utc::now()));
        active.is_open = Set(false);
        active.update(&db.conn).await.unwrap();

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some(&worktree.to_string_lossy()),
            at(0),
        )];
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        let rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        let worktree_row = rows
            .iter()
            .find(|r| r.id != repo_id)
            .expect("worktree folder row");
        assert_eq!(worktree_row.parent_id, None);
    }

    // The fallback stays on `add_folder`'s Preserve semantics, so re-importing
    // into a folder a worktree open already parented cannot demote it, not even
    // when the directory is gone from disk and resolves to nothing.
    #[tokio::test]
    async fn batch_import_preserves_a_recorded_worktree_parent() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let repo_id = seed_folder(&db, "/tmp/proj-repo").await;
        folder_service::add_folder_with_parent(&db.conn, "/tmp/proj-wt", Some(repo_id))
            .await
            .expect("seed worktree folder");

        let summaries = vec![scan_summary(
            "s1",
            AgentType::Codex,
            Some("/tmp/proj-wt"),
            at(0),
        )];
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::Codex, "s1")],
        )
        .await
        .expect("batch import");

        let rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        let worktree_row = rows
            .iter()
            .find(|r| r.path == "/tmp/proj-wt")
            .expect("worktree folder row");
        assert_eq!(worktree_row.parent_id, Some(repo_id));
    }

    #[tokio::test]
    async fn batch_import_reuses_stored_path_for_trailing_slash_variant() {
        use sea_orm::EntityTrait;
        let db = fresh_in_memory_db().await;
        let seeded_id = seed_folder(&db, "/tmp/proj-b").await;

        let summaries = vec![scan_summary(
            "s1",
            AgentType::ClaudeCode,
            Some("/tmp/proj-b/"),
            at(0),
        )];
        let result = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::ClaudeCode, "s1")],
        )
        .await
        .expect("batch import");

        assert_eq!(result.imported, 1);
        assert_eq!(result.created_folders, 0);
        assert!(!result.folders[0].created);
        assert_eq!(result.folders[0].folder_id, seeded_id);

        let folder_rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(
            folder_rows.len(),
            1,
            "the trailing-slash cwd must NOT mint a near-duplicate folder row"
        );
    }

    #[tokio::test]
    async fn batch_import_reopens_soft_deleted_folder_without_duplicate() {
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/proj-c").await;

        let row = crate::db::entities::folder::Entity::find_by_id(folder_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = row.into_active_model();
        active.deleted_at = Set(Some(chrono::Utc::now()));
        active.is_open = Set(false);
        active.update(&db.conn).await.unwrap();

        let summaries = vec![scan_summary(
            "s1",
            AgentType::ClaudeCode,
            Some("/tmp/proj-c"),
            at(0),
        )];
        let result = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            summaries,
            vec![key_of(AgentType::ClaudeCode, "s1")],
        )
        .await
        .expect("batch import");

        assert_eq!(result.imported, 1);
        assert_eq!(
            result.created_folders, 1,
            "reopening a soft-deleted row counts as creating a folder"
        );

        let folder_rows = crate::db::entities::folder::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(folder_rows.len(), 1, "reopened in place, not duplicated");
        assert!(folder_rows[0].deleted_at.is_none());
        assert!(folder_rows[0].is_open);
    }

    #[tokio::test]
    async fn batch_import_skips_already_imported_and_counts_missing_keys() {
        let db = fresh_in_memory_db().await;

        let make = || {
            vec![scan_summary(
                "s1",
                AgentType::ClaudeCode,
                Some("/tmp/proj-d"),
                at(0),
            )]
        };
        let first = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            make(),
            vec![key_of(AgentType::ClaudeCode, "s1")],
        )
        .await
        .unwrap();
        assert_eq!(first.imported, 1);

        let second = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            make(),
            vec![
                key_of(AgentType::ClaudeCode, "s1"),
                key_of(AgentType::Codex, "does-not-exist"),
            ],
        )
        .await
        .unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped, 1, "re-import of an existing row skips");
        assert_eq!(second.not_found, 1, "unresolvable key counts as not_found");
    }

    #[tokio::test]
    async fn batch_import_restores_a_deleted_conversation_in_place() {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
            Set};
        let db = fresh_in_memory_db().await;

        let make = || {
            vec![scan_summary(
                "s1",
                AgentType::ClaudeCode,
                Some("/tmp/proj-e"),
                at(0),
            )]
        };
        import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            make(),
            vec![key_of(AgentType::ClaudeCode, "s1")],
        )
        .await
        .unwrap();

        let row = conversation::Entity::find()
            .filter(conversation::Column::ExternalId.eq("s1"))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let original_id = row.id;
        let mut active = row.into_active_model();
        active.deleted_at = Set(Some(chrono::Utc::now()));
        active.update(&db.conn).await.unwrap();

        // Deleting a conversation is a soft delete, so re-picking the session
        // in the import window brings the ORIGINAL row back rather than
        // inserting a second one — every selection here is a session the user
        // checked while it was badged "deleted".
        let again = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            make(),
            vec![key_of(AgentType::ClaudeCode, "s1")],
        )
        .await
        .unwrap();
        assert_eq!(again.imported, 0, "restored, not re-imported");
        assert_eq!(again.restored, 1);
        assert_eq!(again.skipped, 0);
        assert_eq!(again.folders[0].restored, 1);

        let rows = conversation::Entity::find().all(&db.conn).await.unwrap();
        assert_eq!(rows.len(), 1, "no duplicate row");
        assert_eq!(rows[0].id, original_id);
        assert!(rows[0].deleted_at.is_none(), "back in the sidebar");
    }

    #[tokio::test]
    async fn whole_folder_import_still_never_resurrects_a_deleted_conversation() {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
            Set};
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/proj-sweep").await;
        let items = vec![scan_summary(
            "s1",
            AgentType::ClaudeCode,
            Some("/tmp/proj-sweep"),
            at(0),
        )];

        import_service::import_summaries(
            &db.conn,
            folder_id,
            &items,
            import_service::DeletedPolicy::Skip,
        )
        .await
        .unwrap();
        let row = conversation::Entity::find()
            .filter(conversation::Column::ExternalId.eq("s1"))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = row.into_active_model();
        active.deleted_at = Set(Some(chrono::Utc::now()));
        active.update(&db.conn).await.unwrap();

        // The legacy sweep imports a whole FOLDER, not sessions the user picked
        // one by one, so it must not bring back everything they ever deleted
        // under it.
        let (tally, _ids) = import_service::import_summaries(
            &db.conn,
            folder_id,
            &items,
            import_service::DeletedPolicy::Skip,
        )
        .await
        .unwrap();
        assert_eq!(tally.restored, 0);
        assert_eq!(tally.skipped, 1);
        let rows = conversation::Entity::find().all(&db.conn).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].deleted_at.is_some(), "stays deleted");
    }

    #[tokio::test]
    async fn batch_import_selection_of_delegation_child_counts_not_found() {
        let db = fresh_in_memory_db().await;

        let (agent, mut child) =
            scan_summary("child", AgentType::Hermes, Some("/tmp/proj-f"), at(0));
        child.parent_id = Some("root".into());

        let result = import_selected_from_summaries(
            &db.conn,
            &EventEmitter::Noop,
            vec![(agent, child)],
            vec![key_of(AgentType::Hermes, "child")],
        )
        .await
        .unwrap();

        assert_eq!(result.imported, 0);
        assert_eq!(
            result.not_found, 1,
            "a delegation child must never import as a root row"
        );
    }

    #[tokio::test]
    async fn batch_import_emits_folder_upserts_and_one_bulk_event() {
        use crate::web::event_bridge::{
            WebEventBroadcaster, CONVERSATIONS_BULK_CHANGED_EVENT, FOLDER_CHANGED_EVENT,
        };
        use std::sync::Arc;

        let db = fresh_in_memory_db().await;
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut rx = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster.clone());

        let summaries = vec![
            scan_summary("s1", AgentType::ClaudeCode, Some("/tmp/proj-g"), at(0)),
            scan_summary("s2", AgentType::Codex, Some("/tmp/proj-h"), at(1)),
        ];
        let result = import_selected_from_summaries(
            &db.conn,
            &emitter,
            summaries,
            vec![
                key_of(AgentType::ClaudeCode, "s1"),
                key_of(AgentType::Codex, "s2"),
            ],
        )
        .await
        .unwrap();
        assert_eq!(result.imported, 2);

        let mut folder_events = 0;
        let mut bulk_events = 0;
        while let Ok(evt) = rx.try_recv() {
            match evt.channel.as_str() {
                FOLDER_CHANGED_EVENT => folder_events += 1,
                CONVERSATIONS_BULK_CHANGED_EVENT => {
                    bulk_events += 1;
                    let p = &*evt.payload;
                    assert_eq!(p["imported"], 2);
                    assert_eq!(p["folder_ids"].as_array().unwrap().len(), 2);
                }
                _ => {}
            }
        }
        assert_eq!(folder_events, 2, "one folder upsert per touched folder");
        assert_eq!(bulk_events, 1, "exactly one bulk nudge, never per-row spam");
    }

    #[tokio::test]
    async fn import_selected_sessions_core_rejects_concurrent_and_empty() {
        let _serialized = IMPORT_GUARD_SERIALIZER.lock().await;
        let db = fresh_in_memory_db().await;

        assert!(
            import_selected_sessions_core(&db.conn, &EventEmitter::Noop, vec![])
                .await
                .is_err(),
            "empty selection is invalid input"
        );

        let _held = IMPORT_GUARD.try_lock().expect("guard free in test");
        assert!(
            import_selected_sessions_core(
                &db.conn,
                &EventEmitter::Noop,
                vec![key_of(AgentType::ClaudeCode, "x")],
            )
            .await
            .is_err(),
            "a second import racing the guard must be rejected"
        );
    }

    #[tokio::test]
    async fn legacy_import_shares_the_guard_with_batch_import() {
        // The retained legacy command must NOT bypass IMPORT_GUARD — otherwise a
        // legacy import racing a batch import could double-insert on a DB with no
        // unique index. With the guard held it is rejected BEFORE the folder
        // lookup, so even a valid folder id surfaces the guard error, not a hit.
        let _serialized = IMPORT_GUARD_SERIALIZER.lock().await;
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/legacy-guard").await;

        let _held = IMPORT_GUARD.try_lock().expect("guard free in test");
        let err = import_local_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            folder_id,
        )
            .await
            .expect_err("legacy import must be rejected while an import is in progress");
        let msg = format!("{err:?}").to_lowercase();
        assert!(
            msg.contains("already in progress"),
            "expected the guard error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn import_summaries_counts_row_failures_without_aborting() {
        // A row whose insert fails (here: a non-existent folder_id → FK violation
        // with PRAGMA foreign_keys=ON) is logged and counted as `failed`, never
        // aborting the batch or stranding the good rows — so a mid-group DB error
        // can't lose the committed tally or the folder broadcast.
        let db = fresh_in_memory_db().await;
        let items = vec![
            scan_summary("s1", AgentType::ClaudeCode, Some("/tmp/x"), at(0)),
            scan_summary("s2", AgentType::Codex, Some("/tmp/x"), at(1)),
        ];

        let (tally, updated_ids, failed) =
            import_service::import_summaries_resilient(
                &db.conn,
                999_999,
                &items,
                import_service::DeletedPolicy::Skip,
            )
            .await;
        assert_eq!(failed, 2, "both rows fail the folder FK and are counted");
        assert_eq!(tally.imported, 0);
        assert_eq!(tally.updated, 0);
        assert!(updated_ids.is_empty());

        // Same items into a real folder import cleanly — the resilient loop did
        // not corrupt state or leave a half-open transaction.
        let folder_id = seed_folder(&db, "/tmp/x").await;
        let (tally2, _ids, failed2) =
            import_service::import_summaries_resilient(
                &db.conn,
                folder_id,
                &items,
                import_service::DeletedPolicy::Skip,
            )
            .await;
        assert_eq!(failed2, 0);
        assert_eq!(tally2.imported, 2);
    }

    #[tokio::test]
    async fn legacy_strict_import_summaries_propagates_row_failure() {
        // The legacy per-folder importer keeps its strict contract: a DB error
        // propagates as Err rather than being swallowed into a 0/0/0 tally, so
        // its back-compat command still surfaces failures. (The batch path uses
        // the resilient variant instead.)
        let db = fresh_in_memory_db().await;
        let items = vec![scan_summary(
            "s1",
            AgentType::ClaudeCode,
            Some("/tmp/x"),
            at(0),
        )];
        assert!(
            import_service::import_summaries(
                &db.conn,
                999_999,
                &items,
                import_service::DeletedPolicy::Skip
            )
            .await
            .is_err(),
            "a row FK violation must propagate through the strict importer"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Turn windowing: request resolution + response slicing.
    // ──────────────────────────────────────────────────────────────────────

    fn windowless_detail(turns: Vec<MessageTurn>) -> DbConversationDetail {
        DbConversationDetail {
            summary: DbConversationSummary {
                id: 1,
                folder_id: 1,
                title: None,
                title_locked: false,
                agent_type: AgentType::ClaudeCode,
                status: "completed".into(),
                kind: crate::db::entities::conversation::ConversationKind::Regular,
                model: None,
                git_branch: None,
                external_id: None,
                message_count: turns.len() as u32,
                child_count: 0,
                created_at: at(-100),
                updated_at: at(0),
                pinned_at: None,
                parent_id: None,
                parent_tool_use_id: None,
                delegation_call_id: None,
                origin_cwd: None,
                team: None,
                tag_ids: Vec::new(),
            },
            turns,
            session_stats: None,
            transcript_watermark: Some(123),
            in_flight_user_turn_id: None,
            turns_offset: None,
            turns_total: None,
            assistant_turns_before_offset: None,
            prefix_hash: None,
            uncovered_prefix_max_ts: None,
        }
    }

    fn four_turns() -> Vec<MessageTurn> {
        vec![
            user_text_turn("turn-0", "q1", at(-40)),
            assistant_text_turn("turn-1", "a1", at(-39), true),
            user_text_turn("turn-2", "q2", at(-20)),
            assistant_text_turn("turn-3", "a2", at(-19), true),
        ]
    }

    #[test]
    fn resolve_turn_window_req_rejects_both_selectors() {
        assert!(resolve_turn_window_req(Some(10), Some(3)).is_err());
        assert!(matches!(resolve_turn_window_req(None, None), Ok(None)));
        assert!(resolve_turn_window_req(Some(10), None).unwrap().is_some());
        assert!(resolve_turn_window_req(None, Some(0)).unwrap().is_some());
    }

    #[test]
    fn apply_turn_window_tail_slices_and_stamps_meta() {
        let full = four_turns();
        let mut detail = windowless_detail(full.clone());
        apply_turn_window(
            &mut detail,
            crate::commands::turn_window::TurnWindowReq::Tail(1),
        );
        // Tail(1) lands on turn-3 (assistant) and round-aligns back to the
        // user turn at index 2.
        assert_eq!(detail.turns_offset, Some(2));
        assert_eq!(detail.turns_total, Some(4));
        assert_eq!(detail.assistant_turns_before_offset, Some(1));
        assert_eq!(
            detail.turns.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["turn-2", "turn-3"]
        );
        // The windowed turns are the same objects the full response carries.
        assert_eq!(detail.turns[0].timestamp, full[2].timestamp);
        // Full-transcript fields keep describing the full transcript.
        assert_eq!(detail.summary.message_count, 4);
        assert_eq!(detail.transcript_watermark, Some(123));
        assert_eq!(
            detail.uncovered_prefix_max_ts,
            Some(full[1].timestamp),
            "max ts over the uncovered prefix [0..2)"
        );
        assert_eq!(
            detail.prefix_hash.as_deref(),
            Some(crate::commands::turn_window::prefix_fingerprint(&full[..2]).as_str())
        );
    }

    #[test]
    fn apply_turn_window_from_index_is_exact_and_full_coverage_is_marked() {
        let mut detail = windowless_detail(four_turns());
        apply_turn_window(
            &mut detail,
            crate::commands::turn_window::TurnWindowReq::FromIndex(3),
        );
        // Index 3 is an assistant turn — fromIndex must NOT round-align.
        assert_eq!(detail.turns_offset, Some(3));
        assert_eq!(detail.turns.len(), 1);

        let mut full = windowless_detail(four_turns());
        apply_turn_window(
            &mut full,
            crate::commands::turn_window::TurnWindowReq::FromIndex(0),
        );
        assert_eq!(full.turns_offset, Some(0));
        assert_eq!(full.turns.len(), 4);
        assert_eq!(full.uncovered_prefix_max_ts, None);
        assert!(full.prefix_hash.is_some(), "offset 0 still stamps the seed");
    }

    #[test]
    fn apply_turn_window_from_index_past_total_yields_empty_window() {
        let mut detail = windowless_detail(four_turns());
        apply_turn_window(
            &mut detail,
            crate::commands::turn_window::TurnWindowReq::FromIndex(99),
        );
        assert_eq!(detail.turns_offset, Some(4));
        assert_eq!(detail.turns_total, Some(4));
        assert!(detail.turns.is_empty());
    }

    #[tokio::test]
    async fn turns_page_core_slices_with_seam_proof() {
        // End-to-end through the DB-backed core: a conversation without an
        // external_id parses to zero turns, so drive the page math through the
        // pure helpers on a synthetic list instead, then verify the empty-DB
        // path returns a well-formed empty page.
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/page").await;
        let conv_id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("create conversation");
        let page = get_folder_conversation_turns_core(&db.conn, conv_id, 10, 5)
            .await
            .expect("page fetch");
        assert_eq!(page.turns_total, 0);
        assert_eq!(page.turns_offset, 0);
        assert!(page.turns.is_empty());
        assert_eq!(
            page.prefix_hash, page.prefix_hash_before_index,
            "empty transcript: both fingerprints are the seed"
        );

        // Seam-proof shape on a synthetic list: the page [start..before) must
        // report H(0..start) as its own fingerprint and H(0..before) as the
        // seam — the latter is what the client compares against its current
        // window fingerprint before prepending.
        let turns = four_turns();
        let (start, end) = crate::commands::turn_window::resolve_page_bounds(&turns, 2, 2);
        assert_eq!((start, end), (0, 2));
        let own = crate::commands::turn_window::window_meta(&turns, start);
        let seam = crate::commands::turn_window::window_meta(&turns, 2);
        assert_eq!(own.prefix_hash, crate::commands::turn_window::prefix_fingerprint(&[]));
        assert_eq!(
            seam.prefix_hash,
            crate::commands::turn_window::prefix_fingerprint(&turns[..2])
        );
    }
