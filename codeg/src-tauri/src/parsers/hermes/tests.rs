    use super::*;

    #[test]
    fn resolve_hermes_home_matches_acp_semantics() {
        let home = Some(PathBuf::from("/Users/demo"));
        assert_eq!(
            resolve_hermes_home(None, home.clone()),
            PathBuf::from("/Users/demo/.hermes")
        );
        assert_eq!(
            resolve_hermes_home(Some(String::new()), home.clone()),
            PathBuf::from("/Users/demo/.hermes")
        );
        assert_eq!(
            resolve_hermes_home(Some("   ".to_string()), home.clone()),
            PathBuf::from("/Users/demo/.hermes")
        );
        // A tilde is NOT expanded, because Hermes does not expand it: its own
        // `get_hermes_home` (`hermes_constants.py`) is
        // `Path(os.environ["HERMES_HOME"].strip())`, so it creates and uses a
        // directory literally named `~`. This used to answer `/Users/demo` and
        // `/Users/demo/work` — directories Hermes never opens — which is why
        // the session list came back empty for anyone who set it that way,
        // while `commands::acp::hermes_home_for_launch` (the same resolution,
        // written from the same source) disagreed with this one all along.
        assert_eq!(
            resolve_hermes_home(Some("~".to_string()), home.clone()),
            PathBuf::from("~")
        );
        assert_eq!(
            resolve_hermes_home(Some("~/work".to_string()), home.clone()),
            PathBuf::from("~/work")
        );
        assert_eq!(
            resolve_hermes_home(Some("/custom/hermes".to_string()), home),
            PathBuf::from("/custom/hermes")
        );
        // Unknown home dir falls back to ".".
        assert_eq!(
            resolve_hermes_home(None, None),
            PathBuf::from(".").join(".hermes")
        );
    }

    #[test]
    fn secs_f64_to_datetime_handles_fractional_and_invalid() {
        let dt = secs_f64_to_datetime(1_780_980_974.5);
        assert_eq!(dt.timestamp(), 1_780_980_974);
        assert_eq!(dt.timestamp_subsec_millis(), 500);

        // Invalid inputs must not panic.
        let _ = secs_f64_to_datetime(0.0);
        let _ = secs_f64_to_datetime(-1.0);
        let _ = secs_f64_to_datetime(f64::NAN);
        let _ = secs_f64_to_datetime(f64::INFINITY);
    }

    #[test]
    fn secs_f64_to_datetime_carries_rounding_overflow() {
        // A fraction that rounds to 1e9 nanoseconds must carry into the next
        // whole second rather than producing a leap second or falling back to now.
        let dt = secs_f64_to_datetime(1_000.999_999_999_9);
        assert_eq!(dt.timestamp(), 1_001);
        assert_eq!(dt.timestamp_subsec_nanos(), 0);
    }

    #[test]
    fn decode_plain_text_content() {
        let blocks = decode_hermes_content(Some("hello hermes"));
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "hello hermes"));
        assert!(decode_hermes_content(Some("   ")).is_empty());
        assert!(decode_hermes_content(None).is_empty());
    }

    #[test]
    fn decode_multimodal_content_text_and_image() {
        let raw = format!(
            "{CONTENT_JSON_PREFIX}{}",
            r#"[{"type":"text","text":"look:"},{"type":"image_url","image_url":{"url":"data:image/png;base64,QUJD"}}]"#
        );
        let blocks = decode_hermes_content(Some(&raw));
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "look:"));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Image { data, mime_type, uri }
            if data == "QUJD" && mime_type == "image/png" && uri.is_none()
        ));
    }

    #[test]
    fn decode_non_data_image_url_keeps_text_reference() {
        let raw = format!(
            "{CONTENT_JSON_PREFIX}{}",
            r#"[{"type":"image_url","image_url":{"url":"https://example.com/a.png"}}]"#
        );
        let blocks = decode_hermes_content(Some(&raw));
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "[image] https://example.com/a.png"));
    }

    #[test]
    fn decode_malformed_json_after_prefix_falls_back_to_text() {
        let raw = format!("{CONTENT_JSON_PREFIX}not json at all");
        let blocks = decode_hermes_content(Some(&raw));
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "not json at all"));
    }

    /// v0.21.6 stores a literal message that starts with the sentinel as the
    /// sentinel plus a JSON string (what `json.dumps` writes for it). The
    /// literal below spells out a parts array; it must come back as that text,
    /// not as the parts it imitates.
    #[test]
    fn decode_an_escaped_sentinel_literal_as_its_text() {
        let literal = format!(r#"{CONTENT_JSON_PREFIX}[{{"type":"text","text":"smuggled"}}]"#);
        let stored = format!(
            "{CONTENT_JSON_PREFIX}{}",
            serde_json::to_string(&literal).unwrap()
        );
        let blocks = decode_hermes_content(Some(&stored));
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(&blocks[0], ContentBlock::Text { text } if text == r#"json:[{"type":"text","text":"smuggled"}]"#),
            "{blocks:?}"
        );
    }

    #[test]
    fn secs_f64_to_datetime_opt_rejects_what_the_fallback_would_call_now() {
        assert_eq!(
            secs_f64_to_datetime_opt(1_780_980_974.5).map(|dt| dt.timestamp()),
            Some(1_780_980_974)
        );
        for unusable in [0.0, -1.0, f64::NAN, f64::INFINITY, 8.4e252] {
            assert_eq!(secs_f64_to_datetime_opt(unusable), None, "{unusable}");
        }
    }

    #[test]
    fn normalize_tool_arguments_string_vs_object() {
        assert_eq!(
            normalize_tool_arguments(&serde_json::json!("{\"a\":1}")),
            Some("{\"a\":1}".to_string())
        );
        assert_eq!(
            normalize_tool_arguments(&serde_json::json!({"a": 1})),
            Some("{\"a\":1}".to_string())
        );
        assert_eq!(normalize_tool_arguments(&serde_json::Value::Null), None);
        assert_eq!(normalize_tool_arguments(&serde_json::json!("  ")), None);
    }

    #[test]
    fn parse_tool_calls_extracts_id_name_args() {
        let raw = r#"[
            {"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}},
            {"id":"call_2","type":"function","function":{"name":"patch","arguments":{"path":"b.txt"}}}
        ]"#;
        let calls = parse_hermes_tool_calls(raw);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0.as_deref(), Some("call_1"));
        assert_eq!(calls[0].1, "read_file");
        assert_eq!(calls[0].2.as_deref(), Some("{\"path\":\"a.txt\"}"));
        assert_eq!(calls[1].1, "patch");
        assert_eq!(calls[1].2.as_deref(), Some("{\"path\":\"b.txt\"}"));

        assert!(parse_hermes_tool_calls("not json").is_empty());
        assert!(parse_hermes_tool_calls("{}").is_empty());
    }

    #[test]
    fn content_to_text_reduces_multimodal_to_text() {
        let raw = format!(
            "{CONTENT_JSON_PREFIX}{}",
            r#"[{"type":"text","text":"out"},{"type":"image_url","image_url":{"url":"data:image/png;base64,QUJD"}}]"#
        );
        assert_eq!(content_to_text(Some(&raw)).as_deref(), Some("out\n[image]"));
        assert_eq!(content_to_text(Some("plain")).as_deref(), Some("plain"));
        assert_eq!(content_to_text(Some("  ")), None);
        assert_eq!(content_to_text(None), None);
    }
