    use super::*;
    use serde_json::json;

    fn valid_args() -> Value {
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

    #[test]
    fn parse_questions_happy_path_mints_ids() {
        let qs = parse_questions(&valid_args()).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].question, "Which approach?");
        assert_eq!(qs[0].header, "Approach");
        assert!(!qs[0].multi_select);
        assert_eq!(qs[0].options.len(), 2);
        assert!(!qs[0].id.is_empty());
    }

    fn elicitation_raw(props: Value, required: Value) -> Value {
        json!({
            "mode": "form",
            "sessionId": "sess-1",
            "toolCallId": "item-1",
            "requestedSchema": {
                "type": "object",
                "properties": props,
                "required": required,
            },
            "message": "Input requested",
        })
    }

    fn expect_questions(plan: ElicitationPlan) -> ElicitationQuestions {
        match plan {
            ElicitationPlan::Questions(q) => q,
            ElicitationPlan::Approval(_) => panic!("expected a questions plan"),
        }
    }

    fn expect_approval(plan: ElicitationPlan) -> ElicitationApproval {
        match plan {
            ElicitationPlan::Approval(a) => a,
            ElicitationPlan::Questions(_) => panic!("expected an approval plan"),
        }
    }

    #[test]
    fn classify_elicitation_single_select_maps_options_and_id() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Approach",
                    "description": "Which approach?",
                    "oneOf": [
                        {"const": "Incremental", "title": "Incremental"},
                        {"const": "Rewrite", "title": "Rewrite"}
                    ]
                }
            }),
            json!(["q1"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs.len(), 1);
        assert_eq!(q.specs[0].id, "q1", "property key becomes the spec id");
        assert_eq!(q.specs[0].question, "Which approach?");
        assert_eq!(q.specs[0].header, "Approach");
        assert!(!q.specs[0].multi_select);
        assert!(!q.specs[0].is_secret);
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Incremental", "Rewrite"]);
        assert_eq!(q.fields[0].kind, ElicitationFieldKind::Text);
        // The elicitation's toolCallId rides along so the connection handler can
        // key the synthesized in-stream result card to it (see the fn docs).
        assert_eq!(
            q.tool_call_id.as_deref(),
            Some("item-1"),
            "carries the elicitation toolCallId for the synthesized card"
        );
    }

    /// codex-acp ≥1.12.0's `request_user_input` shape, verbatim off
    /// `buildUserInputRequest`: the QUESTION is in `title`, the short tab label
    /// is in `description`, the companion is `<id>_note` with
    /// `_meta.codex.role`, and an `isOther` question's `oneOf` ends with the
    /// injected "None of the above".
    #[test]
    fn classify_elicitation_reads_codex_112_orientation_and_drops_its_extras() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Which approach should I take for the rewrite?",
                    "description": "Approach",
                    "_meta": {"codex": {"isOther": true, "isSecret": false}},
                    "oneOf": [
                        {"const": "Incremental", "title": "Incremental"},
                        {"const": "Rewrite", "title": "Rewrite"},
                        {
                            "const": "None of the above",
                            "title": "None of the above",
                            "description": "Provide a different answer in the note field."
                        }
                    ]
                },
                "q1_note": {
                    "type": "string",
                    "title": "Additional answer or note",
                    "_meta": {"codex": {
                        "questionId": "q1", "role": "user_note", "isSecret": false
                    }}
                }
            }),
            json!(["q1"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs.len(), 1, "the `_note` companion must not render");
        assert_eq!(q.specs[0].id, "q1");
        assert_eq!(
            q.specs[0].question, "Which approach should I take for the rewrite?",
            "1.12.0 puts the question in `title`"
        );
        assert_eq!(
            q.specs[0].header, "Approach",
            "…and the short tab label in `description`"
        );
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Incremental", "Rewrite"],
            "codex's injected 'None of the above' is dropped for the card's own Other input"
        );
    }

    /// A model that writes "None of the above" itself: codex-acp then injects
    /// nothing, so the option is REAL and must survive. Same label, no codex
    /// description.
    #[test]
    fn classify_elicitation_keeps_a_model_authored_none_of_the_above() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Which approach?",
                    "description": "Approach",
                    "_meta": {"codex": {"isOther": true, "isSecret": false}},
                    "oneOf": [
                        {"const": "Incremental", "title": "Incremental"},
                        {"const": "None of the above", "title": "None of the above"}
                    ]
                },
                "q1_note": {
                    "type": "string",
                    "title": "Additional answer or note",
                    "_meta": {"codex": {"questionId": "q1", "role": "user_note"}}
                }
            }),
            json!(["q1"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Incremental", "None of the above"]);
    }

    /// A codex-acp ≤1.11.0 form still reads the old way round. The `__other`
    /// companion + `isOtherAnswer` marker date the form, so `description` stays
    /// the question and `title` the tab label.
    #[test]
    fn classify_elicitation_keeps_codex_111_orientation() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Approach",
                    "description": "Which approach should I take for the rewrite?",
                    "_meta": {"codex": {"isOther": true, "isSecret": false}},
                    "oneOf": [
                        {"const": "Incremental", "title": "Incremental"},
                        {"const": "Rewrite", "title": "Rewrite"}
                    ]
                },
                "q1__other": {
                    "type": "string",
                    "title": "Other",
                    "description": "Type your own answer instead of choosing an option above.",
                    "_meta": {"codex": {
                        "questionId": "q1", "isOtherAnswer": true, "isSecret": false
                    }}
                }
            }),
            json!([]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs.len(), 1);
        assert_eq!(
            q.specs[0].question, "Which approach should I take for the rewrite?",
            "≤1.11.0 puts the question in `description`"
        );
        assert_eq!(q.specs[0].header, "Approach");
    }

    /// A codex form whose questions all have `isOther: false` carries NO
    /// companion, in either generation — so the form itself cannot be dated and
    /// the running adapter's version is the only real signal. Both readings are
    /// reachable, and `None` falls back to the pinned adapter's shape.
    ///
    /// This is the ordinary case, not an edge one: most `request_user_input`
    /// questions are plain selects.
    #[test]
    fn classify_elicitation_undated_codex_form_follows_the_running_adapter() {
        let props = json!({
            "q1": {
                "type": "string",
                "title": "Which approach should I take?",
                "description": "Approach",
                "_meta": {"codex": {"isOther": false, "isSecret": false}},
                "oneOf": [{"const": "Incremental", "title": "Incremental"}]
            }
        });
        let raw = elicitation_raw(props, json!(["q1"]));

        // Running ≥1.12.0 — `title` is the question.
        let q = expect_questions(
            classify_elicitation(&raw, ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInTitle))).unwrap(),
        );
        assert_eq!(q.specs[0].question, "Which approach should I take?");
        assert_eq!(q.specs[0].header, "Approach");

        // Running ≤1.11.0 — the old reading, even though no marker dates the
        // form. A custom pin is a supported configuration.
        let q = expect_questions(
            classify_elicitation(&raw, ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInDescription))).unwrap(),
        );
        assert_eq!(q.specs[0].question, "Approach");
        // …and the question text lands in the header slot, where the
        // `MAX_HEADER_CHARS` clamp shears it — which is what the wrong
        // orientation costs a user: a truncated chip and no question.
        assert_eq!(
            q.specs[0].header,
            "Which approach should I take?"
                .chars()
                .take(MAX_HEADER_CHARS)
                .collect::<String>()
        );

        // Adapter reported no usable version — fall back to the pinned shape.
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs[0].question, "Which approach should I take?");
    }

    /// The running adapter's version OUTRANKS a companion marker: it describes
    /// the process that built the frame. They agree in practice, so this pins
    /// the precedence rather than a behavior anyone should see.
    #[test]
    fn classify_elicitation_running_version_outranks_the_companion_marker() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Approach",
                    "description": "Which approach should I take?",
                    "_meta": {"codex": {"isOther": true, "isSecret": false}},
                    "oneOf": [{"const": "Incremental", "title": "Incremental"}]
                },
                "q1__other": {
                    "type": "string",
                    "title": "Other",
                    "_meta": {"codex": {"questionId": "q1", "isOtherAnswer": true}}
                }
            }),
            json!([]),
        );
        let q = expect_questions(
            classify_elicitation(&raw, ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInTitle))).unwrap(),
        );
        assert_eq!(q.specs[0].question, "Approach", "running version wins");
        // …and with no running version, the marker dates it the old way.
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs[0].question, "Which approach should I take?");
    }

    /// The 1.12.0 fixups are codex-only. DeepSeek is routed through this same
    /// parser (`build_client_capabilities` advertises `elicitation.form` to it),
    /// and for DeepSeek a value that is not in the option set is decoded as a
    /// CUSTOM answer — so silently eating a real "None of the above" would not
    /// just hide a radio, it would change what the agent is told.
    #[test]
    fn classify_elicitation_non_codex_form_keeps_none_of_the_above() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Pick one",
                    "oneOf": [
                        {"const": "Incremental", "title": "Incremental"},
                        {
                            "const": "None of the above",
                            "title": "None of the above",
                            "description": "Provide a different answer in the note field."
                        }
                    ]
                }
            }),
            json!(["q1"]),
        );
        for peer in [
            ElicitationPeer::Other,
            ElicitationPeer::Codex(None),
            ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInTitle)),
            ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInDescription)),
        ] {
            let q = expect_questions(classify_elicitation(&raw, peer).unwrap());
            let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(
                labels,
                ["Incremental", "None of the above"],
                "no `_meta.codex` ⇒ not a codex form ⇒ no codex fixups (peer={peer:?})"
            );
        }
    }

    /// The gate is the PEER, not the payload. `_meta` is an open namespace, so
    /// a non-codex speaker — DeepSeek, or any MCP server whose form either
    /// adapter relays — can carry a `codex` block of its own meaning. If that
    /// were enough to unlock the codex fixups, a third party could make codeg
    /// hide one of its fields (the companion skip), read its questions upside
    /// down (the orientation flip) and delete one of its options (the synthetic
    /// filter). None of those may fire on `ElicitationPeer::Other`.
    ///
    /// The same payload under a codex peer is asserted right below, so the test
    /// shows the identity gate doing the deciding rather than the payload
    /// merely being inert.
    #[test]
    fn classify_elicitation_foreign_codex_meta_is_ignored_off_codex() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Approach",
                    "description": "Which approach should I take?",
                    "_meta": {"codex": {"isOther": true, "isSecret": false}},
                    "oneOf": [
                        {"const": "Incremental", "title": "Incremental"},
                        {
                            "const": "None of the above",
                            "title": "None of the above",
                            "description": "Provide a different answer in the note field."
                        }
                    ]
                },
                "q1_note": {
                    "type": "string",
                    "title": "Release note",
                    "description": "Anything to add?",
                    "_meta": {"codex": {"questionId": "q1", "role": "user_note"}}
                }
            }),
            json!([]),
        );

        // Not codex: every field is a real field, the JSON Schema reading
        // stands, and every option survives.
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Other).unwrap());
        assert_eq!(
            q.specs.len(),
            2,
            "a `user_note` block from a non-codex peer is not a companion"
        );
        assert_eq!(q.specs[0].question, "Which approach should I take?");
        assert_eq!(q.specs[0].header, "Approach");
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Incremental", "None of the above"]);

        // The identical payload from codex: companion skipped, 1.12 orientation,
        // injected option dropped. Same bytes, opposite treatment — the peer is
        // the only input that changed.
        let q = expect_questions(
            classify_elicitation(
                &raw,
                ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInTitle)),
            )
            .unwrap(),
        );
        assert_eq!(q.specs.len(), 1);
        assert_eq!(q.specs[0].question, "Approach");
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Incremental"]);
    }

    /// The synthetic-option filter matches THIS choice's own `oneOf` entry, by
    /// `const`, rather than asking whether the array contains a synthetic entry
    /// anywhere. Scanning would drop a legitimate option that merely shares the
    /// label with one — and since the labels collide, the dedup pass then eats
    /// the synthetic one too, leaving the real choice unselectable and its
    /// `const` unmappable on the way back.
    ///
    /// A conforming codex cannot send this pair (it writes `const: label` for
    /// every option and skips its injection when the label is already taken),
    /// so this pins the narrowing rather than a shape seen in the wild.
    #[test]
    fn classify_elicitation_keeps_a_same_label_option_with_its_own_const() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Approach",
                    "_meta": {"codex": {"isOther": true, "isSecret": false}},
                    "oneOf": [
                        {
                            "const": "real-none",
                            "title": "None of the above",
                            "description": "A real option the model wrote."
                        },
                        {
                            "const": "None of the above",
                            "title": "None of the above",
                            "description": "Provide a different answer in the note field."
                        }
                    ]
                }
            }),
            json!([]),
        );
        let q = expect_questions(
            classify_elicitation(
                &raw,
                ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInTitle)),
            )
            .unwrap(),
        );
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            ["None of the above"],
            "the model's own option survives; only the injected twin goes"
        );
        // …and it still round-trips to the model's `const`, not the synthetic
        // one, so the agent is told what the user actually picked.
        assert_eq!(
            q.fields[0].value_by_label.get("None of the above"),
            Some(&"real-none".to_string())
        );
    }

    /// …and within codex, a NON-`isOther` question is never injected into
    /// either, so its options are left alone too.
    #[test]
    fn classify_elicitation_codex_non_other_question_keeps_none_of_the_above() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Pick one",
                    "_meta": {"codex": {"isOther": false, "isSecret": false}},
                    "oneOf": [
                        {"const": "Incremental", "title": "Incremental"},
                        {
                            "const": "None of the above",
                            "title": "None of the above",
                            "description": "Provide a different answer in the note field."
                        }
                    ]
                }
            }),
            json!(["q1"]),
        );
        let q = expect_questions(
            classify_elicitation(&raw, ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInTitle))).unwrap(),
        );
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Incremental", "None of the above"]);
    }

    /// A generic MCP server's form has no `codex` namespace at all, so it keeps
    /// the JSON Schema reading codeg has always used — `description` is the
    /// prose, `title` the short label. The swap is codex-only.
    #[test]
    fn classify_elicitation_generic_mcp_form_keeps_description_as_the_question() {
        let raw = elicitation_raw(
            json!({
                "port": {
                    "type": "integer",
                    "title": "Port",
                    "description": "Which port should the server listen on?"
                }
            }),
            json!(["port"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs[0].question, "Which port should the server listen on?");
        assert_eq!(q.specs[0].header, "Port");
    }

    /// 1.12.0 omits `description` entirely when the model supplied no header —
    /// the question still comes from `title`, and the tab label is synthesized.
    #[test]
    fn classify_elicitation_codex_112_without_a_header_still_reads_the_title() {
        let raw = elicitation_raw(
            json!({
                "q1": {
                    "type": "string",
                    "title": "Which approach should I take?",
                    "_meta": {"codex": {"isOther": false, "isSecret": false}}
                }
            }),
            json!(["q1"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs[0].question, "Which approach should I take?");
        assert!(!q.specs[0].header.is_empty(), "header is synthesized");
    }

    /// Defense against over-skipping the renamed companion: a REAL question
    /// whose id merely ends in `_note` carries no `_meta.codex` marker, so it
    /// must render. (This is why the `_note` rename is matched by marker only,
    /// never by name — unlike the legacy `__other`.)
    #[test]
    fn classify_elicitation_keeps_an_unmarked_field_named_note() {
        let raw = elicitation_raw(
            json!({
                "q1": {"type": "string", "title": "Pick one", "enum": ["a", "b"]},
                "release_note": {"type": "string", "title": "Release note"}
            }),
            json!([]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        let ids: Vec<_> = q.specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["q1", "release_note"]);
    }

    #[test]
    fn classify_elicitation_free_text_renders_and_skips_other_companion() {
        // A free-text question (no options) renders as a 0-option spec (the
        // card shows its always-present "Other" input); the synthetic
        // `__other` companion (any digit-suffixed collision variant too) is
        // skipped.
        let raw = elicitation_raw(
            json!({
                "q1": {"type": "string", "title": "Name", "description": "Your name?",
                       "_meta": {"codex": {"isSecret": true}}},
                "q1__other": {"type": "string", "title": "Other"},
                "q1__other1": {"type": "string", "title": "Other"}
            }),
            json!([]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs.len(), 1);
        assert_eq!(q.specs[0].id, "q1");
        assert!(q.specs[0].options.is_empty(), "free text has no options");
        assert!(q.specs[0].is_secret, "secret marker read from raw _meta");
        assert!(validate_specs(&q.specs).is_ok(), "0-option specs register");

        // The typed answer rides the main field verbatim.
        let answer = QuestionAnswer {
            answers: vec![QuestionAnswerItem {
                question_id: "q1".into(),
                labels: vec!["Ada Lovelace".into()],
            }],
            declined: false,
        };
        let outcome = build_outcome(&q.specs, &answer);
        let v = serde_json::to_value(build_elicitation_response(&q, &outcome)).unwrap();
        assert_eq!(v["action"], "accept");
        assert_eq!(v["content"]["q1"], "Ada Lovelace");
    }

    #[test]
    fn classify_elicitation_skips_meta_marked_custom_answer_companion() {
        // claude-agent-acp ≥0.64 (#929) names its companion `question_<n>_custom`
        // — the `__other` heuristic misses it — and marks it with the shared,
        // un-namespaced `_meta._askUserQuestionCustomAnswer`. The marker alone
        // must collapse it into the card's built-in "Other" input.
        let raw = elicitation_raw(
            json!({
                "question_0": {
                    "type": "string",
                    "title": "Pick one",
                    "enum": ["a", "b"],
                },
                "question_0_custom": {
                    "type": "string",
                    "title": "Other",
                    "description": "Type your own answer instead.",
                    "_meta": {
                        "_askUserQuestionCustomAnswer": {
                            "questionId": "question_0",
                            "isCustomAnswer": true,
                        }
                    },
                },
            }),
            json!([]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs.len(), 1, "companion must not render as a question");
        assert_eq!(q.specs[0].id, "question_0");
        assert_eq!(q.specs[0].options.len(), 2);
    }

    #[test]
    fn classify_elicitation_keeps_unmarked_free_text_field() {
        // Defense against over-skipping: a plain string field that merely sits
        // next to a select — no marker, no `__other` name — is a real question.
        let raw = elicitation_raw(
            json!({
                "question_0": {"type": "string", "title": "Pick one", "enum": ["a", "b"]},
                "notes": {"type": "string", "title": "Notes"},
                "unmarked_custom": {
                    "type": "string",
                    "title": "Other",
                    "_meta": {"_askUserQuestionCustomAnswer": {"questionId": "question_0"}},
                },
            }),
            json!([]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        let ids: Vec<&str> = q.specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["notes", "question_0", "unmarked_custom"],
            "only `isCustomAnswer: true` skips; a marker without it does not"
        );
    }

    /// The `request_user_input` form codex-acp 2.1.1 sends codeg, verbatim but
    /// for the two ids: recorded live over stdio with codeg's exact
    /// `clientCapabilities`, a plan-mode turn whose model calls
    /// `request_user_input` (it matches upstream's
    /// `elicitation-user-input-air-custom-answer.json`). 2.1.0's recording
    /// lacks only the note field's root `_askUserQuestionCustomAnswer: true`.
    /// 2.0.1's lacks that and the note field's `jetbrains` block too, and its
    /// `next_step` `oneOf` carries a third, synthetic "None of the above"
    /// entry.
    #[test]
    fn classify_elicitation_reads_codex_211_air_custom_answer_form() {
        let raw = json!({
            "sessionId": "test-session-id",
            "toolCallId": "request-user-input-1",
            "mode": "form",
            "message": "Codex needs your input to continue.",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "next_step": {
                        "title": "What should I do next?",
                        "description": "Next step",
                        "_meta": {"codex": {"isOther": true, "isSecret": false}},
                        "type": "string",
                        "oneOf": [
                            {"const": "Run tests", "title": "Run tests",
                             "description": "Run the focused test suite."},
                            {"const": "Stop", "title": "Stop",
                             "description": "Stop and report current status."}
                        ]
                    },
                    "next_step_note": {
                        "type": "string",
                        "title": "Additional answer or note",
                        "_meta": {
                            "codex": {"questionId": "next_step", "role": "user_note", "isSecret": false},
                            "_askUserQuestionCustomAnswer": true,
                            "jetbrains": {"air": {"version": 1, "customAnswer": true}}
                        }
                    }
                },
                "required": ["next_step"]
            },
            "_meta": {"codex": {"autoResolutionMs": null}}
        });
        let q = expect_questions(
            classify_elicitation(
                &raw,
                ElicitationPeer::Codex(Some(CodexUserInputShape::QuestionInTitle)),
            )
            .unwrap(),
        );
        assert_eq!(q.specs.len(), 1, "the note field is the card's own Other");
        assert_eq!(q.specs[0].id, "next_step");
        assert_eq!(q.specs[0].question, "What should I do next?");
        assert_eq!(q.specs[0].header, "Next step");
        let labels: Vec<_> = q.specs[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect();
        assert_eq!(labels, ["Run tests", "Stop"]);
        assert_eq!(q.tool_call_id.as_deref(), Some("request-user-input-1"));

        // What codeg writes back, and (recorded in the same live run, off the
        // tool output the model then received) what codex is handed for it:
        //   pick "Run tests"          → content.next_step = "Run tests"
        //                               → codex ["Run tests"]
        //   type "Inspect flaky logs" → content.next_step = "Inspect flaky logs"
        //                               → codex ["None of the above",
        //                                        "user_note: Inspect flaky logs"]
        // The typed text goes to the MAIN field and nothing to the note field:
        // that main-field string is what 2.1.0+ reads as the AIR custom answer.
        for (picked, expected) in [
            ("Run tests", "Run tests"),
            ("Inspect flaky logs", "Inspect flaky logs"),
        ] {
            let answer = QuestionAnswer {
                answers: vec![QuestionAnswerItem {
                    question_id: "next_step".into(),
                    labels: vec![picked.into()],
                }],
                declined: false,
            };
            let outcome = build_outcome(&q.specs, &answer);
            let v = serde_json::to_value(build_elicitation_response(&q, &outcome)).unwrap();
            assert_eq!(v["action"], "accept");
            assert_eq!(v["content"], json!({"next_step": expected}));
        }
    }

    /// The AIR `customAnswer` key carries a different value per adapter: codex
    /// 2.1.0 writes a bare `true`, claude 0.82+ `{questionId, isCustomAnswer:
    /// true}`. Either marks the companion. Asserted under a NON-codex peer,
    /// where the marker is the only thing that can skip a field (a codex peer's
    /// note field would already be skipped by its `_meta.codex.role`).
    #[test]
    fn classify_elicitation_reads_both_air_custom_answer_values() {
        let raw = elicitation_raw(
            json!({
                "q1": {"type": "string", "title": "Pick one", "enum": ["a", "b"]},
                "codex_style": {
                    "type": "string",
                    "title": "Additional answer or note",
                    "_meta": {"jetbrains": {"air": {"version": 1, "customAnswer": true}}}
                },
                "claude_style": {
                    "type": "string",
                    "title": "Other",
                    "_meta": {"jetbrains": {"air": {
                        "version": 1,
                        "customAnswer": {"questionId": "q1", "isCustomAnswer": true}
                    }}}
                },
                "bool_false": {
                    "type": "string",
                    "title": "Kept 1",
                    "_meta": {"jetbrains": {"air": {"version": 1, "customAnswer": false}}}
                },
                "object_false": {
                    "type": "string",
                    "title": "Kept 2",
                    "_meta": {"jetbrains": {"air": {
                        "version": 1,
                        "customAnswer": {"questionId": "q1", "isCustomAnswer": false}
                    }}}
                },
                "string_true": {
                    "type": "string",
                    "title": "Kept 3",
                    "_meta": {"jetbrains": {"air": {"version": 1, "customAnswer": "true"}}}
                }
            }),
            json!(["q1"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Other).unwrap());
        let ids: Vec<&str> = q.specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["bool_false", "object_false", "q1", "string_true"],
            "a bare `true` or `isCustomAnswer: true` skips; nothing else does"
        );
    }

    /// codex-acp 2.1.1 (#577) also writes its bare `true` under the old root
    /// key, the one claude's marker used before it moved under AIR. Read on its
    /// own (a non-codex peer, no AIR key), it skips the companion like the
    /// object form always did; `false` does not.
    #[test]
    fn classify_elicitation_reads_a_bare_true_under_the_legacy_key() {
        let raw = elicitation_raw(
            json!({
                "q1": {"type": "string", "title": "Pick one", "enum": ["a", "b"]},
                "bare_true": {
                    "type": "string",
                    "title": "Additional answer or note",
                    "_meta": {"_askUserQuestionCustomAnswer": true}
                },
                "object_true": {
                    "type": "string",
                    "title": "Other",
                    "_meta": {"_askUserQuestionCustomAnswer": {
                        "questionId": "q1",
                        "isCustomAnswer": true
                    }}
                },
                "bare_false": {
                    "type": "string",
                    "title": "Kept",
                    "_meta": {"_askUserQuestionCustomAnswer": false}
                }
            }),
            json!(["q1"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Other).unwrap());
        let ids: Vec<&str> = q.specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["bare_false", "q1"]);
    }

    #[test]
    fn classify_elicitation_multi_select_marks_multi_and_maps_values() {
        let raw = elicitation_raw(
            json!({
                "langs": {
                    "type": "array",
                    "title": "Langs",
                    "description": "Pick languages",
                    "items": { "anyOf": [
                        {"const": "rust", "title": "Rust"},
                        {"const": "ts", "title": "TS"}
                    ]}
                }
            }),
            json!([]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs.len(), 1);
        assert!(q.specs[0].multi_select);
        // Titles display; consts ride back on accept.
        let labels: Vec<_> = q.specs[0].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Rust", "TS"]);
        let answer = QuestionAnswer {
            answers: vec![QuestionAnswerItem {
                question_id: q.specs[0].id.clone(),
                labels: vec!["Rust".into(), "TS".into()],
            }],
            declined: false,
        };
        let outcome = build_outcome(&q.specs, &answer);
        let v = serde_json::to_value(build_elicitation_response(&q, &outcome)).unwrap();
        assert_eq!(v["content"]["langs"], json!(["rust", "ts"]));
    }

    #[test]
    fn classify_elicitation_boolean_and_numbers_type_their_answers() {
        let raw = elicitation_raw(
            json!({
                "confirm": {"type": "boolean", "title": "Confirm", "description": "Proceed?"},
                "count": {"type": "integer", "title": "Count", "description": "How many?"},
                "ratio": {"type": "number", "title": "Ratio", "description": "What ratio?"}
            }),
            json!(["confirm"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs.len(), 3);
        // Booleans render as Yes/No; numbers as free text.
        let confirm = q.specs.iter().position(|s| s.id == "confirm").unwrap();
        let labels: Vec<_> = q.specs[confirm].options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Yes", "No"]);

        let answer = QuestionAnswer {
            answers: vec![
                QuestionAnswerItem {
                    question_id: "confirm".into(),
                    labels: vec!["Yes".into()],
                },
                QuestionAnswerItem {
                    question_id: "count".into(),
                    labels: vec!["42".into()],
                },
                QuestionAnswerItem {
                    question_id: "ratio".into(),
                    labels: vec!["not-a-number".into()],
                },
            ],
            declined: false,
        };
        let outcome = build_outcome(&q.specs, &answer);
        let v = serde_json::to_value(build_elicitation_response(&q, &outcome)).unwrap();
        assert_eq!(v["content"]["confirm"], json!(true), "boolean typed");
        assert_eq!(v["content"]["count"], json!(42), "integer typed");
        assert!(
            v["content"].get("ratio").is_none(),
            "unparseable number omitted, not sent mistyped"
        );
    }

    #[test]
    fn classify_elicitation_enum_names_display_title_but_send_const() {
        // Generic MCP `enum` + `enumNames` normalize to oneOf with
        // const ≠ title: display the title, send the const back.
        let raw = elicitation_raw(
            json!({
                "env": {
                    "type": "string",
                    "title": "Env",
                    "description": "Which environment?",
                    "oneOf": [
                        {"const": "prod-eu-1", "title": "Production (EU)"},
                        {"const": "stg-eu-1", "title": "Staging (EU)"}
                    ]
                }
            }),
            json!(["env"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(q.specs[0].options[0].label, "Production (EU)");
        let answer = QuestionAnswer {
            answers: vec![QuestionAnswerItem {
                question_id: "env".into(),
                labels: vec!["Production (EU)".into()],
            }],
            declined: false,
        };
        let outcome = build_outcome(&q.specs, &answer);
        let v = serde_json::to_value(build_elicitation_response(&q, &outcome)).unwrap();
        assert_eq!(v["content"]["env"], "prod-eu-1");
    }

    #[test]
    fn classify_elicitation_tool_approval_maps_persist_options() {
        // The exact wire shape codex-acp sends for an MCP tool-call approval
        // once `elicitation.form` is advertised (verified against its
        // elicitation-events tests): message + persist choice, approval marker
        // in top-level `_meta`.
        let raw = json!({
            "mode": "form",
            "sessionId": "sess-1",
            "toolCallId": "call-1",
            "message": "Allow tool call?",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "persist": {
                        "type": "string",
                        "title": "Approval scope",
                        "oneOf": [
                            {"const": "once", "title": "Allow once"},
                            {"const": "session", "title": "Allow for this session"},
                            {"const": "always", "title": "Allow and don't ask again"}
                        ],
                        "default": "once"
                    }
                },
                "required": ["persist"]
            },
            "_meta": {"codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"]}
        });
        let approval = expect_approval(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(approval.message, "Allow tool call?");
        assert_eq!(approval.tool_call_id.as_deref(), Some("call-1"));
        assert!(approval.persist_in_content);
        let ids: Vec<_> = approval.options.iter().map(|o| o.option_id.as_str()).collect();
        assert_eq!(ids, ["once", "session", "always", ELICITATION_DECLINE_OPTION_ID]);
        assert_eq!(approval.options[0].label, "Allow once");
        assert_eq!(approval.options[0].kind, "allow_once");
        assert_eq!(approval.options[1].kind, "allow_always");

        // Accepting echoes the chosen persist back in content…
        let v =
            serde_json::to_value(build_elicitation_approval_response(&approval, "session"))
                .unwrap();
        assert_eq!(v["action"], "accept");
        assert_eq!(v["content"]["persist"], "session");
        // …declining (or an unknown option) maps to decline.
        let v = serde_json::to_value(build_elicitation_approval_response(
            &approval,
            ELICITATION_DECLINE_OPTION_ID,
        ))
        .unwrap();
        assert_eq!(v["action"], "decline");
        let v = serde_json::to_value(build_elicitation_approval_response(&approval, "bogus"))
            .unwrap();
        assert_eq!(v["action"], "decline");
    }

    #[test]
    fn classify_elicitation_tool_approval_without_persist_is_allow_decline() {
        // No persist advertised → codex-acp sends an EMPTY properties object.
        // This must NOT auto-decline: it renders Allow/Decline and accepts
        // with no content.
        let raw = json!({
            "mode": "form",
            "sessionId": "sess-1",
            "toolCallId": "call-1",
            "message": "Allow tool call?",
            "requestedSchema": {"type": "object", "properties": {}},
            "_meta": {"codex_approval_kind": "mcp_tool_call"}
        });
        let approval = expect_approval(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert!(!approval.persist_in_content);
        let ids: Vec<_> = approval.options.iter().map(|o| o.option_id.as_str()).collect();
        assert_eq!(ids, ["accept", ELICITATION_DECLINE_OPTION_ID]);
        let v = serde_json::to_value(build_elicitation_approval_response(&approval, "accept"))
            .unwrap();
        assert_eq!(v["action"], "accept");
        assert!(
            v.get("content").is_none() || v["content"].is_null(),
            "plain accept sends no content"
        );
    }

    #[test]
    fn classify_elicitation_message_only_form_is_accept_decline_confirm() {
        // A non-approval form with nothing to fill in (a bare MCP server
        // confirmation) renders Accept/Decline rather than auto-declining.
        let raw = elicitation_raw(json!({}), json!([]));
        let approval = expect_approval(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        assert_eq!(approval.message, "Input requested");
        let ids: Vec<_> = approval.options.iter().map(|o| o.option_id.as_str()).collect();
        assert_eq!(ids, ["accept", ELICITATION_DECLINE_OPTION_ID]);
    }

    #[test]
    fn elicitation_round_trips_answer_keyed_by_property_id() {
        let raw = elicitation_raw(
            json!({
                "colour": {
                    "type": "string",
                    "title": "Colour",
                    "description": "Pick a colour",
                    "oneOf": [
                        {"const": "Red", "title": "Red"},
                        {"const": "Blue", "title": "Blue"}
                    ]
                }
            }),
            json!(["colour"]),
        );
        let q = expect_questions(classify_elicitation(&raw, ElicitationPeer::Codex(None)).unwrap());
        // Simulate the user picking "Blue" through the normal answer path.
        let answer = QuestionAnswer {
            answers: vec![QuestionAnswerItem {
                question_id: "colour".into(),
                labels: vec!["Blue".into()],
            }],
            declined: false,
        };
        let outcome = build_outcome(&q.specs, &answer);
        let resp = build_elicitation_response(&q, &outcome);
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["action"], "accept");
        // Codex reads the answer back by the schema property id.
        assert_eq!(v["content"]["colour"], "Blue");
    }

    #[test]
    fn build_elicitation_response_declined_maps_to_decline() {
        let outcome = QuestionOutcome {
            answers: vec![],
            declined: true,
        };
        let empty = ElicitationQuestions {
            specs: vec![],
            fields: vec![],
            tool_call_id: None,
        };
        let v = serde_json::to_value(build_elicitation_response(&empty, &outcome)).unwrap();
        assert_eq!(v["action"], "decline");
        let v = serde_json::to_value(elicitation_cancel_response()).unwrap();
        assert_eq!(v["action"], "cancel");
    }

    #[test]
    fn validate_specs_accepts_well_formed_and_rejects_malformed() {
        // What parse_questions mints passes the request-side re-check.
        let good = parse_questions(&valid_args()).unwrap();
        assert!(validate_specs(&good).is_ok());

        // Build a spec with a tunable question, option count, and first-label
        // length so each bound can be tripped independently.
        let spec = |question: &str, options: usize, first_label_len: usize| QuestionSpec {
            id: "q".into(),
            question: question.into(),
            header: "H".into(),
            multi_select: false,
            options: (0..options)
                .map(|i| QuestionOption {
                    label: if first_label_len > 0 && i == 0 {
                        "x".repeat(first_label_len)
                    } else {
                        format!("opt{i}")
                    },
                    description: String::new(),
                })
                .collect(),
            is_secret: false,
        };

        assert!(validate_specs(&[]).is_err(), "empty set");
        assert!(
            validate_specs(&[spec(&"q".repeat(MAX_QUESTION_TEXT_CHARS + 1), 2, 0)]).is_err(),
            "oversized question text"
        );
        assert!(
            validate_specs(&[spec("ok", 0, 0)]).is_ok(),
            "0 options is a legal free-text ask (elicitation / MCP forms)"
        );
        assert!(
            validate_specs(&[spec("ok", MAX_OPTIONS + 1, 0)]).is_err(),
            "too many options"
        );
        assert!(
            validate_specs(&[spec("ok", 2, MAX_QUESTION_TEXT_CHARS + 1)]).is_err(),
            "oversized option label"
        );
        assert!(validate_specs(&[spec("   ", 2, 0)]).is_err(), "blank question");

        // Duplicate question id across the set (spec() hardcodes id "q") — answer
        // routing + UI state key on id, so duplicates must be rejected.
        assert!(
            validate_specs(&[spec("a", 2, 0), spec("b", 2, 0)]).is_err(),
            "duplicate question id"
        );
        let blank_id = QuestionSpec {
            id: "  ".into(),
            question: "ok".into(),
            header: "H".into(),
            multi_select: false,
            options: vec![
                QuestionOption {
                    label: "a".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "b".into(),
                    description: String::new(),
                },
            ],
            is_secret: false,
        };
        assert!(validate_specs(&[blank_id]).is_err(), "blank id");
        // Duplicate option label within one question (parse_questions rejects it).
        let dup_label = QuestionSpec {
            id: "q".into(),
            question: "ok".into(),
            header: "H".into(),
            multi_select: false,
            options: vec![
                QuestionOption {
                    label: "same".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: " same ".into(),
                    description: String::new(),
                },
            ],
            is_secret: false,
        };
        assert!(
            validate_specs(&[dup_label]).is_err(),
            "duplicate option label (trimmed)"
        );
    }

    #[test]
    fn parse_questions_rejects_empty_and_overlong_sets() {
        assert!(parse_questions(&json!({ "questions": [] })).is_err());
        assert!(parse_questions(&json!({})).is_err());
        let mut many = Vec::new();
        for _ in 0..(MAX_QUESTIONS + 1) {
            many.push(json!({
                "question": "q", "header": "h", "multiSelect": false,
                "options": [{ "label": "a", "description": "" }, { "label": "b", "description": "" }]
            }));
        }
        assert!(parse_questions(&json!({ "questions": many })).is_err());
    }

    #[test]
    fn parse_questions_enforces_option_count_and_header_len() {
        // One option is not a choice.
        let one_opt = json!({ "questions": [{
            "question": "q", "header": "h", "multiSelect": false,
            "options": [{ "label": "only", "description": "" }]
        }] });
        assert!(parse_questions(&one_opt).is_err());
        // Header too long.
        let long_header = json!({ "questions": [{
            "question": "q", "header": "this-header-is-way-too-long", "multiSelect": false,
            "options": [{ "label": "a", "description": "" }, { "label": "b", "description": "" }]
        }] });
        assert!(parse_questions(&long_header).is_err());
    }

    #[test]
    fn build_outcome_maps_labels_by_id_and_drops_unknown() {
        let qs = parse_questions(&valid_args()).unwrap();
        let qid = qs[0].id.clone();
        let answer = QuestionAnswer {
            answers: vec![
                QuestionAnswerItem {
                    question_id: qid,
                    labels: vec!["Incremental".into()],
                },
                QuestionAnswerItem {
                    question_id: "does-not-exist".into(),
                    labels: vec!["ghost".into()],
                },
            ],
            declined: false,
        };
        let outcome = build_outcome(&qs, &answer);
        assert!(!outcome.declined);
        assert_eq!(outcome.answers.len(), 1);
        assert_eq!(outcome.answers[0].question, "Which approach?");
        assert_eq!(outcome.answers[0].selected, vec!["Incremental".to_string()]);
    }

    #[test]
    fn parse_questions_rejects_overlong_option_label() {
        let huge = "x".repeat(MAX_QUESTION_TEXT_CHARS + 1);
        let bad = json!({ "questions": [{
            "question": "q", "header": "h", "multiSelect": false,
            "options": [
                { "label": huge, "description": "" },
                { "label": "B", "description": "" }
            ]
        }] });
        let err = parse_questions(&bad).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn build_outcome_caps_multiselect_labels_and_ignores_unknown() {
        // A multi-select question with 2 options: a flood of submitted labels
        // plus a flood of unknown answer items must NOT bloat the outcome.
        let args = json!({ "questions": [{
            "question": "Which modules?", "header": "Scope", "multiSelect": true,
            "options": [
                { "label": "auth", "description": "" },
                { "label": "billing", "description": "" }
            ]
        }] });
        let qs = parse_questions(&args).unwrap();
        let qid = qs[0].id.clone();
        let mut items = vec![QuestionAnswerItem {
            question_id: qid,
            labels: (0..1000).map(|i| format!("l{i}")).collect(),
        }];
        // 10k unknown answer items — must be ignored without growth.
        for i in 0..10_000 {
            items.push(QuestionAnswerItem {
                question_id: format!("ghost-{i}"),
                labels: vec!["x".into()],
            });
        }
        let outcome = build_outcome(&qs, &QuestionAnswer { answers: items, declined: false });
        assert_eq!(outcome.answers.len(), 1);
        // Cap = options.len() + 1 = 3 (every real option plus one "Other"); the
        // FIRST three are kept (early break — labels past the cap and the 10k
        // unknown items are never processed/retained).
        assert_eq!(
            outcome.answers[0].selected,
            vec!["l0".to_string(), "l1".to_string(), "l2".to_string()]
        );
    }

    #[test]
    fn parse_questions_rejects_duplicate_option_labels() {
        let dup = json!({ "questions": [{
            "question": "q", "header": "h", "multiSelect": false,
            "options": [
                { "label": "Same", "description": "a" },
                { "label": "Same", "description": "b" }
            ]
        }] });
        let err = parse_questions(&dup).unwrap_err();
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn build_outcome_normalizes_malformed_answer() {
        // Single-select with two labels + an empty + an oversize one, plus a
        // duplicate answer item and an unknown question id. The endpoint must
        // not trust this; build_outcome sanitizes it.
        let qs = parse_questions(&valid_args()).unwrap();
        let qid = qs[0].id.clone();
        let huge = "x".repeat(MAX_QUESTION_TEXT_CHARS + 50);
        let answer = QuestionAnswer {
            answers: vec![
                QuestionAnswerItem {
                    question_id: qid.clone(),
                    labels: vec!["  ".into(), "Incremental".into(), huge.clone()],
                },
                // Duplicate item for the same question — must be deduped (first wins).
                QuestionAnswerItem {
                    question_id: qid,
                    labels: vec!["Rewrite".into()],
                },
                // Unknown question — dropped.
                QuestionAnswerItem {
                    question_id: "ghost".into(),
                    labels: vec!["x".into()],
                },
            ],
            declined: false,
        };
        let outcome = build_outcome(&qs, &answer);
        assert_eq!(outcome.answers.len(), 1, "deduped + unknown dropped");
        // Single-select: empty trimmed away, then truncated to one → "Incremental".
        assert_eq!(outcome.answers[0].selected, vec!["Incremental".to_string()]);
    }

    #[test]
    fn build_outcome_drops_question_with_only_empty_labels() {
        let qs = parse_questions(&valid_args()).unwrap();
        let outcome = build_outcome(
            &qs,
            &QuestionAnswer {
                answers: vec![QuestionAnswerItem {
                    question_id: qs[0].id.clone(),
                    labels: vec!["   ".into(), "".into()],
                }],
                declined: false,
            },
        );
        assert!(outcome.answers.is_empty());
    }

    #[test]
    fn build_outcome_declined_is_empty() {
        let qs = parse_questions(&valid_args()).unwrap();
        let outcome = build_outcome(
            &qs,
            &QuestionAnswer {
                answers: vec![],
                declined: true,
            },
        );
        assert!(outcome.declined);
        assert!(outcome.answers.is_empty());
    }

    #[tokio::test]
    async fn runtime_config_hot_swaps() {
        let cfg = QuestionRuntimeConfig::new();
        assert!(!cfg.is_enabled().await);
        cfg.set(QuestionConfig { enabled: true }).await;
        assert!(cfg.is_enabled().await);
    }

    fn grok_params(questions: Value) -> Value {
        json!({
            "sessionId": "s-1",
            "toolCallId": "call-1",
            "questions": questions,
            "mode": "default",
        })
    }

    #[test]
    fn parse_grok_ext_maps_shape_and_synthesizes_header() {
        // Grok's wire shape: no `header`, camelCase `multiSelect`.
        let specs = parse_grok_ext_questions(&grok_params(json!([{
            "question": "What is your favorite color?",
            "options": [
                { "label": "Red", "description": "warm" },
                { "label": "Blue", "description": "cool" }
            ],
            "multiSelect": false
        }])))
        .unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].question, "What is your favorite color?");
        assert!(!specs[0].multi_select);
        assert_eq!(specs[0].options.len(), 2);
        // Header is synthesized, non-empty, and within the chip bound.
        assert!(!specs[0].header.is_empty());
        assert!(specs[0].header.chars().count() <= MAX_HEADER_CHARS);
        // Whatever we synthesize MUST satisfy the register-time re-validation,
        // else `register_question` silently declines the whole ask.
        validate_specs(&specs).expect("synthesized specs must pass validate_specs");
    }

    #[test]
    fn parse_grok_ext_clamps_questions_and_options_to_bounds() {
        // 6 questions, each with 6 options — both past codeg's maxima.
        let many: Vec<Value> = (0..6)
            .map(|qi| {
                let opts: Vec<Value> = (0..6)
                    .map(|oi| json!({ "label": format!("q{qi}o{oi}"), "description": "" }))
                    .collect();
                json!({ "question": format!("Question {qi}?"), "options": opts })
            })
            .collect();
        let specs = parse_grok_ext_questions(&grok_params(json!(many))).unwrap();
        assert_eq!(specs.len(), MAX_QUESTIONS, "questions clamped");
        assert!(specs.iter().all(|s| s.options.len() == MAX_OPTIONS), "options clamped");
        validate_specs(&specs).unwrap();
    }

    #[test]
    fn parse_grok_ext_dedups_option_labels() {
        // Duplicate labels would fail validate_specs; we dedup, keeping enough.
        let specs = parse_grok_ext_questions(&grok_params(json!([{
            "question": "Pick one",
            "options": [
                { "label": "A", "description": "" },
                { "label": "A", "description": "dup" },
                { "label": "B", "description": "" }
            ]
        }])))
        .unwrap();
        assert_eq!(specs[0].options.len(), 2);
        validate_specs(&specs).unwrap();
    }

    #[test]
    fn parse_grok_ext_rejects_degenerate_and_malformed() {
        // Fewer than two usable options is not a real choice → fail (grok falls back).
        assert!(parse_grok_ext_questions(&grok_params(json!([{
            "question": "Only one",
            "options": [{ "label": "Solo", "description": "" }]
        }])))
        .is_err());
        // Missing questions array.
        assert!(parse_grok_ext_questions(&json!({ "mode": "default" })).is_err());
        // Empty questions.
        assert!(parse_grok_ext_questions(&grok_params(json!([]))).is_err());
    }

    /// The `toolCall` of a `session/request_permission` captured from a live
    /// pi-acp 0.0.33 run of an extension calling `ctx.ui.select` (#644).
    fn pi_select_tool_call() -> Value {
        json!({
            "toolCallId": "pi-ui-69c549d7-9f79-4de6-a860-7ae4d27553f7",
            "title": "[未提交改动] 当前分支有未提交的 AddIOP.cs 修改；创建热修复分支时应如何处理？",
            "kind": "other",
            "status": "pending",
            "rawInput": {
                "method": "select",
                "title": "[未提交改动] 当前分支有未提交的 AddIOP.cs 修改；创建热修复分支时应如何处理？",
                "options": [
                    "1. 暂存后切分支 (Recommended) — 把当前改动保存到具名 stash",
                    "2. 独立 worktree — 保留当前工作区不动",
                    "3. 携带改动切换 — 直接创建并切换分支",
                    "4. Type something."
                ]
            }
        })
    }

    /// The paired permission `options`, same run.
    fn pi_select_options() -> Vec<(String, String)> {
        [
            "1. 暂存后切分支 (Recommended) — 把当前改动保存到具名 stash",
            "2. 独立 worktree — 保留当前工作区不动",
            "3. 携带改动切换 — 直接创建并切换分支",
            "4. Type something.",
        ]
        .iter()
        .enumerate()
        .map(|(i, name)| (format!("choice-{i}"), (*name).to_string()))
        .collect()
    }

    #[test]
    fn parse_pi_select_ask_maps_captured_wire_shape() {
        let ask = parse_pi_select_ask(&pi_select_tool_call(), &pi_select_options())
            .expect("pi select must be recognized");
        assert_eq!(
            ask.spec.question,
            "[未提交改动] 当前分支有未提交的 AddIOP.cs 修改；创建热修复分支时应如何处理？"
        );
        // `ctx.ui.select` yields one value.
        assert!(!ask.spec.multi_select);
        assert_eq!(ask.spec.options.len(), 4);
        assert_eq!(
            ask.spec.options[0].label,
            "1. 暂存后切分支 (Recommended) — 把当前改动保存到具名 stash"
        );
        // Whatever we synthesize MUST satisfy the register-time re-validation,
        // else `register_question` silently declines the ask and the user is
        // left with the raw approval card.
        validate_specs(std::slice::from_ref(&ask.spec))
            .expect("synthesized spec must pass validate_specs");
        // Labels round-trip to the ids pi replies against.
        assert_eq!(
            ask.option_ids
                .iter()
                .map(|(_, id)| id.as_str())
                .collect::<Vec<_>>(),
            ["choice-0", "choice-1", "choice-2", "choice-3"]
        );
    }

    #[test]
    fn parse_pi_select_ask_declines_what_it_cannot_render_faithfully() {
        let opts = |n: usize| -> Vec<(String, String)> {
            (0..n)
                .map(|i| (format!("choice-{i}"), format!("option {i}")))
                .collect()
        };
        // More options than the card can show: truncating would HIDE choices the
        // user must be able to pick, so keep the approval card (which lists all).
        assert!(parse_pi_select_ask(&pi_select_tool_call(), &opts(MAX_OPTIONS + 1)).is_none());
        assert!(parse_pi_select_ask(&pi_select_tool_call(), &opts(MIN_OPTIONS - 1)).is_none());
        // Duplicate labels: the label is the card's selection identity, so a
        // duplicate could answer pi with the wrong index.
        let dup = vec![
            ("choice-0".to_string(), "Same".to_string()),
            ("choice-1".to_string(), "Same".to_string()),
        ];
        assert!(parse_pi_select_ask(&pi_select_tool_call(), &dup).is_none());
        // Blank label.
        let blank = vec![
            ("choice-0".to_string(), "  ".to_string()),
            ("choice-1".to_string(), "B".to_string()),
        ];
        assert!(parse_pi_select_ask(&pi_select_tool_call(), &blank).is_none());
    }

    #[test]
    fn parse_pi_select_ask_ignores_other_permission_requests() {
        let strip = |f: &str| {
            let mut tc = pi_select_tool_call();
            tc.as_object_mut().unwrap().remove(f);
            tc
        };
        // An ordinary tool approval (claude/codex/pi's own tools) has no pi-ui id.
        let mut edit = pi_select_tool_call();
        edit["toolCallId"] = json!("call_abc");
        assert!(parse_pi_select_ask(&edit, &pi_select_options()).is_none());
        // pi's `confirm` dialog is a real yes/no approval, which the permission
        // card already models (allow_once / reject_once).
        let mut confirm = pi_select_tool_call();
        confirm["rawInput"]["method"] = json!("confirm");
        assert!(parse_pi_select_ask(&confirm, &pi_select_options()).is_none());
        assert!(parse_pi_select_ask(&strip("rawInput"), &pi_select_options()).is_none());
        // Option ids that aren't pi-acp's `choice-<index>`: the reply mapping
        // would be a guess.
        let foreign = vec![
            ("allow".to_string(), "Allow".to_string()),
            ("deny".to_string(), "Deny".to_string()),
        ];
        assert!(parse_pi_select_ask(&pi_select_tool_call(), &foreign).is_none());
    }

    #[test]
    fn is_codeg_ask_tool_name_accepts_every_host_spelling_of_codegs_own_tool() {
        for spelling in [
            // claude-agent-acp: an MCP tool's permission card title IS the
            // raw tool name.
            "mcp__codeg-mcp__ask_user_question",
            "codeg-mcp/ask_user_question",
            "codeg-mcp: ask_user_question",
            "mcp.codeg-mcp.ask_user_question",
            // Hosts that title-case or pad it.
            "  MCP__Codeg-MCP__Ask_User_Question  ",
        ] {
            assert!(
                is_codeg_ask_tool_name(spelling),
                "{spelling} is codeg's own ask tool"
            );
        }
    }

    #[test]
    fn is_codeg_ask_tool_name_rejects_tools_that_are_not_codegs_ask() {
        for other in [
            // A third-party MCP server's similarly named tool: approving it is
            // the user's decision, so the bare suffix must NOT be enough.
            "mcp__other-server__ask_user_question",
            "ask_user_question",
            // grok's NATIVE ask arrives on its own ext channel, never as a
            // permission request — and it is not codeg-mcp's tool either.
            "_x.ai/ask_user_question",
            // Codeg's other companion tools keep their approval gate.
            "mcp__codeg-mcp__delegate_to_agent",
            "mcp__codeg-mcp__check_user_feedback",
            // Right server, right words, wrong tool — the match is anchored at
            // the END so a longer name cannot borrow it.
            "mcp__codeg-mcp__ask_user_question_twice",
            "",
        ] {
            assert!(
                !is_codeg_ask_tool_name(other),
                "{other} must keep its approval card"
            );
        }
    }

    #[test]
    fn pi_select_option_id_maps_pick_and_cancels_what_pi_cannot_take() {
        let ask = parse_pi_select_ask(&pi_select_tool_call(), &pi_select_options()).unwrap();
        let answered = |labels: Vec<&str>| QuestionOutcome {
            answers: vec![QuestionAnsweredItem {
                question: ask.spec.question.clone(),
                header: ask.spec.header.clone(),
                multi_select: false,
                selected: labels.into_iter().map(str::to_string).collect(),
            }],
            declined: false,
        };
        assert_eq!(
            pi_select_option_id(&answered(vec![&ask.spec.options[1].label]), &ask).as_deref(),
            Some("choice-1")
        );
        // Declined card → cancel, which is what `ctx.ui.select` sees on Escape.
        assert_eq!(
            pi_select_option_id(
                &QuestionOutcome {
                    answers: vec![],
                    declined: true
                },
                &ask
            ),
            None
        );
        // Free text typed into the card's always-present "Other" box has no
        // option id, so pi can only be told the dialog was cancelled.
        assert_eq!(
            pi_select_option_id(&answered(vec!["something else"]), &ask),
            None
        );
        assert_eq!(pi_select_option_id(&answered(vec![]), &ask), None);
    }

    #[test]
    fn build_grok_ext_response_single_select_is_bare_string() {
        let outcome = QuestionOutcome {
            answers: vec![QuestionAnsweredItem {
                question: "What is your favorite color?".into(),
                header: "What is your".into(),
                multi_select: false,
                selected: vec!["Red".into()],
            }],
            declined: false,
        };
        let v = build_grok_ext_response(&outcome);
        assert_eq!(v["outcome"], "accepted");
        // Keyed by question text; single-select value is a bare string.
        assert_eq!(v["answers"]["What is your favorite color?"], json!("Red"));
        assert!(v["partial_answers"].as_object().unwrap().is_empty());
    }

    #[test]
    fn build_grok_ext_response_multi_select_is_array() {
        let outcome = QuestionOutcome {
            answers: vec![QuestionAnsweredItem {
                question: "Which modules?".into(),
                header: "Which module".into(),
                multi_select: true,
                selected: vec!["auth".into(), "billing".into()],
            }],
            declined: false,
        };
        let v = build_grok_ext_response(&outcome);
        assert_eq!(v["outcome"], "accepted");
        assert_eq!(v["answers"]["Which modules?"], json!(["auth", "billing"]));
    }

    #[test]
    fn build_grok_ext_response_declined_is_skip_interview() {
        let outcome = QuestionOutcome {
            answers: vec![],
            declined: true,
        };
        assert_eq!(build_grok_ext_response(&outcome), json!({ "outcome": "skip_interview" }));
        assert_eq!(grok_ext_skip_response(), json!({ "outcome": "skip_interview" }));
    }

    #[test]
    fn grok_result_card_input_omits_header_and_keeps_shape() {
        // Built from parsed specs (which DO carry a synthesized header) — the card
        // input must nonetheless drop it, so the frontend parses `header:""`.
        let specs = parse_grok_ext_questions(&grok_params(json!([{
            "question": "Which colors?",
            "options": [
                { "label": "Red", "description": "warm" },
                { "label": "Green", "description": "cool" }
            ],
            "multiSelect": true
        }])))
        .unwrap();
        let input = grok_result_card_input(&specs);
        let q = &input["questions"][0];
        assert_eq!(q["question"], "Which colors?");
        assert_eq!(q["multiSelect"], true);
        assert_eq!(q["options"][0]["label"], "Red");
        // No header field is serialized (the frontend reads it as "").
        assert!(q.get("header").is_none(), "input must not carry a header");
    }

    #[test]
    fn grok_result_card_output_single_select_empty_header() {
        let outcome = QuestionOutcome {
            answers: vec![QuestionAnsweredItem {
                question: "你更喜欢哪种演示方式？".into(),
                header: "你更喜欢哪种".into(), // synthesized upstream; must be dropped here
                multi_select: false,
                selected: vec!["随便看看".into()],
            }],
            declined: false,
        };
        let out = grok_result_card_output(&outcome);
        assert_eq!(out["declined"], false);
        let a = &out["answers"][0];
        // Header is forced empty to align with the header-less card input.
        assert_eq!(a["header"], "");
        assert_eq!(a["question"], "你更喜欢哪种演示方式？");
        assert_eq!(a["selected"], json!(["随便看看"]));
    }

    #[test]
    fn grok_result_card_output_multi_and_declined() {
        let multi = QuestionOutcome {
            answers: vec![QuestionAnsweredItem {
                question: "Which colors?".into(),
                header: "Which colors".into(),
                multi_select: true,
                selected: vec!["Red".into(), "Green".into()],
            }],
            declined: false,
        };
        assert_eq!(
            grok_result_card_output(&multi)["answers"][0]["selected"],
            json!(["Red", "Green"])
        );
        let declined = QuestionOutcome {
            answers: vec![],
            declined: true,
        };
        let out = grok_result_card_output(&declined);
        assert_eq!(out["declined"], true);
        assert_eq!(out["answers"], json!([]));
    }

    #[test]
    fn grok_result_card_input_output_question_texts_align() {
        // The frontend matches answers to questions by (header, question); with
        // header empty on both sides, the question text must match verbatim so the
        // capsule shows the pick rather than "未选择". `build_outcome` copies the
        // spec's question into the answer, so a full round trip stays aligned.
        let specs = parse_grok_ext_questions(&grok_params(json!([{
            "question": "Pick one",
            "options": [
                { "label": "A", "description": "" },
                { "label": "B", "description": "" }
            ]
        }])))
        .unwrap();
        let outcome = build_outcome(
            &specs,
            &QuestionAnswer {
                answers: vec![QuestionAnswerItem {
                    question_id: specs[0].id.clone(),
                    labels: vec!["A".into()],
                }],
                declined: false,
            },
        );
        let input = grok_result_card_input(&specs);
        let output = grok_result_card_output(&outcome);
        assert_eq!(input["questions"][0]["question"], output["answers"][0]["question"]);
        assert_eq!(output["answers"][0]["header"], "");
    }
