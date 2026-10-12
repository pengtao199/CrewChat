// custom_registry submodule: the module's unit tests (split from acp/custom_registry.rs —
// mechanical move, see NOTICE.md).

use std::collections::{BTreeMap};
use crate::acp::registry::{AgentDistribution};
use crate::intern::{leak};
use crate::models::agent::{AgentType};

    use super::*;

    fn hydrate_guard() -> std::sync::MutexGuard<'static, ()> {
        hydrate_test_guard()
    }

    fn npx_def(id: &str) -> CustomAgentDef {
        CustomAgentDef {
            registry_id: id.to_string(),
            name: "Qwen Code".into(),
            description: "Alibaba's Qwen coding assistant".into(),
            version: "0.21.0".into(),
            distribution_kind: CustomDistributionKind::Npx,
            spec: CustomAgentSpec {
                npx: Some(NpxSpec {
                    package: "@qwen-code/qwen-code@0.21.0".into(),
                    args: vec!["--acp".into(), "--experimental-skills".into()],
                    env: BTreeMap::new(),
                    cmd: Some("qwen".into()),
                    node_required: Some("20.0.0".into()),
                }),
                ..Default::default()
            },
            icon_url: None,
            skills_shared_store: false,
            skills_dir: None,
            source: Default::default(),
            version_probe: None,
            supports_mcp: true,
        }
    }

    #[test]
    fn derives_command_names_from_package_specs() {
        assert_eq!(derive_command_name("@qwen-code/qwen-code@0.21.0"), "qwen-code");
        assert_eq!(derive_command_name("@github/copilot@1.0.75"), "copilot");
        assert_eq!(derive_command_name("cline@3.0.46"), "cline");
        assert_eq!(derive_command_name("fast-agent-acp==0.9.24"), "fast-agent-acp");
        assert_eq!(
            derive_command_name("hermes-agent[acp,mcp]==0.19.0"),
            "hermes-agent"
        );
        // A bare scoped package with no version must keep its name.
        assert_eq!(derive_command_name("@scope/thing"), "thing");
    }

    // A custom definition must not claim a BUILT-IN registry id: it would be
    // shadowed by `from_registry_id`'s built-in arms and double-listed by
    // `all_acp_agents`. This is also the hydration path that retires a
    // pre-integration `deepseek-acp` / `qoder-cli` / `antigravity-acp` custom
    // entry (kept in the DB, never published) once the id became a built-in.
    #[test]
    fn rejects_builtin_registry_id_collisions() {
        for id in [
            "deepseek-acp",
            "qoder-cli",
            "antigravity-acp",
            "claude-acp",
            "cursor",
        ] {
            assert_eq!(
                validate(&npx_def(id)),
                Err(CustomAgentError::CollidesWithBuiltin(id.to_string())),
                "{id:?} must be rejected"
            );
        }
        let _guard = hydrate_guard();
        let errors = hydrate(&[npx_def("deepseek-acp")]);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].1,
            CustomAgentError::CollidesWithBuiltin(_)
        ));
        assert!(!is_registered("deepseek-acp"));
        hydrate(&[]);
    }

    #[test]
    fn builds_npx_metadata() {
        let meta = build_meta(&npx_def("qwen-code")).expect("valid");
        assert_eq!(meta.agent_type, AgentType::custom("qwen-code").unwrap());
        assert!(meta.supports_mcp);
        assert_eq!(meta.name, "Qwen Code");
        match meta.distribution {
            AgentDistribution::Npx {
                version,
                package,
                cmd,
                args,
                node_required,
                ..
            } => {
                assert_eq!(version, "0.21.0");
                assert_eq!(package, "@qwen-code/qwen-code@0.21.0");
                assert_eq!(cmd, "qwen");
                assert_eq!(args, &["--acp", "--experimental-skills"]);
                assert_eq!(node_required, Some("20.0.0"));
            }
            other => panic!("expected npx distribution, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_definitions() {
        let mut bad_id = npx_def("qwen-code");
        bad_id.registry_id = "../escape".into();
        assert!(matches!(
            build_meta(&bad_id),
            Err(CustomAgentError::InvalidId(_))
        ));

        let mut empty_name = npx_def("qwen-code");
        empty_name.name = "   ".into();
        assert_eq!(build_meta(&empty_name).unwrap_err(), CustomAgentError::EmptyName);

        let mut wrong_channel = npx_def("qwen-code");
        wrong_channel.distribution_kind = CustomDistributionKind::Uvx;
        assert_eq!(
            build_meta(&wrong_channel).unwrap_err(),
            CustomAgentError::MissingChannel("uvx")
        );

        let mut no_package = npx_def("qwen-code");
        no_package.spec.npx.as_mut().unwrap().package = "  ".into();
        assert_eq!(
            build_meta(&no_package).unwrap_err(),
            CustomAgentError::MissingPackage("npx")
        );
    }

    fn binary_def(cmd_for_current: &str, sha: Option<&str>) -> CustomAgentDef {
        let mut binary = BTreeMap::new();
        // Cover every platform so the test runs anywhere.
        for platform in [
            "darwin-aarch64",
            "darwin-x86_64",
            "linux-aarch64",
            "linux-x86_64",
            "windows-aarch64",
            "windows-x86_64",
        ] {
            binary.insert(
                platform.to_string(),
                BinaryPlatformSpec {
                    archive: format!("https://example.com/goose-{platform}.tar.gz"),
                    cmd: cmd_for_current.to_string(),
                    args: vec!["acp".into()],
                    env: BTreeMap::new(),
                    sha256: sha.map(str::to_string),
                },
            );
        }
        CustomAgentDef {
            registry_id: "goose".into(),
            name: "goose".into(),
            description: "local agent".into(),
            version: "1.44.0".into(),
            distribution_kind: CustomDistributionKind::Binary,
            spec: CustomAgentSpec {
                binary,
                ..Default::default()
            },
            icon_url: None,
            skills_shared_store: false,
            skills_dir: None,
            source: Default::default(),
            version_probe: None,
            supports_mcp: true,
        }
    }

    #[test]
    fn maps_registry_binary_entry_to_distribution() {
        let meta = build_meta(&binary_def("./goose", Some("abc123"))).expect("valid");
        match meta.distribution {
            AgentDistribution::Binary {
                version,
                cmd,
                args,
                platforms,
                dir_entry,
                ..
            } => {
                assert_eq!(version, "1.44.0");
                // `./goose` is a single-file archive entry: strip the `./`, no
                // whole-tree extraction.
                assert_eq!(cmd, "goose");
                assert_eq!(args, &["acp"]);
                assert!(dir_entry.is_none());
                assert_eq!(platforms.len(), 6);
                assert!(platforms.iter().all(|p| p.sha256 == Some("abc123")));
                // Sorted for a stable order.
                assert_eq!(platforms[0].platform, "darwin-aarch64");
            }
            other => panic!("expected binary distribution, got {other:?}"),
        }
    }

    // Issue #905, from registry entry to extracted archive, on any host. The
    // registry's Windows entry for a single-file archive (`./kilo.exe`) has to
    // name the file that archive holds; it used to come out as `kilo.exe.exe`,
    // failing every such install at "Locating binary...".
    #[test]
    fn a_single_file_windows_entry_resolves_to_the_file_its_archive_holds() {
        use crate::acp::binary_cache::{executable_file_name_for, find_binary_recursive};

        let meta = build_meta(&binary_def("./kilo.exe", None)).expect("valid");
        let cmd = match meta.distribution {
            AgentDistribution::Binary { cmd, dir_entry, .. } => {
                // A bare entry name takes the single-file copy-out path.
                assert!(dir_entry.is_none());
                cmd
            }
            other => panic!("expected binary distribution, got {other:?}"),
        };
        assert_eq!(cmd, "kilo.exe");

        // What the install step finds after unpacking a one-member archive.
        let tmp = tempfile::tempdir().unwrap();
        let extracted = tmp.path().to_path_buf();
        std::fs::write(extracted.join("kilo.exe"), b"MZ").unwrap();
        let name = executable_file_name_for(cmd, true);
        assert_eq!(
            find_binary_recursive(&extracted, &name),
            Some(extracted.join("kilo.exe")),
            "looked for {name:?}"
        );
    }

    #[test]
    fn nested_archive_entry_switches_to_dir_tree_extraction() {
        let meta = build_meta(&binary_def("dist-package/agent", None)).expect("valid");
        match meta.distribution {
            AgentDistribution::Binary {
                cmd, dir_entry, ..
            } => {
                assert_eq!(cmd, "agent");
                let entry = dir_entry.expect("nested entry must extract the whole tree");
                assert_eq!(entry.unix, "dist-package/agent");
                assert_eq!(entry.windows, "dist-package/agent");
            }
            other => panic!("expected binary distribution, got {other:?}"),
        }
    }

    #[test]
    fn version_labels_that_would_escape_the_binary_cache_are_rejected() {
        // The version becomes a directory name under the binary cache, and that
        // directory is deleted on reinstall — so a definition must not be able
        // to steer it elsewhere.
        for bad in [
            "../../..",
            "..",
            ".",
            "a/b",
            "a\\b",
            "C:",
            ".hidden",
            "with space",
            "semi;colon",
        ] {
            let mut def = npx_def("version-guard");
            def.version = bad.to_string();
            assert!(
                matches!(build_meta(&def), Err(CustomAgentError::UnsafeVersion(_))),
                "version {bad:?} must be rejected"
            );
            assert!(validate(&def).is_err());
        }
        // Ordinary labels still pass, including an empty one (which becomes
        // FALLBACK_VERSION) and the `v`-prefixed spelling.
        for ok in ["0.21.0", "v1.2.3", "2026.07.23-e383d2b", "", "   "] {
            let mut def = npx_def("version-guard");
            def.version = ok.to_string();
            assert!(validate(&def).is_ok(), "version {ok:?} must be accepted");
        }
    }

    #[test]
    fn archive_entries_that_escape_the_extracted_tree_are_rejected() {
        // `binary_cache` joins this onto the install dir, chmod +x's the result
        // and launches it, so an absolute or `..` path would target a file of
        // the definition's choosing anywhere on disk.
        for bad in [
            "/tmp/target",
            "../../target",
            "./../../target",
            "dist/../../../target",
            "..\\..\\target.exe",
            "C:\\Windows\\System32\\cmd.exe",
            "//host/share/target",
        ] {
            let def = binary_def(bad, None);
            assert!(
                matches!(
                    build_meta(&def),
                    Err(CustomAgentError::UnsafeArchiveEntry(_))
                ),
                "archive entry {bad:?} must be rejected"
            );
        }
        // The registry's real spellings still work.
        for ok in ["./goose", "goose", "dist-package/cursor-agent", "amp-acp.exe", ""] {
            let def = binary_def(ok, None);
            assert!(
                validate(&def).is_ok(),
                "archive entry {ok:?} must be accepted"
            );
        }
    }

    #[test]
    fn an_escaping_entry_on_another_platform_is_rejected_too() {
        // `build_binary_distribution` borrows another platform family's path
        // when publishing `dir_entry`, so a hostile entry need not be this
        // machine's to reach the launch path.
        let mut def = binary_def("dist/agent", None);
        let other = if crate::acp::registry::current_platform().starts_with("windows") {
            "linux-x86_64"
        } else {
            "windows-x86_64"
        };
        def.spec.binary.get_mut(other).unwrap().cmd = "../../../../target".into();
        assert!(matches!(
            build_meta(&def),
            Err(CustomAgentError::UnsafeArchiveEntry(_))
        ));
    }

    #[test]
    fn validate_reaches_the_same_verdict_as_build_meta() {
        // The settings list and the upsert path call `validate` precisely so
        // they do not leak a throwaway meta, which only works if the two agree
        // on what is valid.
        let mut escaping_version = npx_def("agree");
        escaping_version.version = "../..".into();
        let mut no_channel = npx_def("agree");
        no_channel.distribution_kind = CustomDistributionKind::Uvx;
        let mut empty_package = npx_def("agree");
        empty_package.spec.npx.as_mut().unwrap().package = "  ".into();
        let mut bad_id = npx_def("agree");
        bad_id.registry_id = "../escape".into();
        let mut blank_name = npx_def("agree");
        blank_name.name = "  ".into();

        let cases = [
            npx_def("agree"),
            escaping_version,
            no_channel,
            empty_package,
            bad_id,
            blank_name,
            binary_def("./goose", Some("abc123")),
            binary_def("../../escape", None),
            binary_def("dist-package/agent", None),
        ];
        for def in &cases {
            assert_eq!(
                validate(def),
                build_meta(def).map(|_| ()),
                "validate and build_meta disagree about {:?}",
                def.registry_id
            );
        }
    }

    #[test]
    fn binary_without_current_platform_is_rejected() {
        let mut def = binary_def("./goose", None);
        def.spec.binary.clear();
        def.spec.binary.insert(
            "some-unreal-platform".into(),
            BinaryPlatformSpec {
                archive: "https://example.com/x.tar.gz".into(),
                cmd: "./x".into(),
                ..Default::default()
            },
        );
        assert!(matches!(
            build_meta(&def),
            Err(CustomAgentError::UnsupportedPlatform(_))
        ));
    }

    #[test]
    fn spec_json_is_paste_compatible_with_the_acp_registry() {
        // Verbatim `distribution` object from the published registry.
        let raw = r#"{
          "binary": {
            "darwin-aarch64": {
              "archive": "https://example.com/kilo-darwin-arm64.zip",
              "cmd": "./kilo",
              "args": ["acp"],
              "sha256": "0961"
            }
          },
          "npx": { "package": "kilo@7.4.16", "args": ["--acp"] }
        }"#;
        let spec: CustomAgentSpec = serde_json::from_str(raw).expect("registry shape parses");
        assert!(spec.npx.is_some());
        assert_eq!(spec.binary.len(), 1);
        assert_eq!(
            spec.binary["darwin-aarch64"].sha256.as_deref(),
            Some("0961")
        );
        // `url` is accepted as an alias for `archive` (older registry payloads).
        let aliased: CustomAgentSpec =
            serde_json::from_str(r#"{"binary":{"linux-x86_64":{"url":"https://e/x.tgz"}}}"#)
                .expect("alias parses");
        assert_eq!(aliased.binary["linux-x86_64"].archive, "https://e/x.tgz");
    }

    #[test]
    fn hydrate_publishes_and_reuses_unchanged_metadata() {
        let _guard = hydrate_guard();
        let def = npx_def("hydrate-test-agent");
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        let first = get("hydrate-test-agent").expect("registered");
        assert_eq!(display_name("hydrate-test-agent"), Some("Qwen Code"));
        assert!(is_registered("hydrate-test-agent"));
        assert!(all().contains(&AgentType::custom("hydrate-test-agent").unwrap()));

        // Re-hydrating an unchanged definition must reuse the SAME leak.
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        let second = get("hydrate-test-agent").expect("still registered");
        assert!(std::ptr::eq(first, second), "unchanged def must not re-leak");

        // Hydrating an empty set unregisters everything.
        assert!(hydrate(&[]).is_empty());
        assert!(get("hydrate-test-agent").is_none());
        assert!(!is_registered("hydrate-test-agent"));
    }

    #[test]
    fn the_skills_declaration_survives_hydrate_and_gates_by_registration() {
        let _guard = hydrate_guard();
        let mut def = npx_def("skills-flag-agent");
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        assert_eq!(
            skills_decl("skills-flag-agent"),
            CustomSkillsDecl::default(),
            "undeclared agents stay out of the skills surfaces"
        );

        // Flipping a declaration changes the fingerprint, so the same hydrate
        // path republishes and the accessor flips with it — for the shared
        // store and the dedicated directory alike.
        def.skills_shared_store = true;
        def.skills_dir = Some("/opt/agent/skills".into());
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        let decl = skills_decl("skills-flag-agent");
        assert!(decl.shared_store);
        assert_eq!(decl.dir, Some("/opt/agent/skills"));

        // A blank directory reads as undeclared rather than as an empty path.
        def.skills_dir = Some("   ".into());
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        assert_eq!(skills_decl("skills-flag-agent").dir, None);

        // An unregistered (deleted) id reads as all-off — a phantom column
        // must not survive its agent.
        assert!(hydrate(&[]).is_empty());
        assert_eq!(skills_decl("skills-flag-agent"), CustomSkillsDecl::default());
    }

    #[test]
    fn source_and_version_probe_survive_hydrate_and_gate_by_registration() {
        let _guard = hydrate_guard();
        let mut def = npx_def("provenance-agent");
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        assert_eq!(
            source_of("provenance-agent"),
            Some(CustomAgentSource::Registry)
        );
        assert_eq!(version_probe_of("provenance-agent"), None);

        def.source = CustomAgentSource::Manual;
        def.version_probe = Some("qwen --version".into());
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        assert_eq!(
            source_of("provenance-agent"),
            Some(CustomAgentSource::Manual)
        );
        assert_eq!(version_probe_of("provenance-agent"), Some("qwen --version"));

        // A blank probe reads as undeclared rather than as an empty command.
        def.version_probe = Some("   ".into());
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        assert_eq!(version_probe_of("provenance-agent"), None);

        assert!(hydrate(&[]).is_empty());
        assert_eq!(source_of("provenance-agent"), None);
    }

    #[test]
    fn the_mcp_declaration_reaches_the_launch_metadata() {
        let _guard = hydrate_guard();
        let mut def = npx_def("mcp-flag-agent");
        // On unless the user says otherwise: this is the gate
        // `connection.rs` reads to decide whether codeg-mcp goes out on
        // `session/new`.
        assert!(build_meta(&def).expect("valid").supports_mcp);

        def.supports_mcp = false;
        assert!(!build_meta(&def).expect("valid").supports_mcp);

        // And flipping it changes the fingerprint, so a save republishes the
        // meta instead of reusing the leaked one with the stale flag.
        def.supports_mcp = true;
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        assert!(get("mcp-flag-agent").expect("registered").supports_mcp);
        def.supports_mcp = false;
        assert!(hydrate(std::slice::from_ref(&def)).is_empty());
        assert!(!get("mcp-flag-agent").expect("registered").supports_mcp);

        hydrate(&[]);
    }

    #[test]
    fn a_definition_stored_before_the_mcp_flag_existed_still_forwards_mcp() {
        // The field is skipped when serializing at its default, so an older
        // stored definition has no `supports_mcp` key at all. It must read back
        // as ON — and must fingerprint identically to the same definition
        // written today, or every existing agent would re-leak a meta on the
        // first hydrate after the upgrade.
        let def = npx_def("legacy-shape-agent");
        let json = serde_json::to_string(&def).expect("serializes");
        assert!(
            !json.contains("supports_mcp"),
            "the default must not change the stored shape: {json}"
        );
        let back: CustomAgentDef = serde_json::from_str(&json).expect("parses");
        assert!(back.supports_mcp);

        // An explicit opt-out does round-trip.
        let mut off = npx_def("legacy-shape-agent");
        off.supports_mcp = false;
        let off_json = serde_json::to_string(&off).expect("serializes");
        assert!(off_json.contains("supports_mcp"));
        let off_back: CustomAgentDef = serde_json::from_str(&off_json).expect("parses");
        assert!(!off_back.supports_mcp);
    }

    #[test]
    fn a_custom_uvx_agent_advertises_its_own_cli_as_system_fallback() {
        // The uvx launch path falls back to `system_cmd` when the uv runner is
        // absent; for a custom agent the user's own install of the CLI is that
        // fallback, with the same entry-script args.
        let def = CustomAgentDef {
            registry_id: "uvx-fallback-agent".to_string(),
            name: "Fast Agent".into(),
            description: String::new(),
            version: "0.9.24".into(),
            distribution_kind: CustomDistributionKind::Uvx,
            spec: CustomAgentSpec {
                uvx: Some(UvxSpec {
                    package: "fast-agent-acp==0.9.24".into(),
                    args: vec!["--acp".into()],
                    cmd: Some("fast-agent".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            icon_url: None,
            skills_shared_store: false,
            skills_dir: None,
            source: Default::default(),
            version_probe: None,
            supports_mcp: true,
        };
        let meta = build_meta(&def).expect("builds");
        match meta.distribution {
            AgentDistribution::Uvx {
                cmd,
                args,
                system_cmd,
                ..
            } => {
                assert_eq!(system_cmd, Some((cmd, args)));
                assert_eq!(cmd, "fast-agent");
                assert_eq!(args, ["--acp"]);
            }
            _ => panic!("expected a uvx distribution"),
        }
    }

    #[test]
    fn hydrate_skips_invalid_rows_without_dropping_valid_ones() {
        let _guard = hydrate_guard();
        let good = npx_def("hydrate-good-agent");
        let mut bad = npx_def("hydrate-bad-agent");
        bad.spec.npx = None;
        let errors = hydrate(&[good, bad]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "hydrate-bad-agent");
        assert!(is_registered("hydrate-good-agent"));
        assert!(!is_registered("hydrate-bad-agent"));
        hydrate(&[]);
    }
