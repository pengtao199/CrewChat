    use super::*;

    fn test_claude_servers() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Ok(BTreeMap::from([
            (
                "claude-only".to_string(),
                json!({"type": "stdio", "command": "claude-only"}),
            ),
            (
                "shared".to_string(),
                json!({"type": "stdio", "command": "claude-wins"}),
            ),
        ]))
    }

    fn test_gemini_servers() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Ok(BTreeMap::from([
            (
                "gemini-only".to_string(),
                json!({"type": "stdio", "command": "gemini-only"}),
            ),
            (
                "shared".to_string(),
                json!({"type": "stdio", "command": "gemini-loses"}),
            ),
        ]))
    }

    fn test_broken_antigravity_servers() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Err(mcp_configuration_invalid("broken Antigravity fixture"))
    }

    fn test_openclaw_servers() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Ok(BTreeMap::from([(
            "shared-remote".to_string(),
            json!({
                "type": "http",
                "url": "https://example.test/mcp",
                "auth": "oauth"
            }),
        )]))
    }

    fn test_kimi_servers() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Ok(BTreeMap::from([(
            "shared-remote".to_string(),
            json!({
                "type": "http",
                "url": "https://example.test/mcp",
                "bearerTokenEnvVar": "MCP_TOKEN"
            }),
        )]))
    }

    #[test]
    fn pi_mcp_source_is_registered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        let raw = json!({"mcpServers": {
            "shared": {"command": "pi-shared"},
            "pi-only": {"command": "npx", "args": ["-y", "test-mcp"],
                        "env": {"TEST_KEY": "fixture"}},
            "remote": {"url": "https://example.test/mcp", "headers": {"X-Test": "fixture"}},
            "invalid": {"command": ""}
        }})
        .to_string();
        fs::write(&path, &raw).unwrap();
        temp_env::with_var("PI_CODING_AGENT_DIR", Some(dir.path()), || {
            let pi = local_mcp_readers()
                .into_iter()
                .find(|reader| serde_json::to_value(reader.app).unwrap() == json!("pi"))
                .expect("pi must be a registered scan source");
            let scan = scan_local_servers_from_readers(&[
                LocalMcpReader::new("Claude Code", McpAppType::ClaudeCode, test_claude_servers),
                pi,
            ]);
            assert!(scan.warnings.is_empty());
            assert_eq!(scan.servers.len(), 4);
            let shared = scan
                .servers
                .iter()
                .find(|server| server.id == "shared")
                .unwrap();
            assert_eq!(shared.apps, [McpAppType::ClaudeCode, McpAppType::Pi]);
            assert_eq!(shared.spec["command"], "claude-wins");
            let local = scan
                .servers
                .iter()
                .find(|server| server.id == "pi-only")
                .unwrap();
            assert_eq!(local.apps, [McpAppType::Pi]);
            assert_eq!(local.spec["type"], "stdio");
            assert_eq!(local.spec["args"], json!(["-y", "test-mcp"]));
            assert_eq!(local.spec["env"]["TEST_KEY"], "fixture");
            let remote = scan
                .servers
                .iter()
                .find(|server| server.id == "remote")
                .unwrap();
            assert_eq!(remote.spec["type"], "http");
            assert_eq!(remote.spec["headers"]["X-Test"], "fixture");
            // Discovery must neither rewrite the extension's file nor change ACP forwarding.
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
            assert!(
                read_servers_for_agent_type(crate::models::agent::AgentType::Pi)
                    .unwrap()
                    .is_empty()
            );
        });
    }

    #[test]
    fn pi_mcp_missing_empty_and_invalid_configs_follow_scan_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        assert!(read_pi_servers_at(&path).unwrap().is_empty());
        assert!(!remove_pi_server_at(&path, "missing").unwrap());
        assert!(!path.exists());
        for raw in ["", "  \n", "{}", r#"{"mcpServers":{}}"#] {
            fs::write(&path, raw).unwrap();
            assert!(read_pi_servers_at(&path).unwrap().is_empty());
        }
        fs::write(&path, "{broken").unwrap();
        temp_env::with_var("PI_CODING_AGENT_DIR", Some(dir.path()), || {
            let scan = scan_local_servers_from_readers(&[
                LocalMcpReader::new("pi", McpAppType::Pi, read_pi_servers),
                LocalMcpReader::new("Claude Code", McpAppType::ClaudeCode, test_claude_servers),
            ]);
            assert_eq!(scan.servers.len(), 2);
            assert_eq!(scan.warnings.len(), 1);
            assert_eq!(scan.warnings[0].app, McpAppType::Pi);
            assert!(scan.warnings[0].message.contains(path.to_str().unwrap()));
            assert!(require_complete_scan(&scan).is_err());
            assert!(
                upsert_server_for_app(McpAppType::Pi, "test", &json!({"command": "test"})).is_err()
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "{broken");
        });
    }

    #[test]
    fn pi_mcp_edit_and_remove_preserve_other_extension_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        let original = json!({
            "settings": {"toolPrefix": "mcp"},
            "mcpServers": {
                "existing": {"command": "keep-me", "extensionOption": true},
                "editable": {"command": "before", "extensionOption": {"enabled": true}}
            }
        });
        fs::write(&path, original.to_string()).unwrap();
        temp_env::with_var("PI_CODING_AGENT_DIR", Some(dir.path()), || {
            let mut spec = read_pi_servers().unwrap().remove("editable").unwrap();
            spec["command"] = json!("after");
            upsert_server_for_app(McpAppType::Pi, "editable", &spec).unwrap();
            let edited = read_json_file(&path).unwrap();
            assert_eq!(edited["settings"], original["settings"]);
            assert_eq!(
                edited["mcpServers"]["existing"],
                original["mcpServers"]["existing"]
            );
            assert_eq!(edited["mcpServers"]["editable"]["command"], "after");
            assert_eq!(
                edited["mcpServers"]["editable"]["extensionOption"],
                json!({"enabled": true})
            );
            assert!(remove_server_for_app(McpAppType::Pi, "editable").unwrap());
            assert!(!remove_server_for_app(McpAppType::Pi, "editable").unwrap());
            let removed = read_json_file(&path).unwrap();
            assert_eq!(removed["settings"], original["settings"]);
            assert_eq!(
                removed["mcpServers"]["existing"],
                original["mcpServers"]["existing"]
            );
            assert!(removed["mcpServers"].get("editable").is_none());
        });
    }

    #[test]
    fn best_effort_scan_keeps_valid_sources_around_a_failed_source() {
        let readers = [
            LocalMcpReader::new("Claude Code", McpAppType::ClaudeCode, test_claude_servers),
            LocalMcpReader::new(
                "Antigravity",
                McpAppType::Antigravity,
                test_broken_antigravity_servers,
            ),
            LocalMcpReader::new("Gemini", McpAppType::Gemini, test_gemini_servers),
        ];

        let scan = scan_local_servers_from_readers(&readers);

        assert_eq!(
            scan.servers
                .iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["claude-only", "gemini-only", "shared"]
        );
        let shared = scan
            .servers
            .iter()
            .find(|server| server.id == "shared")
            .expect("shared server");
        assert_eq!(shared.spec["command"], "claude-wins");
        assert_eq!(shared.apps, [McpAppType::ClaudeCode, McpAppType::Gemini]);

        // Dropping the source silently would leave the user staring at an
        // Antigravity column that is simply blank, with nothing to act on.
        assert_eq!(
            scan.warnings
                .iter()
                .map(|warning| warning.app)
                .collect::<Vec<_>>(),
            [McpAppType::Antigravity]
        );
    }

    #[test]
    fn a_failed_source_is_refused_by_the_write_guard() {
        let readers = [
            LocalMcpReader::new("Claude Code", McpAppType::ClaudeCode, test_claude_servers),
            LocalMcpReader::new(
                "Antigravity",
                McpAppType::Antigravity,
                test_broken_antigravity_servers,
            ),
            LocalMcpReader::new("Gemini", McpAppType::Gemini, test_gemini_servers),
        ];

        // The list survives the broken source; a reassignment computed from that
        // same list must not, or Antigravity is dropped from the "keep it here"
        // set purely because codeg could not read it.
        let err = require_complete_scan(&scan_local_servers_from_readers(&readers))
            .expect_err("writers must stay fail-closed on a degraded scan");
        assert!(
            err.message.contains("Antigravity"),
            "the refusal has to name the file to go fix: {}",
            err.message
        );
    }

    #[test]
    fn empty_antigravity_file_reads_as_no_servers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp_config.json");
        std::fs::write(&path, "").expect("seed empty config");

        // Issue #632 verbatim: Antigravity touches this file into existence
        // before it ever writes a server, and a 0-byte config means "nothing
        // configured", not "corrupt". Treating it as a reader error deadlocks
        // the user — every writer starts by reading the file it is about to
        // change, so nothing can ever put content into it again.
        assert_eq!(
            read_antigravity_servers_at(&path).expect("empty config is not an error"),
            BTreeMap::new()
        );
    }

    #[test]
    fn successful_scan_preserves_openclaw_and_kimi_merge_rules() {
        let readers = [
            LocalMcpReader::new("OpenClaw", McpAppType::OpenClaw, test_openclaw_servers),
            LocalMcpReader::new("Kimi Code", McpAppType::KimiCode, test_kimi_servers),
        ];

        let scan = scan_local_servers_from_readers(&readers);

        assert!(
            scan.warnings.is_empty(),
            "readable sources must not warn: {:?}",
            scan.warnings
        );
        let servers = scan.servers;
        assert_eq!(servers.len(), 1);
        let shared = &servers[0];
        assert_eq!(shared.id, "shared-remote");
        assert_eq!(shared.spec["auth"], "oauth");
        assert_eq!(shared.spec["bearerTokenEnvVar"], "MCP_TOKEN");
        assert_eq!(shared.apps, [McpAppType::OpenClaw, McpAppType::KimiCode]);
    }

    #[test]
    fn normalize_mcp_type_canonical_pass_through() {
        assert_eq!(normalize_mcp_type("stdio"), Some("stdio"));
        assert_eq!(normalize_mcp_type("http"), Some("http"));
        assert_eq!(normalize_mcp_type("sse"), Some("sse"));
        assert_eq!(normalize_mcp_type("local"), Some("local"));
        assert_eq!(normalize_mcp_type("remote"), Some("remote"));
    }

    #[test]
    fn normalize_mcp_type_streamable_http_aliases_collapse_to_http() {
        for raw in [
            "streamable-http",
            "streamableHttp",
            "streamable_http",
            "Streamable HTTP",
            "STREAMABLE-HTTP",
            "  streamable-http  ",
            "streamable.http",
        ] {
            assert_eq!(normalize_mcp_type(raw), Some("http"), "input {raw:?}");
        }
    }

    #[test]
    fn normalize_mcp_type_rejects_unknown() {
        assert!(normalize_mcp_type("").is_none());
        assert!(normalize_mcp_type("   ").is_none());
        assert!(normalize_mcp_type("Foo").is_none());
        assert!(normalize_mcp_type("ws").is_none());
    }

    #[test]
    fn kimi_code_mcp_json_round_trips() {
        // Kimi reads `<KIMI_CODE_HOME>/mcp.json` (`mcpServers`) natively; verify
        // the read/upsert/remove cycle against an isolated path.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");

        // Missing file → no servers, and removing is a no-op.
        assert!(read_kimi_code_servers_at(&path)
            .expect("read missing")
            .is_empty());
        assert!(!remove_kimi_code_server_at(&path, "ctx7").expect("remove missing"));

        // Upsert a stdio server.
        let spec = json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "ctx7-mcp"],
        });
        upsert_kimi_code_server_at(&path, "ctx7", &spec).expect("upsert");

        // It round-trips, canonicalized, under `mcpServers`.
        let servers = read_kimi_code_servers_at(&path).expect("read back");
        assert_eq!(servers.len(), 1);
        let stored = servers.get("ctx7").expect("ctx7 present");
        assert_eq!(stored.get("type").and_then(Value::as_str), Some("stdio"));
        assert_eq!(stored.get("command").and_then(Value::as_str), Some("npx"));

        // On-disk shape is `{ "mcpServers": { "ctx7": { .. } } }`.
        let raw = std::fs::read_to_string(&path).expect("read file");
        let root: Value = serde_json::from_str(&raw).expect("parse json");
        assert!(root
            .get("mcpServers")
            .and_then(Value::as_object)
            .map(|m| m.contains_key("ctx7"))
            .unwrap_or(false));

        // Remove it; the file no longer lists it and a second remove is a no-op.
        assert!(remove_kimi_code_server_at(&path, "ctx7").expect("remove"));
        assert!(read_kimi_code_servers_at(&path)
            .expect("read after remove")
            .is_empty());
        assert!(!remove_kimi_code_server_at(&path, "ctx7").expect("remove again"));
    }

    #[test]
    fn cursor_mcp_json_round_trips_and_strips_type() {
        // Cursor reads `~/.cursor/mcp.json` (`mcpServers`) natively, shape-
        // discriminated (command ⇒ stdio, url ⇒ remote) with NO `type` key —
        // the writer must emit only the fields Cursor models, and the reader
        // must re-infer transport rather than trusting a foreign `type`.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");

        assert!(read_cursor_servers_at(&path).expect("read missing").is_empty());
        assert!(!remove_cursor_server_at(&path, "ctx7").expect("remove missing"));

        // Upsert a stdio server; the canonical `type` must not reach disk.
        let spec = json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "ctx7-mcp"],
            "env": { "TOKEN": "t" },
        });
        upsert_cursor_server_at(&path, "ctx7", &spec).expect("upsert");
        let raw = std::fs::read_to_string(&path).expect("read file");
        let root: Value = serde_json::from_str(&raw).expect("parse json");
        let on_disk = root.pointer("/mcpServers/ctx7").expect("entry on disk");
        assert!(on_disk.get("type").is_none(), "no type key on disk");
        assert_eq!(on_disk.get("command").and_then(Value::as_str), Some("npx"));

        // Read-back canonicalizes (command ⇒ stdio).
        let servers = read_cursor_servers_at(&path).expect("read back");
        assert_eq!(
            servers.get("ctx7").and_then(|s| s.get("type")).and_then(Value::as_str),
            Some("stdio")
        );

        // A remote entry keeps url/headers only; a foreign on-disk `type` is
        // ignored on read (shape wins, like the CLI's Zod parse).
        upsert_cursor_server_at(
            &path,
            "remote",
            &json!({"type": "sse", "url": "https://mcp.example.com/sse"}),
        )
        .expect("upsert remote");
        let raw2 = std::fs::read_to_string(&path).expect("read file 2");
        let root2: Value = serde_json::from_str(&raw2).expect("parse json 2");
        assert!(root2.pointer("/mcpServers/remote/type").is_none());
        let servers2 = read_cursor_servers_at(&path).expect("read back 2");
        assert_eq!(
            servers2.get("remote").and_then(|s| s.get("type")).and_then(Value::as_str),
            Some("http"),
            "url-only entries classify as http (Cursor auto-negotiates)"
        );

        // Remove round-trips.
        assert!(remove_cursor_server_at(&path, "ctx7").expect("remove"));
        assert!(remove_cursor_server_at(&path, "remote").expect("remove remote"));
        assert!(read_cursor_servers_at(&path).expect("read after remove").is_empty());
    }

    #[test]
    fn deepseek_mcp_json_round_trips_the_canonical_spec() {
        // `$DSH_HOME/mcp.json` is codeg's OWN store (deepseek-acp reads no MCP
        // file; the wire is the delivery path), so unlike every other agent it
        // keeps the canonical spec verbatim — `type` included. Round-tripping
        // it unchanged is what lets `load_mcp_servers_for_agent` map the entry
        // to the ACP schema without a second guess at the transport.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");

        assert!(read_deepseek_servers_at(&path)
            .expect("read missing")
            .is_empty());
        assert!(!remove_deepseek_server_at(&path, "ctx7").expect("remove missing"));

        upsert_deepseek_server_at(
            &path,
            "ctx7",
            &json!({
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "ctx7-mcp"],
                "env": { "TOKEN": "t" },
            }),
        )
        .expect("upsert");
        // A url-only entry canonicalizes to streamable HTTP — the one remote
        // transport the bridge mounts.
        upsert_deepseek_server_at(
            &path,
            "remote",
            &json!({ "url": "https://mcp.example.com/mcp" }),
        )
        .expect("upsert remote");

        let servers = read_deepseek_servers_at(&path).expect("read back");
        assert_eq!(
            servers
                .get("ctx7")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str),
            Some("stdio")
        );
        assert_eq!(
            servers
                .get("ctx7")
                .and_then(|s| s.get("command"))
                .and_then(Value::as_str),
            Some("npx")
        );
        assert_eq!(
            servers
                .get("remote")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str),
            Some("http")
        );
        // The type IS persisted here (no foreign schema to strip it for).
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read file"))
                .expect("parse json");
        assert_eq!(
            root.pointer("/mcpServers/ctx7/type").and_then(Value::as_str),
            Some("stdio")
        );

        // SSE is rejected for DeepSeek the same way it is for Codex: the
        // bridge fails `session/new` on an unsupported transport rather than
        // skipping the entry, so it must never be assigned in the first place.
        let sse = json!({ "type": "sse", "url": "https://mcp.example.com/sse" });
        assert!(!app_can_host_spec(McpAppType::DeepSeek, &sse));
        assert!(app_can_host_spec(
            McpAppType::DeepSeek,
            &json!({ "type": "http", "url": "https://mcp.example.com/mcp" })
        ));

        assert!(remove_deepseek_server_at(&path, "ctx7").expect("remove"));
        assert!(remove_deepseek_server_at(&path, "remote").expect("remove remote"));
        assert!(read_deepseek_servers_at(&path)
            .expect("read after remove")
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn deepseek_mcp_store_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;

        // codeg CREATES this file (no other agent's store works that way), and
        // a stdio entry's `env` carries the server's token — so a fresh file
        // must not inherit the umask's `0644`.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("mcp.json");
        upsert_deepseek_server_at(
            &path,
            "ctx7",
            &json!({ "command": "npx", "env": { "TOKEN": "super-secret" } }),
        )
        .expect("upsert");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o077, 0, "fresh store must be owner-only, got {mode:o}");
        let parent_mode = std::fs::metadata(path.parent().expect("parent"))
            .expect("stat parent")
            .permissions()
            .mode();
        assert_eq!(
            parent_mode & 0o077,
            0,
            "created parent must be owner-only, got {parent_mode:o}"
        );

        // An existing world-readable file (older build, hand-edited) is
        // repaired on the next write; a group-shared `0640` is left alone.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        upsert_deepseek_server_at(&path, "ctx7", &json!({ "command": "npx" })).expect("re-upsert");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o007,
            0,
            "world bits must be cleared"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod 640");
        upsert_deepseek_server_at(&path, "ctx7", &json!({ "command": "npx" })).expect("re-upsert 2");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o640,
            "a deliberate group-shared mode is preserved"
        );
    }

    #[test]
    fn antigravity_mcp_config_handles_both_document_shapes() {
        // `mcp_servers.py::parse_mcp_config_file` does
        // `servers_dict = data.get("mcpServers", data)` — a BARE top-level map
        // is a first-class shape, and the fallback is winner-take-all. So
        // adding an `mcpServers` wrapper next to bare entries would unmount
        // every one of them. They have to be migrated INTO the wrapper.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp_config.json");

        // A user's existing BARE document.
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "ctx7": { "command": "npx", "args": ["-y", "ctx7"] },
                "remote": { "url": "https://example.test/mcp" }
            }))
            .unwrap(),
        )
        .expect("seed bare config");

        // Both bare entries are visible before any write.
        let seen = read_antigravity_servers_at(&path).expect("read bare");
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen.contains_key("ctx7") && seen.contains_key("remote"));

        upsert_antigravity_server_at(&path, "added", &json!({ "command": "added-bin" }))
            .expect("upsert");

        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        let servers = root
            .get("mcpServers")
            .and_then(Value::as_object)
            .expect("wrapper created");
        // The two pre-existing servers moved INTO the wrapper — the agent still
        // sees exactly the set it saw before, plus the new one.
        for id in ["ctx7", "remote", "added"] {
            assert!(servers.contains_key(id), "{id} lost: {root}");
        }
        assert!(
            root.as_object().unwrap().len() == 1,
            "nothing may be left stranded at the root: {root}"
        );

        // Removal reaches a migrated entry too.
        assert!(remove_antigravity_server_at(&path, "ctx7").expect("remove"));
        let after = read_antigravity_servers_at(&path).expect("read back");
        assert!(!after.contains_key("ctx7"));
        assert_eq!(after.len(), 2);

        // A MALFORMED `mcpServers` is not something to migrate around. The
        // loader hands that value to its `isinstance` check and rejects the
        // WHOLE file — it does NOT fall back to the root — so codeg must
        // report no servers, and must refuse to edit rather than delete a key
        // whose meaning it cannot read.
        let malformed = dir.path().join("malformed.json");
        let original = r#"{"mcpServers":"sentinel","legacy":{"command":"legacy-bin"}}"#;
        std::fs::write(&malformed, original).expect("seed malformed");

        let seen = read_antigravity_servers_at(&malformed).expect("read malformed");
        assert!(
            seen.is_empty(),
            "the agent sees nothing here, so neither may codeg: {seen:?}"
        );
        assert!(
            upsert_antigravity_server_at(&malformed, "x", &json!({ "command": "x" })).is_err(),
            "editing must refuse rather than drop `mcpServers`"
        );
        assert!(!remove_antigravity_server_at(&malformed, "legacy").expect("remove is a no-op"));
        assert_eq!(
            std::fs::read_to_string(&malformed).unwrap(),
            original,
            "the file must be byte-identical after a refused edit"
        );

        // The preflight the multi-app commands run before touching ANY agent
        // must agree exactly with what the writer will accept — otherwise a
        // save either aborts for nothing, or gets halfway through and then
        // discovers it cannot finish.
        for (doc, blocks) in [
            (json!({ "mcpServers": "sentinel" }), true),
            (json!({ "mcpServers": ["a"] }), true),
            (json!({ "mcpServers": { "a": { "command": "a" } } }), false),
            (json!({ "bare": { "command": "b" } }), false),
            (json!({}), false),
        ] {
            assert_eq!(
                antigravity_config_blocks_edit(&doc),
                blocks,
                "preflight disagrees about {doc}"
            );
            let mut editable = doc.clone();
            assert_eq!(
                antigravity_servers_object(&mut editable).is_err(),
                blocks,
                "writer disagrees with the preflight about {doc}"
            );
        }

        // A refused preflight must mean ZERO mutations, not a walk that stops
        // partway. `mcp_upsert_local_server` removes the server from every
        // non-target agent inside that walk, so a late failure would strip it
        // from all of them and add it nowhere.
        let mut mutated = false;
        let refused = with_preflight(
            || Err(mcp_configuration_invalid("nope")),
            || {
                mutated = true;
                Ok(())
            },
        );
        assert!(refused.is_err());
        assert!(
            !mutated,
            "a refused preflight must not have run a single mutation"
        );

        // The approving direction still runs it, so the guard is not simply
        // blocking everything.
        let mut ran = false;
        with_preflight(
            || Ok(()),
            || {
                ran = true;
                Ok(())
            },
        )
        .expect("an approving preflight lets the mutation through");
        assert!(ran);

        // An agent that is not a target is never preflighted — a malformed
        // Antigravity config must not block a save that never mentions it.
        let mut other_ran = false;
        with_upsert_preflight(&BTreeSet::from([McpAppType::ClaudeCode]), || {
            other_ran = true;
            Ok(())
        })
        .expect("an untargeted Antigravity cannot block another agent's save");
        assert!(other_ran);

        // And the already-wrapped shape keeps working, foreign keys intact.
        let wrapped = dir.path().join("wrapped.json");
        std::fs::write(
            &wrapped,
            serde_json::to_string_pretty(&json!({
                "mcpServers": { "a": { "command": "a-bin" } },
                "someOtherKey": { "kept": true }
            }))
            .unwrap(),
        )
        .expect("seed wrapped");
        upsert_antigravity_server_at(&wrapped, "b", &json!({ "command": "b-bin" }))
            .expect("upsert wrapped");
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&wrapped).unwrap()).expect("parse");
        assert_eq!(root["someOtherKey"]["kept"], true);
        assert!(root["mcpServers"].get("a").is_some());
        assert!(root["mcpServers"].get("b").is_some());
    }

    #[test]
    fn empty_json_config_reads_as_no_servers() {
        // Issue #632: a 0-byte `~/.gemini/config/mcp_config.json` made serde
        // report `EOF while parsing a value at line 1 column 0`, which the
        // reader raised as a hard configuration error. An empty file is
        // "nothing configured", exactly like an absent one.
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, body) in [("empty.json", ""), ("blank.json", "  \r\n\t ")] {
            let path = dir.path().join(name);
            std::fs::write(&path, body).expect("seed file");

            assert_eq!(read_json_file(&path).expect(name), json!({}));
            assert!(read_antigravity_servers_at(&path).expect(name).is_empty());
            assert!(read_cursor_servers_at(&path).expect(name).is_empty());
            assert!(read_kimi_code_servers_at(&path).expect(name).is_empty());
            assert!(read_qoder_servers_at(&path).expect(name).is_empty());
            assert!(read_deepseek_servers_at(&path).expect(name).is_empty());

            // Writing into one still works — there is nothing in an empty file
            // to preserve, so the save must not refuse either.
            upsert_antigravity_server_at(&path, "ctx7", &json!({ "command": "npx" }))
                .expect("upsert into an empty config");
            assert!(read_antigravity_servers_at(&path)
                .expect("read back")
                .contains_key("ctx7"));
        }

        // A file with actual content that is not JSON is still an error: codeg
        // must not overwrite something the user wrote and it failed to read.
        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "{ not json").expect("seed broken");
        assert!(read_json_file(&broken).is_err());
    }

    #[test]
    fn a_config_that_cannot_be_stat_ed_is_an_error_not_an_empty_one() {
        // Absence is decided by the read, not by `Path::exists()`: `exists()`
        // answers `false` for any failed stat, so a config codeg cannot open
        // would be reported as "this agent has none" — silently, with no
        // warning for `require_complete_scan` to refuse a reassignment on.
        let dir = tempfile::tempdir().expect("tempdir");

        let absent = dir.path().join("absent.json");
        assert!(read_config_to_string(&absent)
            .expect("absent is Ok")
            .is_none());
        assert_eq!(read_json_file(&absent).expect("absent"), json!({}));

        // A symlink loop: present enough to matter, unreadable, and `exists()`
        // reports it as absent. (Only the construction is unix-specific; the
        // rule it pins is not.)
        #[cfg(unix)]
        {
            let loop_a = dir.path().join("loop-a.json");
            let loop_b = dir.path().join("loop-b.json");
            std::os::unix::fs::symlink(&loop_b, &loop_a).expect("link a->b");
            std::os::unix::fs::symlink(&loop_a, &loop_b).expect("link b->a");
            assert!(!loop_a.exists(), "exists() cannot tell this from absent");

            let err = read_config_to_string(&loop_a).expect_err("unreadable");
            // The kind alone ("Permission denied", "I/O operation failed")
            // names no file, so the path has to survive into the detail or the
            // scan warning tells the user nothing they can act on.
            assert!(
                err.detail
                    .as_deref()
                    .is_some_and(|d| d.contains(&loop_a.display().to_string())),
                "{err:?}"
            );

            assert!(read_json_file(&loop_a).is_err(), "must not read as {{}}");
            assert!(read_hermes_servers_at(&loop_a).is_err());
            assert!(read_grok_root_toml_at(&loop_a).is_err());
        }
    }

    fn test_one_readable_server() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Ok(BTreeMap::from([(
            "ctx7".to_string(),
            json!({ "command": "npx" }),
        )]))
    }

    fn test_parse_failure() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Err(mcp_configuration_invalid(
            "invalid JSON at /x/mcp_config.json: boom",
        ))
    }

    fn test_io_failure() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Err(AppCommandError::io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "permission denied (os error 13)",
        )))
    }

    fn test_late_server() -> Result<BTreeMap<String, Value>, AppCommandError> {
        Ok(BTreeMap::from([(
            "later".to_string(),
            json!({ "command": "later-bin" }),
        )]))
    }

    #[test]
    fn one_unreadable_source_warns_instead_of_emptying_the_scan() {
        // Issue #632: `scan_local_servers` read every agent behind a fail-fast
        // `?`, so a single broken config hid EVERY agent's servers behind one
        // "load failed" banner. A bad source must degrade to a warning.
        let scan = scan_local_servers_from_readers(&[
            LocalMcpReader::new("Claude Code", McpAppType::ClaudeCode, test_one_readable_server),
            LocalMcpReader::new("Antigravity", McpAppType::Antigravity, test_parse_failure),
            LocalMcpReader::new("Cursor", McpAppType::Cursor, test_io_failure),
            LocalMcpReader::new("Codex", McpAppType::Codex, test_late_server),
        ]);

        // Readable sources on BOTH sides of the failures still land.
        assert_eq!(
            scan.servers
                .iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["ctx7", "later"]
        );
        assert_eq!(
            scan.warnings
                .iter()
                .map(|warning| warning.app)
                .collect::<Vec<_>>(),
            [McpAppType::Antigravity, McpAppType::Cursor]
        );

        // The reader's own message names the offending file, so the user can go
        // fix it rather than just seeing that agent's servers go missing.
        assert!(
            scan.warnings[0].message.contains("/x/mcp_config.json"),
            "{:?}",
            scan.warnings[0].message
        );

        // An I/O failure carries its specifics in `detail` rather than in
        // `message`, so the warning has to fold both in or it reads as a bare
        // "Permission denied" with nothing to act on.
        assert!(
            scan.warnings[1].message.contains("os error 13"),
            "{:?}",
            scan.warnings[1].message
        );
    }

    #[test]
    fn hermes_reports_an_unreadable_config_instead_of_swallowing_it() {
        // Hermes used to answer `Ok(empty)` for a config.yaml it could not
        // parse, back when an `Err` would have aborted the whole scan. Now that
        // a failure only warns, swallowing it would leave Hermes missing from a
        // scan that still reports itself complete — and `require_complete_scan`
        // would wave through a reassignment that strips the server from the
        // agents that DID load and then fails on the Hermes write.
        let dir = tempfile::tempdir().expect("tempdir");

        // Absent and empty stay "no Hermes servers": most machines have none.
        let missing = dir.path().join("nope.yaml");
        assert!(read_hermes_servers_at(&missing).expect("absent").is_empty());
        let empty = dir.path().join("empty.yaml");
        std::fs::write(&empty, "  \n").expect("seed empty");
        assert!(read_hermes_servers_at(&empty).expect("empty").is_empty());

        // Unparseable is an error that names the file.
        let broken = dir.path().join("config.yaml");
        std::fs::write(&broken, "mcp_servers: [unclosed\n").expect("seed broken");
        let err = read_hermes_servers_at(&broken).expect_err("invalid YAML");
        assert!(
            err.message.contains(&broken.display().to_string()),
            "{:?}",
            err.message
        );

        // A valid document still reads.
        std::fs::write(
            &broken,
            "model:\n  provider: openai\nmcp_servers:\n  ctx7:\n    command: npx\n",
        )
        .expect("seed valid");
        let servers = read_hermes_servers_at(&broken).expect("valid");
        assert!(servers.contains_key("ctx7"), "{servers:?}");
    }

    #[test]
    fn the_write_guard_accepts_a_clean_scan_and_refuses_a_degraded_one() {
        // Reading past an unreadable source is the whole point of #632. WRITING
        // past one is not: the app list a save is handed comes from the scan,
        // and every agent missing from it gets the server removed — so an agent
        // that is missing only because its file could not be read would be
        // stripped without the user ever unchecking it.
        //
        // This covers the guard itself. That the two write commands actually
        // CALL it before their first write is asserted by
        // `a_failed_source_is_refused_by_the_write_guard` plus the call sites.
        let clean = LocalMcpScan {
            servers: vec![LocalMcpServer {
                id: "ctx7".to_string(),
                spec: json!({ "command": "npx" }),
                apps: vec![McpAppType::ClaudeCode],
            }],
            warnings: Vec::new(),
        };
        require_complete_scan(&clean).expect("a complete scan may be written from");

        let degraded = LocalMcpScan {
            warnings: vec![LocalMcpSourceWarning {
                app: McpAppType::Antigravity,
                message: "invalid JSON at /x/mcp_config.json: boom".to_string(),
            }],
            ..clean
        };
        let err = require_complete_scan(&degraded).expect_err("a degraded scan may not");
        // The refusal has to name the file, or the user cannot clear the block.
        assert!(
            err.message.contains("Antigravity") && err.message.contains("/x/mcp_config.json"),
            "{:?}",
            err.message
        );
    }

    #[test]
    fn qoder_settings_json_round_trips_and_preserves_foreign_keys() {
        // `~/.qoder/settings.json` is QODER'S file, not codeg's: the CLI owns
        // unrelated keys beside `mcpServers` (`securityScan`, `security.auth`,
        // `model.name`, the `projects` map, …). Every write must be a
        // read-modify-write merge that leaves those keys intact — the property
        // that separates this writer from DeepSeek's own-store writer above.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");

        assert!(read_qoder_servers_at(&path)
            .expect("read missing")
            .is_empty());
        assert!(!remove_qoder_server_at(&path, "ctx7").expect("remove missing"));

        // A realistic settings.json: foreign keys plus one valid server and
        // one entry too malformed to canonicalize (exercises the skip path
        // on read — codeg must skip it, never rewrite the file to "fix" it).
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "securityScan": true,
                "security": { "auth": { "provider": "qoder-account" } },
                "model": { "name": "qmodel_38max" },
                "permissions": { "trustDirectories": true },
                "mcpServers": {
                    "existing": { "command": "uvx", "args": ["mcp-existing"] },
                    "broken": {}
                }
            }))
            .expect("seed json"),
        )
        .expect("seed settings.json");

        upsert_qoder_server_at(
            &path,
            "ctx7",
            &json!({
                "command": "npx",
                "args": ["-y", "ctx7-mcp"],
                "env": { "TOKEN": "t" },
            }),
        )
        .expect("upsert");
        // Qoder's verified handshake advertises http+sse — a url-only entry
        // canonicalizes to http, and neither remote transport is refused
        // (unlike Codex/DeepSeek in `app_can_host_spec`).
        upsert_qoder_server_at(
            &path,
            "remote",
            &json!({ "url": "https://mcp.example.com/mcp" }),
        )
        .expect("upsert remote");
        assert!(app_can_host_spec(
            McpAppType::Qoder,
            &json!({ "type": "sse", "url": "https://mcp.example.com/sse" })
        ));

        let servers = read_qoder_servers_at(&path).expect("read back");
        assert_eq!(servers.len(), 3, "valid entries only — broken is skipped");
        assert_eq!(
            servers
                .get("ctx7")
                .and_then(|s| s.get("command"))
                .and_then(Value::as_str),
            Some("npx")
        );
        assert_eq!(
            servers
                .get("remote")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str),
            Some("http")
        );
        assert_eq!(
            servers
                .get("existing")
                .and_then(|s| s.get("command"))
                .and_then(Value::as_str),
            Some("uvx")
        );

        // The merge promise: after both writes, every foreign key and the
        // skipped malformed entry are still on disk.
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read file"))
                .expect("parse json");
        assert_eq!(root.pointer("/securityScan"), Some(&json!(true)));
        assert_eq!(
            root.pointer("/model/name"),
            Some(&json!("qmodel_38max"))
        );
        assert_eq!(
            root.pointer("/permissions/trustDirectories"),
            Some(&json!(true))
        );
        assert_eq!(
            root.pointer("/mcpServers/existing/command"),
            Some(&json!("uvx"))
        );
        assert!(
            root.pointer("/mcpServers/broken").is_some(),
            "the malformed entry stays on disk — skipping it must not rewrite it away"
        );

        // Remove takes only the targeted server; a missing id returns false
        // without rewriting, and the foreign keys survive the final write too.
        assert!(remove_qoder_server_at(&path, "ctx7").expect("remove"));
        assert!(!remove_qoder_server_at(&path, "nope").expect("remove missing id"));
        let after = read_qoder_servers_at(&path).expect("read after remove");
        assert_eq!(after.len(), 2);
        assert!(after.contains_key("remote"));
        assert!(after.contains_key("existing"));
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("re-read"))
                .expect("re-parse");
        assert_eq!(root.pointer("/model/name"), Some(&json!("qmodel_38max")));
    }

    #[test]
    fn all_mcp_apps_is_exhaustive() {
        // `ALL_MCP_APPS` drives BOTH write paths' "and no others" semantics:
        // `mcp_upsert_local_server` removes the server from every app it lists
        // but was not handed, and `mcp_remove_server`'s `apps: None` branch
        // deletes it from every app it lists. An app missing from it fails
        // SILENTLY — the stale entry stays on disk and the very next scan
        // reports it as a live assignment, so an "uninstalled" server reappears
        // on refresh. Nothing else catches that: the per-app dispatchers are
        // exhaustive `match`es, but a constant is just data.
        //
        // The `match` below is the guard. It has no `_` arm, so adding a
        // variant to `McpAppType` stops COMPILING here until whoever added it
        // decides — deliberately — whether the write paths must reach it.
        for app in ALL_MCP_APPS {
            match app {
                McpAppType::ClaudeCode
                | McpAppType::Codex
                | McpAppType::OpenCode
                | McpAppType::Gemini
                | McpAppType::OpenClaw
                | McpAppType::Cline
                | McpAppType::Hermes
                | McpAppType::CodeBuddy
                | McpAppType::KimiCode
                | McpAppType::Grok
                | McpAppType::Cursor
                | McpAppType::DeepSeek
                | McpAppType::Qoder
                | McpAppType::Antigravity
                | McpAppType::Pi => {}
            }
        }

        // No duplicates: `mcp_upsert_local_server` walks this list once per app,
        // so a repeat would upsert (or remove) the same file twice per save.
        let unique = ALL_MCP_APPS.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            ALL_MCP_APPS.len(),
            "ALL_MCP_APPS must not repeat an app"
        );

        // Every app the scan can ATTRIBUTE a server to must also be one the
        // write paths can reach; otherwise the UI shows an assignment codeg can
        // neither edit nor clear. (The reverse is allowed: a write-only target
        // with no reader would just never show up.)
        let scannable = local_mcp_readers()
            .iter()
            .map(|reader| reader.app)
            .collect::<BTreeSet<_>>();
        assert!(
            scannable.is_subset(&unique),
            "every scanned source must be writable: {:?}",
            scannable.difference(&unique).collect::<Vec<_>>()
        );
    }

    #[test]
    fn mcp_app_type_wire_names_match_the_agent_type_they_name() {
        use crate::models::agent::AgentType;

        // The settings page sends ONE spelling per agent. `McpAppType` derives
        // its own from `rename_all = "snake_case"`, so a variant whose CamelCase
        // splits differently than `AgentType::as_wire` spells it (`DeepSeek` →
        // `deep_seek` vs `deepseek`) makes `mcp_upsert_local_server` reject the
        // app with `unknown variant` while every other page accepts it. Pin the
        // pairing rather than the string: the next agent added to both enums
        // fails here instead of at the user's save button.
        for (app, agent) in [
            (McpAppType::ClaudeCode, AgentType::ClaudeCode),
            (McpAppType::Codex, AgentType::Codex),
            (McpAppType::Gemini, AgentType::Gemini),
            (McpAppType::OpenClaw, AgentType::OpenClaw),
            (McpAppType::OpenCode, AgentType::OpenCode),
            (McpAppType::Cline, AgentType::Cline),
            (McpAppType::Hermes, AgentType::Hermes),
            (McpAppType::CodeBuddy, AgentType::CodeBuddy),
            (McpAppType::KimiCode, AgentType::KimiCode),
            (McpAppType::Grok, AgentType::Grok),
            (McpAppType::Cursor, AgentType::Cursor),
            (McpAppType::DeepSeek, AgentType::DeepSeek),
            (McpAppType::Qoder, AgentType::Qoder),
            (McpAppType::Antigravity, AgentType::Antigravity),
            (McpAppType::Pi, AgentType::Pi),
        ] {
            let wire = serde_json::to_value(app).expect("serialize app type");
            assert_eq!(
                wire.as_str(),
                Some(agent.as_wire().as_ref()),
                "{app:?} must serialize as the agent's wire name"
            );
            // And it must parse back: the app list arrives as these strings.
            assert_eq!(
                serde_json::from_value::<McpAppType>(wire).expect("round-trip"),
                app
            );
        }
    }

    #[test]
    fn grok_config_toml_round_trips_and_preserves_sections() {
        // Grok reads `<GROK_HOME>/config.toml` `[mcp_servers.<name>]` natively —
        // same table as Codex but with NO `type` key (transport inferred). The
        // file also holds unrelated `[cli]`/`[ui]` sections that must survive.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[cli]\nauto_update = true\n\n[ui]\nyolo = false\n",
        )
        .expect("seed config");

        // Missing entry → no servers; removing is a no-op.
        assert!(read_grok_servers_at(&path).expect("read seed").is_empty());
        assert!(!remove_grok_server_at(&path, "fs").expect("remove missing"));

        // Upsert a stdio server carrying command/args/env/cwd.
        let stdio = json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
            "env": { "TOKEN": "sk-abc" },
            "cwd": "/work/dir",
        });
        upsert_grok_server_at(&path, "fs", &stdio).expect("upsert stdio");

        // Upsert a remote server with headers (Grok uses `headers`, not
        // Codex's `http_headers`).
        let http = json!({
            "type": "http",
            "url": "https://mcp.example.com/mcp",
            "headers": { "Authorization": "Bearer xyz" },
        });
        upsert_grok_server_at(&path, "remote", &http).expect("upsert http");

        // Upsert an SSE server — Grok marks these with an explicit `type = "sse"`;
        // without it the entry would round-trip back to `http`.
        let sse = json!({ "type": "sse", "url": "https://mcp.linear.app/sse" });
        upsert_grok_server_at(&path, "linear", &sse).expect("upsert sse");

        // All round-trip, canonicalized, with cwd + headers + sse transport kept.
        let servers = read_grok_servers_at(&path).expect("read back");
        assert_eq!(servers.len(), 3);
        let fs = servers.get("fs").expect("fs present");
        assert_eq!(fs.get("type").and_then(Value::as_str), Some("stdio"));
        assert_eq!(fs.get("command").and_then(Value::as_str), Some("npx"));
        assert_eq!(fs.get("cwd").and_then(Value::as_str), Some("/work/dir"));
        assert_eq!(
            fs.pointer("/env/TOKEN").and_then(Value::as_str),
            Some("sk-abc")
        );
        let remote = servers.get("remote").expect("remote present");
        assert_eq!(remote.get("type").and_then(Value::as_str), Some("http"));
        assert_eq!(
            remote.pointer("/headers/Authorization").and_then(Value::as_str),
            Some("Bearer xyz")
        );
        let linear = servers.get("linear").expect("linear present");
        assert_eq!(linear.get("type").and_then(Value::as_str), Some("sse"));
        assert_eq!(
            linear.get("url").and_then(Value::as_str),
            Some("https://mcp.linear.app/sse")
        );

        // On-disk: `[mcp_servers.fs]` has NO `type` key, `[cli]`/`[ui]` survive.
        let raw = std::fs::read_to_string(&path).expect("read file");
        let root: toml::Value = raw.parse().expect("parse toml");
        let table = root.as_table().expect("root table");
        assert!(table.contains_key("cli"), "[cli] preserved");
        assert!(table.contains_key("ui"), "[ui] preserved");
        let fs_entry = table
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .and_then(|m| m.get("fs"))
            .and_then(toml::Value::as_table)
            .expect("mcp_servers.fs");
        assert!(!fs_entry.contains_key("type"), "stdio entries omit `type`");
        assert_eq!(
            fs_entry.get("cwd").and_then(toml::Value::as_str),
            Some("/work/dir")
        );
        // SSE entries, by contrast, must keep the explicit `type = "sse"`.
        let linear_entry = table
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .and_then(|m| m.get("linear"))
            .and_then(toml::Value::as_table)
            .expect("mcp_servers.linear");
        assert_eq!(
            linear_entry.get("type").and_then(toml::Value::as_str),
            Some("sse")
        );

        // Remove one; the others and the unrelated sections remain.
        assert!(remove_grok_server_at(&path, "fs").expect("remove fs"));
        let after = read_grok_servers_at(&path).expect("read after remove");
        assert_eq!(after.len(), 2);
        assert!(after.contains_key("remote"));
        assert!(after.contains_key("linear"));
        let raw2 = std::fs::read_to_string(&path).expect("read file 2");
        let root2: toml::Value = raw2.parse().expect("parse toml 2");
        assert!(root2.as_table().expect("t").contains_key("cli"));
    }

    fn codex_entry(toml_src: &str) -> toml::Value {
        toml::from_str::<toml::Value>(toml_src).expect("parse test toml")
    }

    #[test]
    fn codex_entry_canonicalizes_streamable_http_aliases() {
        for raw in ["streamableHttp", "streamable-http", "streamable_http"] {
            let value = codex_entry(&format!(
                "type = \"{raw}\"\nurl = \"https://mcp.example.com/mcp\"\n"
            ));
            let canonical = codex_entry_to_canonical("ex", &value)
                .unwrap_or_else(|err| panic!("input {raw:?} should normalize: {err}"));
            assert_eq!(
                canonical
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "http",
                "input {raw:?}"
            );
            assert_eq!(
                canonical
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "https://mcp.example.com/mcp"
            );
        }
    }

    #[test]
    fn codex_entry_keeps_canonical_types_intact() {
        let stdio = codex_entry("type = \"stdio\"\ncommand = \"npx\"\n");
        let canonical = codex_entry_to_canonical("ex", &stdio).expect("stdio entry");
        assert_eq!(canonical.get("type").and_then(Value::as_str), Some("stdio"));
        assert_eq!(
            canonical.get("command").and_then(Value::as_str),
            Some("npx")
        );

        let sse = codex_entry("type = \"sse\"\nurl = \"https://mcp.example.com/sse\"\n");
        let canonical = codex_entry_to_canonical("ex", &sse).expect("sse entry");
        assert_eq!(canonical.get("type").and_then(Value::as_str), Some("sse"));
    }

    #[test]
    fn codex_entry_rejects_unknown_type_with_raw_in_message() {
        let value = codex_entry("type = \"Foo\"\nurl = \"https://x\"\n");
        let err = codex_entry_to_canonical("ex", &value).expect_err("Foo should be rejected");
        let msg = err.to_string();
        assert!(msg.contains("'Foo'"), "error should echo raw type: {msg}");
        assert!(msg.contains("'ex'"), "error should mention id: {msg}");
        assert_eq!(
            err.i18n_key.as_deref(),
            Some("errors.codexEntryUnsupportedType")
        );
        let params = err.i18n_params.as_ref().expect("i18n params attached");
        assert_eq!(params.get("id").map(String::as_str), Some("ex"));
        assert_eq!(params.get("type").map(String::as_str), Some("Foo"));
    }

    #[test]
    fn codex_entry_rejects_opencode_only_aliases() {
        // OpenCode-native types are not valid in Codex TOML; catching them keeps
        // the Codex pipeline's accepted set tight.
        for raw in ["local", "remote"] {
            let value = codex_entry(&format!("type = \"{raw}\"\nurl = \"https://x\"\n"));
            assert!(
                codex_entry_to_canonical("ex", &value).is_err(),
                "raw {raw:?} should not be accepted by Codex pipeline",
            );
        }
    }

    #[test]
    fn canonical_to_codex_entry_never_emits_type_field() {
        // Codex infers the transport from the keys present; an emitted `type` is
        // schema-invalid and fatal under `codex --strict-config` (#325). No
        // transport may emit it.
        let stdio = canonical_to_codex_entry(&json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "tavily-mcp@0.2.15"],
        }))
        .expect("stdio entry")
        .as_table()
        .cloned()
        .expect("stdio table");
        assert!(!stdio.contains_key("type"), "stdio must not carry type");
        assert_eq!(
            stdio.get("command").and_then(toml::Value::as_str),
            Some("npx")
        );

        let http = canonical_to_codex_entry(&json!({
            "type": "http",
            "url": "https://mcp.exa.ai/mcp",
        }))
        .expect("http entry")
        .as_table()
        .cloned()
        .expect("http table");
        assert!(!http.contains_key("type"), "http must not carry type");
        assert_eq!(
            http.get("url").and_then(toml::Value::as_str),
            Some("https://mcp.exa.ai/mcp")
        );

        // Codex can't represent SSE (its config.toml has only stdio + streamable
        // HTTP); the writer rejects it rather than degrade to a bare `url` that
        // would read back as `http` and reclassify the shared spec.
        assert!(
            canonical_to_codex_entry(&json!({
                "type": "sse",
                "url": "https://mcp.example.com/sse",
            }))
            .is_err(),
            "sse must be rejected for Codex"
        );
    }

    #[test]
    fn app_can_host_spec_excludes_codex_from_sse_only() {
        let sse = json!({"type": "sse", "url": "https://x/sse"});
        let http = json!({"type": "http", "url": "https://x/mcp"});
        let stdio = json!({"type": "stdio", "command": "npx"});
        // Codex can host stdio/http but not sse.
        assert!(!app_can_host_spec(McpAppType::Codex, &sse));
        assert!(app_can_host_spec(McpAppType::Codex, &http));
        assert!(app_can_host_spec(McpAppType::Codex, &stdio));
        // Every other agent can host sse.
        assert!(app_can_host_spec(McpAppType::Gemini, &sse));
        assert!(app_can_host_spec(McpAppType::Cline, &sse));
        assert!(app_can_host_spec(McpAppType::KimiCode, &sse));
    }

    #[test]
    fn codex_entry_infers_transport_when_type_absent() {
        // Native Codex tables (and codeg's own post-#325 output) carry no `type`;
        // the reader must infer it from the transport keys, not assume stdio (which
        // silently dropped every url-only server). Mirrors the issue's config.
        let http = codex_entry("url = \"https://mcp.exa.ai/mcp\"\n");
        let canonical = codex_entry_to_canonical("exa", &http).expect("url-only entry");
        assert_eq!(canonical.get("type").and_then(Value::as_str), Some("http"));
        assert_eq!(
            canonical.get("url").and_then(Value::as_str),
            Some("https://mcp.exa.ai/mcp")
        );

        let stdio = codex_entry("command = \"npx\"\nargs = [\"-y\", \"tavily-mcp@0.2.15\"]\n");
        let canonical = codex_entry_to_canonical("tavily", &stdio).expect("command-only entry");
        assert_eq!(canonical.get("type").and_then(Value::as_str), Some("stdio"));
        assert_eq!(
            canonical.get("command").and_then(Value::as_str),
            Some("npx")
        );
    }

    #[test]
    fn codex_write_read_round_trips_without_type_key() {
        // The writer's output must read back to the same canonical spec with no
        // `type` ever hitting disk. (sse is excluded: Codex rejects it — covered by
        // `canonical_to_codex_entry_never_emits_type_field`.)
        for spec in [
            json!({"type": "stdio", "command": "npx", "args": ["-y", "srv"], "env": {"A": "b"}}),
            json!({"type": "http", "url": "https://mcp.exa.ai/mcp"}),
        ] {
            let entry = canonical_to_codex_entry(&spec).expect("to codex entry");
            assert!(
                !entry.as_table().expect("table").contains_key("type"),
                "no type on disk for {spec}"
            );
            let back = codex_entry_to_canonical("id", &entry).expect("read back");
            assert_eq!(
                back.get("type").and_then(Value::as_str),
                spec.get("type").and_then(Value::as_str),
                "round-trip type for {spec}"
            );
        }
    }

    #[test]
    fn canonical_to_cline_entry_remaps_http_to_streamable_http() {
        // Cline's zod `type` literal accepts only stdio|sse|streamableHttp; the
        // canonical `http` must become `streamableHttp` or Cline drops every
        // server (#325). stdio/sse pass through unchanged.
        let http = canonical_to_cline_entry(&json!({
            "type": "http",
            "url": "https://mcp.exa.ai/mcp",
        }))
        .expect("http entry");
        assert_eq!(
            http.get("type").and_then(Value::as_str),
            Some("streamableHttp"),
            "http must be remapped for Cline"
        );
        assert_eq!(
            http.get("url").and_then(Value::as_str),
            Some("https://mcp.exa.ai/mcp")
        );

        let stdio = canonical_to_cline_entry(&json!({"type": "stdio", "command": "npx"}))
            .expect("stdio entry");
        assert_eq!(stdio.get("type").and_then(Value::as_str), Some("stdio"));

        let sse = canonical_to_cline_entry(&json!({"type": "sse", "url": "https://x/sse"}))
            .expect("sse entry");
        assert_eq!(sse.get("type").and_then(Value::as_str), Some("sse"));

        // And codeg reads `streamableHttp` straight back to canonical `http`.
        let round_trip = canonicalize_spec(
            &json!({"type": "streamableHttp", "url": "https://mcp.exa.ai/mcp"}),
            "test",
        )
        .expect("canonicalize streamableHttp");
        assert_eq!(round_trip.get("type").and_then(Value::as_str), Some("http"));
    }

    #[test]
    fn canonical_to_kimi_code_entry_pins_remote_transport() {
        // Kimi 0.23.3 keys the transport off `transport` (defaulting url-only to
        // HTTP), so codeg must emit an explicit `transport` or an SSE server silently
        // downgrades to HTTP (#325). stdio is left as-is (Kimi infers it from
        // `command`).
        let sse = canonical_to_kimi_code_entry(&json!({"type": "sse", "url": "https://x/stream"}))
            .expect("sse entry");
        assert_eq!(sse.get("transport").and_then(Value::as_str), Some("sse"));
        assert_eq!(sse.get("type").and_then(Value::as_str), Some("sse"));

        let http = canonical_to_kimi_code_entry(&json!({"type": "http", "url": "https://x/mcp"}))
            .expect("http entry");
        assert_eq!(http.get("transport").and_then(Value::as_str), Some("http"));

        let stdio = canonical_to_kimi_code_entry(&json!({"type": "stdio", "command": "npx"}))
            .expect("stdio entry");
        assert!(
            stdio.get("transport").is_none(),
            "stdio must not carry a transport key"
        );
    }

    #[test]
    fn kimi_code_entry_reads_native_transport_and_never_leaks_it() {
        // A native Kimi SSE entry uses `transport: "sse"`, not `type`; the reader
        // must classify it as sse from that explicit `transport` and must NOT surface
        // `transport` in the canonical spec — otherwise it would leak into e.g. Codex
        // TOML when the same server is later synced to another agent (#325).
        let native_sse = json!({"url": "https://x/stream", "transport": "sse"});
        let canonical = kimi_code_entry_to_canonical(&native_sse, "srv").expect("native sse");
        assert_eq!(canonical.get("type").and_then(Value::as_str), Some("sse"));
        assert!(
            canonical.get("transport").is_none(),
            "transport must be consumed, never leaked into the canonical spec"
        );

        // Full writer→reader round-trip stays canonical and transport-free.
        let written = canonical_to_kimi_code_entry(&json!({"type": "sse", "url": "https://x/stream"}))
            .expect("write sse");
        let back = kimi_code_entry_to_canonical(&written, "srv").expect("read back");
        assert_eq!(back.get("type").and_then(Value::as_str), Some("sse"));
        assert!(back.get("transport").is_none());
    }

    #[test]
    fn kimi_code_entry_mirrors_kimi_0_23_transport_selection() {
        // Kimi Code 0.23.3 defaults a url-only remote entry to HTTP and does NOT
        // infer SSE from a `/sse` URL path — only an explicit `transport: "sse"`
        // yields SSE. (Corrects the earlier FastMCP-based reader; verified against
        // the published 0.23.3 Zod schema.)
        let sse_url = kimi_code_entry_to_canonical(&json!({"url": "https://host/sse"}), "s")
            .expect("url-only /sse");
        assert_eq!(
            sse_url.get("type").and_then(Value::as_str),
            Some("http"),
            "url-only must be http, not sse-from-url"
        );

        let http_url = kimi_code_entry_to_canonical(&json!({"url": "https://host/mcp"}), "s")
            .expect("url-only");
        assert_eq!(http_url.get("type").and_then(Value::as_str), Some("http"));

        // An on-disk `type` with NO `transport` does not classify: Kimi strips `type`
        // and infers HTTP from the url, so codeg must too (not report it as SSE).
        let stale_type = kimi_code_entry_to_canonical(
            &json!({"type": "sse", "url": "https://host/mcp"}),
            "s",
        )
        .expect("type-without-transport");
        assert_eq!(stale_type.get("type").and_then(Value::as_str), Some("http"));

        // Explicit `transport: "sse"` yields SSE (and `type` is ignored, matching
        // Kimi); `transport` is stripped from the canonical spec.
        let sse = kimi_code_entry_to_canonical(
            &json!({"type": "http", "url": "https://host/mcp", "transport": "sse"}),
            "s",
        )
        .expect("explicit sse");
        assert_eq!(sse.get("type").and_then(Value::as_str), Some("sse"));
        assert!(sse.get("transport").is_none());

        // An explicit unknown transport Kimi would hard-reject is surfaced as an
        // invalid entry, not reported as an active server. (`stdio` on a url-only
        // entry is likewise invalid — Kimi's stdio variant requires `command`.)
        for bad in ["streamable-http", "ws", "stdio"] {
            assert!(
                kimi_code_entry_to_canonical(
                    &json!({"url": "https://host/mcp", "transport": bad}),
                    "s"
                )
                .is_err(),
                "transport {bad:?} must be rejected"
            );
        }
        // A non-string transport is rejected too (Kimi's literals are exact).
        assert!(
            kimi_code_entry_to_canonical(&json!({"url": "https://host/mcp", "transport": 3}), "s")
                .is_err()
        );

        // The `transport` discriminant wins over the entry's key shape: an explicit
        // `sse` on an entry that ALSO carries `command` is SSE (Kimi ignores the
        // extra `command`), not stdio.
        let sse_over_cmd = kimi_code_entry_to_canonical(
            &json!({"transport": "sse", "command": "npx", "url": "https://host/mcp"}),
            "s",
        )
        .expect("transport wins over command");
        assert_eq!(sse_over_cmd.get("type").and_then(Value::as_str), Some("sse"));
    }

    #[test]
    fn canonical_to_kimi_code_entry_drops_wrong_typed_and_foreign_fields() {
        // Kimi validates its known fields and rejects the whole `mcpServers` record on
        // a wrong-typed one, so the writer must not let a stray same-named foreign
        // value ride canonicalize's passthrough onto disk. See #325.
        let entry = canonical_to_kimi_code_entry(&json!({
            "type": "http",
            "url": "https://host/mcp",
            "enabled": "false",   // wrong shape (string, not bool) → dropped
            "autoApprove": ["a"], // foreign key → dropped
        }))
        .expect("http entry");
        let obj = entry.as_object().expect("object");
        assert_eq!(obj.get("transport").and_then(Value::as_str), Some("http"));
        assert!(!obj.contains_key("enabled"), "wrong-typed enabled must be dropped");
        assert!(!obj.contains_key("autoApprove"), "foreign key must be dropped");

        // A correctly-typed `enabled` bool is preserved.
        let ok = canonical_to_kimi_code_entry(&json!({
            "type": "http", "url": "https://host/mcp", "enabled": true,
        }))
        .expect("http entry");
        assert_eq!(
            ok.as_object().and_then(|o| o.get("enabled")).and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn kimi_code_entry_preserves_kimi_only_fields_through_a_round_trip() {
        // Fields Kimi models but codeg has no UI for must survive read→write:
        // dropping them breaks a server the user configured through Kimi's own
        // `/mcp-config` the first time they touch it in codeg's editor.
        let on_disk = json!({
            "command": "npx",
            "args": ["-y", "server"],
            "runtime_id": "remote-box",       // Kimi 0.38.0+
            "executor": "kaos",
            "startupTimeoutMs": 30_000,
            "toolTimeoutMs": 120_000,
            "enabledTools": ["a", "b"],
            "disabledTools": [],
        });
        let canonical = kimi_code_entry_to_canonical(&on_disk, "srv").expect("canonical");
        let written = canonical_to_kimi_code_entry(&canonical).expect("write");
        let obj = written.as_object().expect("object");
        assert_eq!(obj.get("runtime_id").and_then(Value::as_str), Some("remote-box"));
        assert_eq!(obj.get("executor").and_then(Value::as_str), Some("kaos"));
        assert_eq!(obj.get("startupTimeoutMs").and_then(Value::as_i64), Some(30_000));
        assert_eq!(obj.get("toolTimeoutMs").and_then(Value::as_i64), Some(120_000));
        assert_eq!(obj.get("enabledTools"), Some(&json!(["a", "b"])));
        assert_eq!(obj.get("disabledTools"), Some(&json!([])));

        // Remote auth fields ride along on http/sse entries.
        let remote = canonical_to_kimi_code_entry(&json!({
            "type": "http",
            "url": "https://host/mcp",
            "auth": "oauth",
            "bearerTokenEnvVar": "MCP_TOKEN",
        }))
        .expect("http entry");
        let remote = remote.as_object().expect("object");
        assert_eq!(remote.get("auth").and_then(Value::as_str), Some("oauth"));
        assert_eq!(
            remote.get("bearerTokenEnvVar").and_then(Value::as_str),
            Some("MCP_TOKEN")
        );
    }

    #[test]
    fn kimi_extension_fields_survive_a_scan_that_another_agent_won() {
        // `scan_local_servers` is first-writer-wins, so a server that also lives
        // in an earlier-scanned agent resolves to that agent's projection — which
        // never carried Kimi's own fields. They must be folded back in, or the
        // editor never shows them and the next save writes them off disk.
        let kimi = json!({
            "type": "stdio",
            "command": "npx",
            "runtime_id": "remote-box",
            "executor": "kaos",
            "toolTimeoutMs": 120_000,
            "disabledTools": ["danger"],
        });
        let none = Map::new();
        // What Codex's reader produces for the same id: the core spec only.
        let mut canonical = json!({"type": "stdio", "command": "npx", "args": ["-y", "ctx7"]});
        merge_kimi_extension_fields(&mut canonical, &kimi, &none);
        let obj = canonical.as_object().expect("object");
        assert_eq!(obj.get("runtime_id").and_then(Value::as_str), Some("remote-box"));
        assert_eq!(obj.get("executor").and_then(Value::as_str), Some("kaos"));
        assert_eq!(obj.get("toolTimeoutMs").and_then(Value::as_i64), Some(120_000));
        assert_eq!(obj.get("disabledTools"), Some(&json!(["danger"])));
        // The winning spec still owns every key it already had.
        assert_eq!(obj.get("args"), Some(&json!(["-y", "ctx7"])));

        // Kimi's copy is authoritative for the keys it owns, absence included:
        // codeg's permissive writers copy them into other agents' files, so an
        // earlier-scanned agent's stale copy must not pin the canonical spec.
        let mut stale = json!({
            "type": "stdio",
            "command": "npx",
            "runtime_id": "was-here",   // Kimi now says "remote-box"
            "enabledTools": ["gone"],   // Kimi no longer sets this at all
        });
        merge_kimi_extension_fields(&mut stale, &kimi, &none);
        assert_eq!(
            stale.get("runtime_id").and_then(Value::as_str),
            Some("remote-box")
        );
        assert!(
            !stale.as_object().expect("object").contains_key("enabledTools"),
            "a key Kimi no longer sets must not survive in another agent's copy"
        );
        // Only the Kimi-owned keys are touched.
        assert_eq!(stale.get("command").and_then(Value::as_str), Some("npx"));

        // `auth` is OpenClaw's as much as Kimi's, so OpenClaw's own value wins
        // over a Kimi copy that does not set it...
        let kimi_remote = json!({
            "type": "http",
            "url": "https://host/mcp",
            "bearerTokenEnvVar": "TOKEN",
        });
        let owns_auth: Map<String, Value> =
            [("auth".to_string(), json!("oauth"))].into_iter().collect();
        let mut openclaw_first = json!({
            "type": "http",
            "url": "https://host/mcp",
            "auth": "oauth",
        });
        merge_kimi_extension_fields(&mut openclaw_first, &kimi_remote, &owns_auth);
        assert_eq!(
            openclaw_first.get("auth").and_then(Value::as_str),
            Some("oauth"),
            "OpenClaw declared `auth` itself; Kimi's copy must not clear it"
        );
        assert_eq!(
            openclaw_first.get("bearerTokenEnvVar").and_then(Value::as_str),
            Some("TOKEN")
        );

        // ...even when the spec that won the scan came from a third agent that
        // carries no `auth` at all. Only the owner's VALUE can restore it.
        let mut claude_first = json!({"type": "http", "url": "https://host/mcp"});
        merge_kimi_extension_fields(&mut claude_first, &kimi_remote, &owns_auth);
        assert_eq!(
            claude_first.get("auth").and_then(Value::as_str),
            Some("oauth"),
            "a scan won by a third agent must not drop OpenClaw's OAuth"
        );

        // But the same value in an agent that does NOT model `auth` is only an
        // echo codeg wrote there, so removing it in Kimi must take effect.
        let mut echo = json!({
            "type": "http",
            "url": "https://host/mcp",
            "auth": "oauth",
        });
        merge_kimi_extension_fields(&mut echo, &kimi_remote, &none);
        assert!(
            !echo.as_object().expect("object").contains_key("auth"),
            "a passthrough echo must not pin a key Kimi no longer sets"
        );

        // stdio-only fields are never folded onto a remote server, and remote-only
        // fields are never folded onto a stdio one.
        let mut remote = json!({"type": "http", "url": "https://host/mcp"});
        merge_kimi_extension_fields(&mut remote, &kimi, &none);
        let remote_obj = remote.as_object().expect("object");
        assert!(!remote_obj.contains_key("runtime_id"));
        assert!(!remote_obj.contains_key("executor"));
        assert_eq!(remote_obj.get("toolTimeoutMs").and_then(Value::as_i64), Some(120_000));

        let mut stdio = json!({"type": "stdio", "command": "npx"});
        merge_kimi_extension_fields(
            &mut stdio,
            &json!({"type": "http", "url": "https://host/mcp", "auth": "oauth"}),
            &none,
        );
        assert!(!stdio.as_object().expect("object").contains_key("auth"));
    }

    #[test]
    fn kimi_extension_keys_and_their_validator_stay_in_step() {
        // `merge_kimi_extension_fields` clears KIMI_OWNED_KEYS before folding
        // Kimi's values back in, so a key listed there but unknown to
        // `is_kimi_extension_field` would be silently erased and never restored.
        let sample = |key: &str| -> (Value, bool) {
            match key {
                "runtime_id" => (json!("local"), true),
                "executor" => (json!("kaos"), true),
                "auth" => (json!("oauth"), false),
                "bearerTokenEnvVar" => (json!("TOKEN"), false),
                "startupTimeoutMs" | "toolTimeoutMs" => (json!(1_000), true),
                "enabledTools" | "disabledTools" => (json!(["a"]), true),
                other => panic!("{other:?} was listed with no sample value here"),
            }
        };
        for key in KIMI_OWNED_KEYS.iter().chain(KIMI_SHARED_KEYS) {
            let (value, stdio) = sample(key);
            assert!(
                is_kimi_extension_field(key, &value, stdio),
                "{key} is listed but its validator rejects a well-formed value"
            );
        }
        // A key another agent also models must never be cleared as if Kimi owned
        // it — that is what makes it "shared".
        for key in KIMI_SHARED_KEYS {
            assert!(
                !KIMI_OWNED_KEYS.contains(key),
                "{key} cannot be both owned and shared"
            );
        }
    }

    #[test]
    fn kimi_code_upsert_honors_removing_a_kimi_only_field() {
        // The counterpart to folding fields in at scan time: because the editor
        // shows them, a spec that omits one means the user deleted it. The writer
        // must not resurrect it from whatever is already on disk.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        upsert_kimi_code_server_at(
            &path,
            "remote",
            &json!({
                "type": "http",
                "url": "https://host/mcp",
                "auth": "oauth",
                "bearerTokenEnvVar": "OLD_TOKEN",
            }),
        )
        .expect("seed");
        assert_eq!(
            read_kimi_code_servers_at(&path).expect("read")["remote"]
                .get("bearerTokenEnvVar")
                .and_then(Value::as_str),
            Some("OLD_TOKEN")
        );

        upsert_kimi_code_server_at(
            &path,
            "remote",
            &json!({"type": "http", "url": "https://host/mcp", "auth": "oauth"}),
        )
        .expect("save without the token field");
        let after = read_kimi_code_servers_at(&path).expect("read");
        let obj = after["remote"].as_object().expect("object");
        assert!(
            !obj.contains_key("bearerTokenEnvVar"),
            "removing a field in codeg's editor must reach disk"
        );
        assert_eq!(obj.get("auth").and_then(Value::as_str), Some("oauth"));
    }

    #[test]
    fn kimi_code_entry_drops_kimi_only_fields_that_fail_kimi_schema() {
        // The whole-record guard still holds: a same-named value of the wrong
        // shape would make Kimi reject every server in `mcp.json`, so it is
        // dropped rather than written back.
        let entry = canonical_to_kimi_code_entry(&json!({
            "type": "stdio",
            "command": "npx",
            "runtime_id": "",              // schema wants min length 1
            "executor": "podman",          // not one of local | kaos
            "startupTimeoutMs": 0,         // schema wants >= 1
            "toolTimeoutMs": "30000",      // string, not int
            "enabledTools": ["a", 7],      // not all strings
        }))
        .expect("stdio entry");
        let obj = entry.as_object().expect("object");
        for key in [
            "runtime_id",
            "executor",
            "startupTimeoutMs",
            "toolTimeoutMs",
            "enabledTools",
        ] {
            assert!(!obj.contains_key(key), "{key} must be dropped");
        }
        // `auth` accepts only the `oauth` literal.
        let bad_auth = canonical_to_kimi_code_entry(&json!({
            "type": "http", "url": "https://host/mcp", "auth": "bearer",
        }))
        .expect("http entry");
        assert!(!bad_auth.as_object().expect("object").contains_key("auth"));
    }

    #[test]
    fn codex_entry_rejects_both_command_and_url() {
        // Codex hard-errors on a mixed-transport entry; codeg must reject it rather
        // than silently classify as stdio and drop the `url` (#325).
        let both = codex_entry("command = \"npx\"\nurl = \"https://x/mcp\"\n");
        assert!(codex_entry_to_canonical("mixed", &both).is_err());
        // Rejected even when an explicit (legacy) type is present.
        let both_typed =
            codex_entry("type = \"stdio\"\ncommand = \"npx\"\nurl = \"https://x/mcp\"\n");
        assert!(codex_entry_to_canonical("mixed", &both_typed).is_err());
    }

    #[test]
    fn canonical_to_codex_entry_passthrough_is_type_validated() {
        // Foreign keys, transport-specific fields, and — crucially — same-named
        // fields of the WRONG shape must NOT reach Codex TOML; each is fatal under
        // --strict-config. Only transport-agnostic fields validated to Codex's exact
        // type pass through. See #325.
        let entry = canonical_to_codex_entry(&json!({
            "type": "http",
            "url": "https://mcp.exa.ai/mcp",
            "enabled": true,             // valid Codex bool → kept
            "required": "yes",           // wrong shape (string, not bool) → dropped
            "autoApprove": ["a"],        // foreign key → dropped
            "transport": "sse",          // canonical-only discriminator → dropped
            "env_vars": [{"name": "X"}], // stdio-only, wrong arm here → dropped
            "startup_timeout_sec": 10.0, // not in the minimal allowlist → dropped
        }))
        .expect("http entry")
        .as_table()
        .cloned()
        .expect("table");
        assert_eq!(entry.get("enabled").and_then(toml::Value::as_bool), Some(true));
        for dropped in [
            "type",
            "required",
            "autoApprove",
            "transport",
            "env_vars",
            "startup_timeout_sec",
        ] {
            assert!(
                !entry.contains_key(dropped),
                "'{dropped}' must be dropped from Codex TOML"
            );
        }
    }

    #[test]
    fn transport_protocol_normalizes_aliases() {
        assert_eq!(transport_protocol("stdio"), Some("stdio".to_string()));
        assert_eq!(transport_protocol("http"), Some("http".to_string()));
        assert_eq!(transport_protocol("sse"), Some("sse".to_string()));
        assert_eq!(
            transport_protocol("streamable-http"),
            Some("http".to_string())
        );
        assert_eq!(
            transport_protocol("streamableHttp"),
            Some("http".to_string())
        );
        assert_eq!(transport_protocol("local"), None);
        assert_eq!(transport_protocol("foo"), None);
    }

    fn make_transport(kind: &str, url: &str) -> OfficialTransport {
        let payload = serde_json::json!({
            "type": kind,
            "url": url,
        });
        serde_json::from_value(payload).expect("OfficialTransport from json")
    }

    #[test]
    fn remote_spec_from_transport_normalizes_aliases() {
        for raw in ["streamable-http", "streamableHttp", "http"] {
            let transport = make_transport(raw, "https://mcp.example.com/mcp");
            let spec =
                remote_spec_from_transport_with_values(&transport, &Map::new(), false).unwrap();
            assert_eq!(
                spec.get("type").and_then(Value::as_str),
                Some("http"),
                "raw {raw:?}"
            );
        }

        let sse = make_transport("sse", "https://mcp.example.com/sse");
        let spec = remote_spec_from_transport_with_values(&sse, &Map::new(), false).unwrap();
        assert_eq!(spec.get("type").and_then(Value::as_str), Some("sse"));

        let unknown = make_transport("ws", "https://x");
        let err = remote_spec_from_transport_with_values(&unknown, &Map::new(), false)
            .expect_err("ws should be rejected");
        assert_eq!(
            err.i18n_key.as_deref(),
            Some("errors.unsupportedTransportType")
        );
        let params = err.i18n_params.as_ref().expect("i18n params attached");
        assert_eq!(params.get("type").map(String::as_str), Some("ws"));
    }

    fn make_smithery_connection(kind: &str) -> SmitheryConnection {
        let payload = serde_json::json!({ "type": kind });
        serde_json::from_value(payload).expect("SmitheryConnection from json")
    }

    #[test]
    fn smithery_connection_protocol_normalizes_aliases() {
        assert_eq!(
            smithery_connection_protocol(&make_smithery_connection("streamable-http")),
            "http"
        );
        assert_eq!(
            smithery_connection_protocol(&make_smithery_connection("streamableHttp")),
            "http"
        );
        assert_eq!(
            smithery_connection_protocol(&make_smithery_connection("sse")),
            "sse"
        );
        // Unknown falls back to http (preserves prior permissive behavior).
        assert_eq!(
            smithery_connection_protocol(&make_smithery_connection("ws")),
            "http"
        );
    }

    fn hermes_entry(yaml_src: &str) -> serde_yaml::Value {
        serde_yaml::from_str::<serde_yaml::Value>(yaml_src).expect("parse test yaml")
    }

    #[test]
    fn hermes_entry_to_canonical_stdio() {
        let entry = hermes_entry(
            "command: npx\nargs:\n  - -y\n  - \"@modelcontextprotocol/server-github\"\nenv:\n  GITHUB_TOKEN: ghp_x\n",
        );
        let spec = hermes_entry_to_canonical(&entry, "github").expect("canonical");
        assert_eq!(spec.get("type").and_then(Value::as_str), Some("stdio"));
        assert_eq!(spec.get("command").and_then(Value::as_str), Some("npx"));
        let args = spec.get("args").and_then(Value::as_array).expect("args");
        assert_eq!(args.len(), 2);
        assert_eq!(
            spec.get("env")
                .and_then(|e| e.get("GITHUB_TOKEN"))
                .and_then(Value::as_str),
            Some("ghp_x")
        );
    }

    #[test]
    fn hermes_entry_to_canonical_http_and_sse() {
        // A bare `url` is StreamableHTTP.
        let http = hermes_entry_to_canonical(
            &hermes_entry("url: https://mcp.example.com/mcp\n"),
            "remote-http",
        )
        .expect("http canonical");
        assert_eq!(http.get("type").and_then(Value::as_str), Some("http"));
        assert_eq!(
            http.get("url").and_then(Value::as_str),
            Some("https://mcp.example.com/mcp")
        );
        // `transport: sse` maps to the canonical `sse` type.
        let sse = hermes_entry_to_canonical(
            &hermes_entry("url: http://localhost:8000/sse\ntransport: sse\n"),
            "remote-sse",
        )
        .expect("sse canonical");
        assert_eq!(sse.get("type").and_then(Value::as_str), Some("sse"));
    }

    #[test]
    fn canonical_to_hermes_entry_drops_type_and_maps_transport() {
        // stdio → command/args/env, no `type`/`transport` keys.
        let stdio = canonical_to_hermes_entry(&json!({
            "type": "stdio",
            "command": "uvx",
            "args": ["some-server"],
            "env": {"KEY": "v"},
        }))
        .expect("stdio entry");
        let map = stdio.as_mapping().expect("mapping");
        assert!(map.contains_key(serde_yaml::Value::String("command".into())));
        assert!(!map.contains_key(serde_yaml::Value::String("type".into())));
        assert!(!map.contains_key(serde_yaml::Value::String("transport".into())));

        // sse → url + `transport: sse`, no `type`; mTLS keys pass through.
        let sse = canonical_to_hermes_entry(&json!({
            "type": "sse",
            "url": "https://x/sse",
            "headers": {"Authorization": "Bearer t"},
            "client_cert": "/tmp/cert.pem",
        }))
        .expect("sse entry");
        let map = sse.as_mapping().expect("mapping");
        assert_eq!(
            map.get(serde_yaml::Value::String("transport".into()))
                .and_then(serde_yaml::Value::as_str),
            Some("sse")
        );
        assert!(!map.contains_key(serde_yaml::Value::String("type".into())));
        assert_eq!(
            map.get(serde_yaml::Value::String("client_cert".into()))
                .and_then(serde_yaml::Value::as_str),
            Some("/tmp/cert.pem")
        );
    }

    #[test]
    fn hermes_mcp_canonical_round_trips() {
        // canonical → hermes entry → canonical is stable for both transports.
        for spec in [
            json!({"type": "stdio", "command": "npx", "args": ["-y", "srv"], "env": {"A": "b"}}),
            json!({"type": "sse", "url": "https://x/sse", "headers": {"H": "v"}}),
            json!({"type": "http", "url": "https://x/mcp"}),
        ] {
            let entry = canonical_to_hermes_entry(&spec).expect("to entry");
            let back = hermes_entry_to_canonical(&entry, "srv").expect("from entry");
            let canonical = canonicalize_spec(&spec, "expected").expect("canonical");
            assert_eq!(back, canonical, "round-trip mismatch for {spec}");
        }
    }
