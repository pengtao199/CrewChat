    use super::*;

    fn base_inputs() -> DiagInputs {
        DiagInputs {
            node: CmdProbe {
                path: Some("/usr/bin/node".to_string()),
                version: Some("v20.11.1".to_string()),
            },
            npm: CmdProbe {
                path: Some("/usr/bin/npm".to_string()),
                version: Some("10.2.4".to_string()),
            },
            ..Default::default()
        }
    }

    fn agent_installed_unresolved() -> AgentDiag {
        AgentDiag {
            name: "Codex CLI".to_string(),
            cmd: "codex-acp".to_string(),
            distribution: "npx",
            db_version: Some("1.1.2".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn redact_only_masks_secretish_keys() {
        assert_eq!(redact_secret("NVM_DIR", "/home/u/.nvm"), "/home/u/.nvm");
        assert_eq!(redact_secret("SHELL", "/bin/zsh"), "/bin/zsh");
        // secret-ish key → masked, and multibyte-safe (no panic / no split).
        let masked = redact_secret("XAI_API_KEY", "sk-测试secret-1234567890");
        assert!(masked.contains('•'));
        assert!(!masked.contains("secret"));
        assert_eq!(redact_secret("MY_TOKEN", "abcd"), "••••");
    }

    #[test]
    fn verdict_node_missing() {
        let inp = DiagInputs::default();
        assert_eq!(compute_verdict(&inp).code, "node_missing");
    }

    #[test]
    fn verdict_ok_without_agent() {
        assert_eq!(compute_verdict(&base_inputs()).code, "ok");
    }

    #[test]
    fn verdict_user_prefix_not_on_path() {
        let mut inp = base_inputs();
        let mut a = agent_installed_unresolved();
        a.user_prefix_bin = Some("/home/u/.codeg/npm-global/bin/codex-acp".to_string());
        inp.agent = Some(a);
        let v = compute_verdict(&inp);
        assert_eq!(v.code, "user_prefix_not_on_path");
        assert_eq!(v.level, DiagLevel::Fail);
    }

    #[test]
    fn verdict_homebrew_bin_not_on_path() {
        let mut inp = base_inputs();
        let mut a = agent_installed_unresolved();
        a.homebrew_bin = Some("/opt/homebrew/bin/codex-acp".to_string());
        inp.agent = Some(a);
        assert_eq!(compute_verdict(&inp).code, "homebrew_bin_not_on_path");
    }

    #[test]
    fn verdict_terminal_only_path() {
        let mut inp = base_inputs();
        let mut a = agent_installed_unresolved();
        a.detected_version = Some("1.1.2".to_string());
        inp.agent = Some(a);
        inp.terminal.cmd_resolved = Some("/Users/u/.nvm/versions/node/v20/bin/codex-acp".to_string());
        assert_eq!(compute_verdict(&inp).code, "terminal_only_path");
    }

    #[test]
    fn verdict_terminal_only_path_without_npm_evidence() {
        let mut inp = base_inputs();
        inp.agent = Some(AgentDiag {
            name: "Hermes Agent".to_string(),
            cmd: "hermes".to_string(),
            distribution: "npx",
            ..Default::default()
        });
        inp.terminal.cmd_resolved = Some("/Users/u/.local/bin/hermes".to_string());
        let v = compute_verdict(&inp);
        assert_eq!(v.code, "terminal_only_path");
        assert_eq!(v.level, DiagLevel::Fail);
    }

    // That no-record check sits after the Node checks, so Node problems are
    // still reported first, as for every npx agent...
    #[test]
    fn verdict_node_missing_outranks_terminal_only_path() {
        let mut inp = DiagInputs {
            agent: Some(AgentDiag {
                name: "Hermes Agent".to_string(),
                cmd: "hermes".to_string(),
                distribution: "npx",
                ..Default::default()
            }),
            ..Default::default()
        };
        inp.terminal.cmd_resolved = Some("/Users/u/.local/bin/hermes".to_string());
        assert_eq!(compute_verdict(&inp).code, "node_missing");
    }

    // ...and before the adapter explainer: when the adapter's OWN command
    // resolves in the login shell, the adapter is installed, so "the adapter
    // is not installed" would be wrong.
    #[test]
    fn verdict_terminal_only_path_outranks_adapter_explainer() {
        let mut inp = base_inputs();
        inp.agent = Some(adapter_agent_never_installed(Some(
            "/opt/homebrew/bin/codex",
        )));
        inp.terminal.cmd_resolved = Some("/Users/u/bin/codex-acp".to_string());
        assert_eq!(compute_verdict(&inp).code, "terminal_only_path");
    }

    #[test]
    fn verdict_node_too_old() {
        let mut inp = base_inputs();
        inp.node.version = Some("v18.19.0".to_string());
        let mut a = agent_installed_unresolved();
        a.node_required = Some("20.0.0".to_string());
        a.resolve_npx = Some("/x/codex-acp".to_string()); // resolves, but node too old
        inp.agent = Some(a);
        assert_eq!(compute_verdict(&inp).code, "node_too_old");
    }

    // The floor is a full version, and diagnostics must read it the way the
    // launch preflight does: codex-acp 2.2 needs 20.3.0 (`AbortSignal.any`),
    // so a Node 20.2 that resolves the command is still too old.
    #[test]
    fn verdict_node_too_old_within_the_required_major() {
        let mut inp = base_inputs();
        inp.node.version = Some("v20.2.0".to_string());
        let mut a = agent_installed_unresolved();
        a.node_required = Some("20.3.0".to_string());
        a.resolve_npx = Some("/x/codex-acp".to_string());
        inp.agent = Some(a);
        assert_eq!(compute_verdict(&inp).code, "node_too_old");

        inp.node.version = Some("v20.3.0".to_string());
        assert_eq!(compute_verdict(&inp).code, "ok");
    }

    #[test]
    fn node_requirement_compares_whole_versions() {
        assert_eq!(node_meets_requirement("v20.2.0", "20.3.0"), Some(false));
        assert_eq!(node_meets_requirement("v20.3.0", "20.3.0"), Some(true));
        assert_eq!(node_meets_requirement("v20.19.1", "20.3.0"), Some(true));
        assert_eq!(node_meets_requirement("v22.18.0", "22.19.0"), Some(false));
        assert_eq!(node_meets_requirement("v24.0.0", "22.19.0"), Some(true));
        assert_eq!(
            node_meets_requirement("v22.0.0-nightly", "22.0.0"),
            Some(true)
        );
        // A version without its patch still compares by major.
        assert_eq!(node_meets_requirement("v20", "20.3.0"), Some(true));
        assert_eq!(node_meets_requirement("v18", "20.3.0"), Some(false));
        assert_eq!(node_meets_requirement("unknown", "20.3.0"), None);
    }

    #[test]
    fn verdict_ok_when_resolved() {
        let mut inp = base_inputs();
        let mut a = agent_installed_unresolved();
        a.resolve_npx = Some("/usr/local/bin/codex-acp".to_string());
        inp.agent = Some(a);
        assert_eq!(compute_verdict(&inp).code, "ok");
    }

    #[test]
    fn verdict_not_installed_when_nothing_recorded() {
        let mut inp = base_inputs();
        inp.agent = Some(AgentDiag {
            cmd: "codex-acp".to_string(),
            distribution: "npx",
            ..Default::default()
        });
        assert_eq!(compute_verdict(&inp).code, "not_installed");
    }

    #[test]
    fn verdict_binary_launchable_ignores_node() {
        // Binary agents (e.g. Cursor) do not need Node; a launchable cached/system
        // binary must read "ok" even with no node/npm on PATH — regression guard
        // against the npx model being applied to every distribution.
        // node & npm absent
        let inp = DiagInputs {
            agent: Some(AgentDiag {
                name: "Cursor".to_string(),
                cmd: "cursor-agent".to_string(),
                distribution: "binary",
                launchable: Some("/Users/u/.local/bin/cursor-agent".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(compute_verdict(&inp).code, "ok");
    }

    #[test]
    fn verdict_binary_recorded_but_unresolved() {
        let inp = DiagInputs {
            agent: Some(AgentDiag {
                cmd: "cursor-agent".to_string(),
                distribution: "binary",
                db_version: Some("2026.07.16".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(compute_verdict(&inp).code, "installed_but_unresolved");
    }

    #[test]
    fn verdict_uvx_launchable_ok_else_not_installed() {
        let ok = DiagInputs {
            agent: Some(AgentDiag {
                cmd: "hermes".to_string(),
                distribution: "uvx",
                launchable: Some("/Users/u/.local/bin/uvx".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(compute_verdict(&ok).code, "ok");

        let missing = DiagInputs {
            agent: Some(AgentDiag {
                cmd: "hermes".to_string(),
                distribution: "uvx",
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(compute_verdict(&missing).code, "not_installed");
    }

    fn adapter_agent_never_installed(native_path: Option<&str>) -> AgentDiag {
        AgentDiag {
            name: "Codex CLI".to_string(),
            cmd: "codex-acp".to_string(),
            distribution: "npx",
            adapter: Some(AdapterProbe {
                native_cmd: "codex".to_string(),
                native_path: native_path.map(str::to_string),
                native_version: native_path.map(|_| "codex-cli 0.145.0".to_string()),
                shared_config_dir: "~/.codex".to_string(),
            }),
            ..Default::default()
        }
    }

    // The canonical support ticket: `codex` is on the machine, the adapter is
    // not. The generic "not installed" verdict reads as codeg being wrong, so
    // this case gets its own code.
    #[test]
    fn verdict_adapter_missing_while_vendor_cli_present() {
        let mut inp = base_inputs();
        inp.agent = Some(adapter_agent_never_installed(Some("/opt/homebrew/bin/codex")));
        let v = compute_verdict(&inp);
        assert_eq!(v.code, "adapter_missing_native_present");
        assert_eq!(v.level, DiagLevel::Info);
    }

    #[test]
    fn verdict_adapter_missing_without_vendor_cli() {
        let mut inp = base_inputs();
        inp.agent = Some(adapter_agent_never_installed(None));
        assert_eq!(compute_verdict(&inp).code, "adapter_missing");
    }

    // Node problems still win: they block installing the adapter in the first
    // place, so explaining the adapter split there would bury the real fix.
    #[test]
    fn verdict_node_missing_outranks_adapter_missing() {
        let inp = DiagInputs {
            agent: Some(adapter_agent_never_installed(Some(
                "/opt/homebrew/bin/codex",
            ))),
            ..Default::default()
        };
        assert_eq!(compute_verdict(&inp).code, "node_missing");
    }

    // Once the adapter resolves the split is irrelevant — plain ok, no special
    // casing that would leave an adapter agent permanently "explaining itself".
    #[test]
    fn verdict_ok_when_adapter_resolves_even_with_vendor_cli() {
        let mut inp = base_inputs();
        let mut a = adapter_agent_never_installed(Some("/opt/homebrew/bin/codex"));
        a.resolve_npx = Some("/usr/local/bin/codex-acp".to_string());
        inp.agent = Some(a);
        assert_eq!(compute_verdict(&inp).code, "ok");
    }

    // A previously-installed adapter that stopped resolving is a PATH problem,
    // not an explainer case — the adapter branch must not swallow it.
    #[test]
    fn verdict_installed_but_unresolved_still_wins_for_adapter_agents() {
        let mut inp = base_inputs();
        let mut a = adapter_agent_never_installed(Some("/opt/homebrew/bin/codex"));
        a.db_version = Some("1.1.7".to_string());
        inp.agent = Some(a);
        assert_eq!(compute_verdict(&inp).code, "installed_but_unresolved");
    }

    // The pasted report has to answer "but I have codex installed" on its own.
    #[test]
    fn report_names_the_vendor_cli_and_shared_config_dir() {
        let mut inp = base_inputs();
        inp.agent = Some(adapter_agent_never_installed(Some("/opt/homebrew/bin/codex")));
        let r = build_report(&inp, "FIXED-TS".to_string(), Some(AgentType::Codex));
        assert!(r.plain_text.contains("codex (your own CLI)"));
        assert!(r.plain_text.contains("/opt/homebrew/bin/codex"));
        assert!(r.plain_text.contains("~/.codex"));
        assert!(r.plain_text.contains("verdict [adapter_missing_native_present]"));
    }

    // Non-adapter agents keep the old shape exactly — no stray rows.
    #[test]
    fn report_omits_adapter_rows_for_plain_agents() {
        let mut inp = base_inputs();
        inp.agent = Some(AgentDiag {
            name: "Gemini CLI".to_string(),
            cmd: "gemini".to_string(),
            distribution: "npx",
            resolve_npx: Some("/usr/local/bin/gemini".to_string()),
            ..Default::default()
        });
        let r = build_report(&inp, "FIXED-TS".to_string(), Some(AgentType::Gemini));
        assert!(!r.plain_text.contains("your own CLI"));
        assert!(!r.plain_text.contains("shared config dir"));
    }

    #[test]
    fn build_report_is_deterministic_and_includes_plain_text() {
        let inp = base_inputs();
        let r = build_report(&inp, "FIXED-TS".to_string(), None);
        assert_eq!(r.generated_at, "FIXED-TS");
        assert!(r.plain_text.contains("Codeg environment diagnostics"));
        assert!(r.plain_text.contains("verdict [ok]"));
        assert!(!r.sections.is_empty());
    }
