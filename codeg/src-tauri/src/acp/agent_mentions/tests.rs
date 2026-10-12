// agent_mentions unit tests (split from acp/agent_mentions.rs).
use crate::acp::types::PromptInputBlock;

    use super::*;

    fn text(block: &PromptInputBlock) -> &str {
        match block {
            PromptInputBlock::Text { text } => text,
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn wire_shape_retains_camel_case_agent_type() {
        let value = serde_json::to_value(AgentRoute {
            agent_type: "antigravity".into(),
        })
        .unwrap();
        assert_eq!(value, serde_json::json!({"agentType": "antigravity"}));
    }

    #[test]
    fn a_prompt_with_no_agent_link_is_left_exactly_as_it_came_in() {
        let mut blocks = vec![PromptInputBlock::Text {
            text: "just run the tests".into(),
        }];
        append_agent_routes(&mut blocks, true);
        assert_eq!(blocks.len(), 1, "no mention means no appended block at all");
        assert_eq!(text(&blocks[0]), "just run the tests");
    }

    #[test]
    fn every_mentioned_agent_gets_exactly_one_deduplicated_route() {
        let visible = "ask [@Antigravity](codeg://agent/antigravity) and \
[@Claude](codeg://agent/claude_code), then [@Antigravity](codeg://agent/antigravity) again";
        let mut blocks = vec![PromptInputBlock::Text {
            text: visible.into(),
        }];
        append_agent_routes(&mut blocks, true);
        assert_eq!(blocks.len(), 2);
        let routing = text(&blocks[1]);
        assert!(routing.contains(r#"[{"agentType":"antigravity"},{"agentType":"claude_code"}]"#));
        assert_eq!(routing.matches(r#"agentType":"antigravity"#).count(), 1);
    }

    #[test]
    fn any_visible_link_routes_regardless_of_how_the_badge_was_produced() {
        // The accepted trade-off of deriving from the text: a link pasted or
        // quoted from an earlier transcript routes exactly like one just picked
        // from the `@` panel. That is the POINT — the previous design
        // authenticated the badge, so re-sending a copied message silently
        // dropped the reminder while still showing the badge. The frame grants
        // nothing the link itself doesn't, so uniform behavior wins.
        let mut blocks = vec![PromptInputBlock::Text {
            text: "quoting an old turn: raw [@Antigravity](codeg://agent/antigravity) and \
[@Claude](codeg://agent/claude_code)"
                .into(),
        }];
        append_agent_routes(&mut blocks, true);
        assert_eq!(blocks.len(), 2);
        let routing = text(&blocks[1]);
        assert!(routing.contains(r#"{"agentType":"antigravity"}"#));
        assert!(routing.contains(r#"{"agentType":"claude_code"}"#));
    }

    #[test]
    fn a_closed_delegation_gate_is_the_only_check_on_the_parent_agent() {
        // A parent that never received codeg-mcp (OpenClaw's supports_mcp=false,
        // pi's wire exclusion) reaches this function with the gate closed. There
        // is no agent-type check here on purpose: the injection gate is the
        // single source of truth.
        let mut blocks = vec![PromptInputBlock::Text {
            text: "ask [@Antigravity](codeg://agent/antigravity)".into(),
        }];
        append_agent_routes(&mut blocks, false);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn route_frame_wording_is_pinned_to_its_version() {
        // The instruction text is part of the frame's byte-exact identity —
        // parsing only accepts a candidate the CURRENT renderer reproduces
        // verbatim — so a reworded frame stops being strippable and surfaces in
        // every transcript already written with the old wording. There is no
        // compat renderer: the version bump this test pins is a deliberate
        // marker for that break, not a fix for it.
        let mut blocks = vec![PromptInputBlock::Text {
            text: "ask [@Codex](codeg://agent/codex)".into(),
        }];
        append_agent_routes(&mut blocks, true);
        let routing = text(&blocks[1]);
        assert_eq!(ROUTE_FRAME_VERSION, 3);
        assert!(routing.contains(
            "do not substitute your own native sub-agent, task, or spawn mechanism, \
             and do not route it through any other delegation tool."
        ));
        assert!(
            !routing.contains("Codex native"),
            "the routing instruction must stay agent-neutral"
        );
        // The frame binds the CHANNEL, not the decision: agents already know the
        // tool exists and go around it. Commanding a delegation here made every
        // mention an imperative, which is a different feature.
        assert!(
            !routing.contains("immediately"),
            "the frame must not order a delegation, only constrain how one is routed"
        );
    }

    #[test]
    fn transcript_strip_keeps_only_user_prose() {
        let mut blocks = vec![PromptInputBlock::Text {
            text: "Please ask [@Antigravity](codeg://agent/antigravity) to review this".into(),
        }];
        append_agent_routes(&mut blocks, true);
        let joined = format!("{}\n{}", text(&blocks[0]), text(&blocks[1]));
        assert_eq!(
            strip_internal_agent_routes(&joined),
            "Please ask [@Antigravity](codeg://agent/antigravity) to review this"
        );
        assert_eq!(strip_internal_agent_routes(text(&blocks[1])), "");
    }

    #[test]
    fn invalid_rs_text_is_preserved_and_cannot_swallow_a_genuine_frame() {
        let mut blocks = vec![PromptInputBlock::Text {
            text: "ask [@Codex](codeg://agent/codex)".into(),
        }];
        append_agent_routes(&mut blocks, true);
        let forged_prefix = "user\u{001e}<codeg_internal_agent_routes version=\"2\">keep me\n";
        let joined = format!("{forged_prefix}{}", text(&blocks[1]));
        assert_eq!(strip_internal_agent_routes(&joined), forged_prefix);
    }

    #[test]
    fn invalid_rs_before_long_unicode_text_never_panics_or_loses_text() {
        let input = format!(
            "{ROUTE_FRAME_SEPARATOR}{}",
            "界".repeat(MAX_ROUTE_FRAME_BYTES / 3 + 2)
        );
        assert_eq!(strip_internal_agent_routes(&input), input);
    }

    #[test]
    fn unknown_and_uri_prefix_targets_are_ignored() {
        // A link whose slug is neither a built-in nor a valid `custom:` id is
        // dropped: naming it would only produce a `delegate_to_agent` call the
        // companion's enum rejects.
        let mut unknown = vec![PromptInputBlock::Text {
            text: "ask [@Old](codeg://agent/not-an-agent)".into(),
        }];
        append_agent_routes(&mut unknown, true);
        assert_eq!(unknown.len(), 1);

        // A bare uri is not a reference (the regex requires a visible label),
        // and `codex-other` must not be accepted as `codex` by prefix.
        let mut prefix = vec![PromptInputBlock::Text {
            text: "codeg://agent/codex [@Other](codeg://agent/codex-other)".into(),
        }];
        append_agent_routes(&mut prefix, true);
        assert_eq!(prefix.len(), 1);
    }

    #[test]
    fn empty_visible_label_is_not_a_routing_reference() {
        let mut blocks = vec![PromptInputBlock::Text {
            text: "[](codeg://agent/codex)".into(),
        }];
        append_agent_routes(&mut blocks, true);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn the_route_cap_counts_distinct_agents_and_keeps_the_frame_parseable() {
        let mut prompt = String::from("ask ");
        for index in 0..(MAX_AGENT_ROUTES + 4) {
            prompt.push_str(&format!("[@A{index}](codeg://agent/custom:agent-{index}) "));
        }
        let mut blocks = vec![PromptInputBlock::Text { text: prompt }];
        append_agent_routes(&mut blocks, true);
        assert_eq!(blocks.len(), 2);
        let routing = text(&blocks[1]);
        assert_eq!(routing.matches(r#""agentType""#).count(), MAX_AGENT_ROUTES);
        // A truncated frame must still round-trip, or the parsers would leave it
        // in the transcript.
        assert_eq!(strip_internal_agent_routes(routing), "");
    }

    #[test]
    fn prompt_ingress_scrubs_rs_across_every_textual_block_field() {
        let mut blocks = vec![
            PromptInputBlock::Text {
                text: "user\u{001e}text".into(),
            },
            PromptInputBlock::Image {
                // Nominally base64, but nothing validates that before the block
                // is forwarded to the agent, and an API caller fills it freely.
                data: "QUJ\u{001e}D".into(),
                mime_type: "image/\u{001e}png".into(),
                uri: Some("clipboard://a\u{001e}b".into()),
            },
            PromptInputBlock::Resource {
                uri: "file:///tmp/\u{001e}a".into(),
                mime_type: Some("text/\u{001e}plain".into()),
                text: Some("user\u{001e}resource".into()),
                blob: Some("SGVsb\u{001e}G8=".into()),
            },
            PromptInputBlock::ResourceLink {
                uri: "file:///tmp/\u{001e}b".into(),
                name: "re\u{001e}port".into(),
                mime_type: Some("text/\u{001e}csv".into()),
                description: Some("row\u{001e}sep".into()),
            },
        ];
        assert!(strip_route_separator_from_prompt(&mut blocks));
        assert!(
            !prompt_has_route_separator(&blocks),
            "no user-supplied separator may survive to the wire"
        );
        assert!(matches!(
            &blocks[0],
            PromptInputBlock::Text { text } if text == "usertext"
        ));
        assert!(matches!(
            &blocks[2],
            PromptInputBlock::Resource { text: Some(text), .. } if text == "userresource"
        ));

        // Clean prompts are reported untouched so the caller can stay quiet.
        let mut clean = vec![PromptInputBlock::Text {
            text: "ordinary prompt".into(),
        }];
        assert!(!strip_route_separator_from_prompt(&mut clean));
    }

    /// Test-only inverse of [`strip_route_separator_from_prompt`]: walks the
    /// serialized form so a field the scrub forgot fails the test, instead of
    /// asserting the scrub against its own field list.
    fn prompt_has_route_separator(blocks: &[PromptInputBlock]) -> bool {
        fn any_string(value: &serde_json::Value) -> bool {
            match value {
                serde_json::Value::String(text) => text.contains(ROUTE_FRAME_SEPARATOR),
                serde_json::Value::Array(items) => items.iter().any(any_string),
                serde_json::Value::Object(map) => map.values().any(any_string),
                _ => false,
            }
        }
        any_string(&serde_json::to_value(blocks).expect("prompt blocks are serializable"))
    }

    /// Exactly what Antigravity's ACP server writes into its trajectory: the
    /// prompt's text blocks joined with a space, and each separator replaced by
    /// one. Verified against a real `conversations/<id>.db`, where a routed turn
    /// reads `…（使用codeg-mcp工具）  {"kind":…delegation tool.\n ` — note the
    /// body's own newlines came through, which is why the scan can count them.
    fn as_antigravity_persists(blocks: &[PromptInputBlock]) -> String {
        blocks
            .iter()
            .map(|block| text(block).replace(ROUTE_FRAME_SEPARATOR, " "))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_frame_stored_without_its_separators_is_still_stripped() {
        let visible = "调用[@Claude Code](codeg://agent/claude_code) 看下这个文件夹干什么的";
        let mut blocks = vec![PromptInputBlock::Text {
            text: visible.into(),
        }];
        append_agent_routes(&mut blocks, true);
        let persisted = as_antigravity_persists(&blocks);
        assert!(
            !persisted.contains(ROUTE_FRAME_SEPARATOR),
            "the fixture must have LOST its separators, or it proves nothing"
        );

        assert!(contains_internal_agent_routes(&persisted));
        // Only the body goes; the spaces that replaced the separators are the
        // caller's trim to make (`parsers::sanitize_text`).
        assert_eq!(strip_internal_agent_routes(&persisted).trim_end(), visible);
    }

    #[test]
    fn a_separator_less_frame_still_has_to_round_trip_byte_for_byte() {
        let mut blocks = vec![PromptInputBlock::Text {
            text: "ask [@Codex](codeg://agent/codex)".into(),
        }];
        append_agent_routes(&mut blocks, true);
        let body = text(&blocks[1]).replace(ROUTE_FRAME_SEPARATOR, "");
        assert_eq!(strip_internal_agent_routes(&body), "");

        // One byte off anywhere in the wording, and it is prose again.
        let reworded = body.replace("delegation tool.", "delegation tool!");
        assert_eq!(strip_internal_agent_routes(&reworded), reworded);
        // A nonce that is not a UUID is not a nonce this renderer produced.
        let forged = body.replacen("\"nonce\":\"", "\"nonce\":\"x", 1);
        assert_eq!(strip_internal_agent_routes(&forged), forged);
        // Truncated mid-body there is no third line, so nothing matches — which
        // is why the title path cuts rather than strips.
        let truncated = &body[..body.len() - 20];
        assert_eq!(strip_internal_agent_routes(truncated), truncated);
    }

    #[test]
    fn the_body_anchor_is_what_the_renderer_starts_with() {
        let mut blocks = vec![PromptInputBlock::Text {
            text: "ask [@Codex](codeg://agent/codex)".into(),
        }];
        append_agent_routes(&mut blocks, true);
        assert!(
            text(&blocks[1])
                .starts_with(&format!("{ROUTE_FRAME_SEPARATOR}{ROUTE_FRAME_BODY_PREFIX}")),
            "reordering the descriptor's fields would silently unanchor the \
             separator-less scan"
        );
    }

    // --- [crewchat] team frame (Phase 1.3) -----------------------------------

    fn member(name: &str, agent_type: &str) -> crate::acp::delegation::team::TeamMember {
        crate::acp::delegation::team::TeamMember {
            name: name.into(),
            agent_type: agent_type.into(),
            duty: None,
            role_id: String::new(),
                mcp: Vec::new(),
        }
    }

    #[test]
    fn team_frame_names_members_and_is_append_only_for_teams() {
        let mut blocks = vec![PromptInputBlock::Text {
            text: "plan the work".into(),
        }];
        append_team_frame(
            &mut blocks,
            &[
                crate::acp::delegation::team::TeamMember {
                    name: "后端开发工程师".into(),
                    agent_type: "open_code".into(),
                    duty: Some("脚本、环境与部署".into()),
                    role_id: "r-backend".into(),
                mcp: Vec::new(),
                },
                member("前端工程师", "codex"),
            ],
        );
        assert_eq!(blocks.len(), 2);
        let frame = text(&blocks[1]);
        assert!(frame.contains(
            r#"[{"name":"后端开发工程师","agentType":"open_code","duty":"脚本、环境与部署"},{"name":"前端工程师","agentType":"codex"}]"#
        ));
        assert!(frame.contains("Only the members above are delegable"));
        assert!(
            frame.starts_with(&format!("{ROUTE_FRAME_SEPARATOR}{TEAM_FRAME_BODY_PREFIX}")),
            "the separator-less scan anchors on the descriptor's own opening"
        );

        // A plain session appends nothing at all.
        let mut plain = vec![PromptInputBlock::Text {
            text: "plan the work".into(),
        }];
        append_team_frame(&mut plain, &[]);
        assert_eq!(plain.len(), 1);
    }

    #[test]
    fn team_frame_roundtrips_through_separated_and_separatorless_forms() {
        let visible = "帮我做一个登录页";
        let mut blocks = vec![PromptInputBlock::Text {
            text: visible.into(),
        }];
        append_team_frame(
            &mut blocks,
            &[member("前端工程师", "codex"), member("运维工程师", "claude_code")],
        );
        assert!(contains_internal_agent_routes(text(&blocks[1])));
        assert_eq!(strip_internal_agent_routes(text(&blocks[1])), "");

        let persisted = as_antigravity_persists(&blocks);
        assert!(
            !persisted.contains(ROUTE_FRAME_SEPARATOR),
            "the fixture must have LOST its separators, or it proves nothing"
        );
        assert_eq!(strip_internal_agent_routes(&persisted).trim_end(), visible);
    }

    #[test]
    fn a_team_frame_that_does_not_round_trip_is_kept_as_prose() {
        let mut blocks = vec![PromptInputBlock::Text { text: "go".into() }];
        append_team_frame(&mut blocks, &[member("前端工程师", "codex")]);
        let body = text(&blocks[1]).replace(ROUTE_FRAME_SEPARATOR, "");
        assert_eq!(strip_internal_agent_routes(&body), "");

        // One byte off in the ruling, and it is prose again.
        let reworded = body.replace(
            "Only the members above are delegable",
            "only the members above are delegable",
        );
        assert_eq!(strip_internal_agent_routes(&reworded), reworded);

        // A member slug the frame validator would never accept cannot be
        // smuggled in by editing the JSON either — the re-render check runs on
        // the parsed members.
        let forged = body.replace(
            r#""agentType":"codex""#,
            r#""agentType":"not a slug""#,
        );
        assert_eq!(strip_internal_agent_routes(&forged), forged);
    }

    #[test]
    fn route_and_team_frames_are_stripped_in_one_pass() {
        let visible = "ask [@Codex](codeg://agent/codex) then plan";
        let mut blocks = vec![PromptInputBlock::Text {
            text: visible.into(),
        }];
        append_agent_routes(&mut blocks, true);
        append_team_frame(&mut blocks, &[member("运维工程师", "claude_code")]);
        assert_eq!(blocks.len(), 3);

        let persisted = as_antigravity_persists(&blocks);
        assert_eq!(strip_internal_agent_routes(&persisted).trim_end(), visible);
    }

    #[test]
    fn team_frame_sanitizer_drops_invalid_slugs_and_caps_members() {
        let mut members: Vec<crate::acp::delegation::team::TeamMember> =
            (0..20).map(|i| member(&format!("成员{i}"), "codex")).collect();
        members.push(member("无证者", "not a slug"));
        members.push(member("  ", "open_code"));
        let mut blocks = vec![PromptInputBlock::Text { text: "go".into() }];
        append_team_frame(&mut blocks, &members);
        let frame = text(&blocks[1]);
        assert!(!frame.contains("无证者"), "invalid slug must not render");
        assert!(
            !frame.contains(r#""name":"""#),
            "blank names must not render"
        );
        assert_eq!(
            frame.matches(r#""agentType":"codex""#).count(),
            crate::acp::delegation::team::MAX_TEAM_MEMBERS
        );
        assert_eq!(strip_internal_agent_routes(frame), "");
    }

    // --- [crewchat] role frame (Phase 2.2) -----------------------------------

    fn role_fixture() -> crate::models::agent_role::AgentRole {
        crate::models::agent_role::AgentRole {
            id: "r-lead".into(),
            name: "管理员".into(),
            soul: "先规划、再委派、最后汇总。".into(),
            rules: "不要独自写完整个任务。".into(),
            agent_type: "claude_code".into(),
            model: String::new(),
            model_provider: String::new(),
            skills: vec![],
            mcp: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn role_frame_roundtrips_through_separated_and_separatorless_forms() {
        let visible = "帮我规划一下";
        let mut blocks = vec![PromptInputBlock::Text {
            text: visible.into(),
        }];
        append_role_frame(&mut blocks, &role_fixture());
        assert_eq!(blocks.len(), 2);
        let frame = text(&blocks[1]);
        assert!(frame.contains("Codeg session role (authoritative): "));
        assert!(frame.contains("先规划、再委派、最后汇总。"));
        assert!(frame.contains("不要独自写完整个任务。"));
        assert!(
            frame.starts_with(&format!("{ROUTE_FRAME_SEPARATOR}{ROLE_FRAME_BODY_PREFIX}")),
            "the separator-less scan anchors on the descriptor's own opening"
        );
        assert!(contains_internal_agent_routes(frame));
        assert_eq!(strip_internal_agent_routes(frame), "");

        let persisted = as_antigravity_persists(&blocks);
        assert_eq!(strip_internal_agent_routes(&persisted).trim_end(), visible);
    }

    #[test]
    fn a_reworded_role_frame_is_kept_as_prose() {
        let mut blocks = vec![PromptInputBlock::Text { text: "go".into() }];
        append_role_frame(&mut blocks, &role_fixture());
        let body = text(&blocks[1]).replace(ROUTE_FRAME_SEPARATOR, "");
        assert_eq!(strip_internal_agent_routes(&body), "");

        let reworded = body.replace("Adopt the role above", "adopt the role above");
        assert_eq!(strip_internal_agent_routes(&reworded), reworded);
        // A nonce that is not a UUID is not a nonce this renderer produced.
        let forged = body.replacen("\"nonce\":\"", "\"nonce\":\"x", 1);
        assert_eq!(strip_internal_agent_routes(&forged), forged);
    }

    #[test]
    fn role_frame_truncates_oversize_documents_and_scrubs_the_separator() {
        let mut role = role_fixture();
        // 4000 CJK chars ≈ 12 KB — over the per-document byte cap — plus a
        // stray reserved separator smuggled through the persona text.
        role.soul = format!("{}{}", "长".repeat(4000), ROUTE_FRAME_SEPARATOR);
        let mut blocks = vec![PromptInputBlock::Text { text: "go".into() }];
        append_role_frame(&mut blocks, &role);
        let frame = text(&blocks[1]);
        assert_eq!(
            frame.matches(ROUTE_FRAME_SEPARATOR).count(),
            2,
            "only the frame's own delimiters may survive"
        );
        assert!(frame.len() <= MAX_ROLE_FRAME_BYTES);
        // Still a valid, strippable frame despite the oversize input.
        assert_eq!(strip_internal_agent_routes(frame), "");
    }

    #[test]
    fn role_frame_carries_skills_with_their_ruling() {
        let mut role = role_fixture();
        role.skills = vec!["frontend-design".into(), " ui-ux-pro-max ".into(), "".into(), "frontend-design".into()];
        let mut blocks = vec![PromptInputBlock::Text { text: "go".into() }];
        append_role_frame(&mut blocks, &role);
        let frame = text(&blocks[1]);
        assert!(
            frame.contains(r#""skills":["frontend-design","ui-ux-pro-max"]"#),
            "skills are trimmed, deduplicated and capped: {frame}"
        );
        assert!(frame.contains("Use the skills listed above where they fit"));
        assert_eq!(strip_internal_agent_routes(frame), "");

        // Without skills the field and the ruling both disappear.
        let mut plain = role_fixture();
        plain.skills = Vec::new();
        let mut blocks = vec![PromptInputBlock::Text { text: "go".into() }];
        append_role_frame(&mut blocks, &plain);
        let frame = text(&blocks[1]);
        assert!(!frame.contains("\"skills\""));
        assert!(!frame.contains("Use the skills listed above"));
        assert_eq!(strip_internal_agent_routes(frame), "");
    }

    #[test]
    fn a_role_with_no_documents_appends_nothing() {
        let mut role = role_fixture();
        role.soul = "   ".into();
        role.rules = String::new();
        let mut blocks = vec![PromptInputBlock::Text { text: "go".into() }];
        append_role_frame(&mut blocks, &role);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn a_title_is_cut_at_a_role_frame_marker_too() {
        let mut separated = String::from("hi \u{001e}{\"kind\":\"codeg_internal_role\",\"versi");
        cut_at_route_frame_marker(&mut separated);
        assert_eq!(separated, "hi");

        let mut bare = String::from("hi  {\"kind\":\"codeg_internal_r");
        cut_at_route_frame_marker(&mut bare);
        assert_eq!(bare, "hi");
    }

    #[test]
    fn a_title_is_cut_at_a_team_frame_marker_too() {
        let mut separated = String::from("hi \u{001e}{\"kind\":\"codeg_internal_team\",\"versio");
        cut_at_route_frame_marker(&mut separated);
        assert_eq!(separated, "hi");

        // The cap halved the team descriptor; the fragment is still cut.
        let mut bare = String::from("hi  {\"kind\":\"codeg_internal_tea");
        cut_at_route_frame_marker(&mut bare);
        assert_eq!(bare, "hi");
    }

    #[test]
    fn a_title_is_cut_at_whichever_frame_marker_comes_first() {
        // Separator intact: the cut lands on it, as it always has.
        let mut separated = String::from("hi \u{001e}{\"kind\":\"codeg_internal_agent_routes\",\"ve");
        cut_at_route_frame_marker(&mut separated);
        assert_eq!(separated, "hi");

        // Separator rewritten away: the descriptor's own opening is the only
        // handle left.
        let mut bare = String::from("hi  {\"kind\":\"codeg_internal_agent_routes\",\"ve");
        cut_at_route_frame_marker(&mut bare);
        assert_eq!(bare, "hi");

        let mut clean = String::from("just a normal title");
        cut_at_route_frame_marker(&mut clean);
        assert_eq!(clean, "just a normal title");
    }

    #[test]
    fn a_title_whose_cap_halved_the_marker_is_cut_as_well() {
        // The separator is one char and lands inside the window or not at all;
        // the 37-byte descriptor prefix can be sliced through, and `find` is
        // blind to the fragment that survives.
        let mut halved = String::from("hi  {\"kind\":\"codeg_int...");
        cut_at_route_frame_marker(&mut halved);
        assert_eq!(halved, "hi");

        // A parser that caps without an ellipsis leaves the same fragment bare.
        let mut bare = String::from("hi  {\"kind\":\"codeg");
        cut_at_route_frame_marker(&mut bare);
        assert_eq!(bare, "hi");

        // Below the floor it is generic punctuation, not route metadata, and
        // trimming it would eat ordinary prose.
        for prose in ["what does this do: {", "see the shape {\"kind\":\""] {
            let mut title = String::from(prose);
            cut_at_route_frame_marker(&mut title);
            assert_eq!(title, prose);
        }
    }

    #[test]
    fn user_authored_envelope_text_is_left_untouched() {
        let input = "hello <codeg_internal_agent_routes version=\"1\">\nuser text\n</codeg_internal_agent_routes>";
        assert_eq!(strip_internal_agent_routes(input), input);
        assert!(!contains_internal_agent_routes(input));
    }
