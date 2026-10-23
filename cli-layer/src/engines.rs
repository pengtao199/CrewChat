//! CLI engine registry.
//!
//! The adapters themselves are external (official CLIs, Zed/`agentclientprotocol`
//! adapters, or community packages); this file only records how to install and
//! launch them. Versions and commands mirror codeg's `acp/registry.rs` pins.
//!
//! Per-role model injection is only implemented for Claude / Codex / OpenCode.
//! Every other engine is list/install/launch only; its model comes from the CLI's
//! own configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use agent_client_protocol::AcpAgent;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distribution {
    Npx,
    Binary,
}

/// Which CLI home overlay we know how to build for per-role models.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayKind {
    Claude,
    Codex,
    OpenCode,
    None,
}

#[derive(Clone, Copy, Debug)]
pub struct EngineSpec {
    pub agent_type: &'static str,
    pub aliases: &'static [&'static str],
    pub name: &'static str,
    pub description: &'static str,
    pub distribution: Distribution,
    /// npm package, without a version.
    pub package: &'static str,
    pub version: &'static str,
    pub cmd: &'static str,
    pub args: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
    /// (platform, URL with `{version}`). Empty for npx engines.
    pub platforms: &'static [(&'static str, &'static str)],
    pub overlay: OverlayKind,
    pub supports_mcp: bool,
}

const OPENCODE_PLATFORMS: &[(&str, &str)] = &[
    (
        "darwin-aarch64",
        "https://github.com/anomalyco/opencode/releases/download/v{version}/opencode-darwin-arm64.zip",
    ),
    (
        "darwin-x86_64",
        "https://github.com/anomalyco/opencode/releases/download/v{version}/opencode-darwin-x64.zip",
    ),
    (
        "linux-aarch64",
        "https://github.com/anomalyco/opencode/releases/download/v{version}/opencode-linux-arm64.tar.gz",
    ),
    (
        "linux-x86_64",
        "https://github.com/anomalyco/opencode/releases/download/v{version}/opencode-linux-x64.tar.gz",
    ),
    (
        "windows-aarch64",
        "https://github.com/anomalyco/opencode/releases/download/v{version}/opencode-windows-arm64.zip",
    ),
    (
        "windows-x86_64",
        "https://github.com/anomalyco/opencode/releases/download/v{version}/opencode-windows-x64.zip",
    ),
];

const CURSOR_PLATFORMS: &[(&str, &str)] = &[
    (
        "darwin-aarch64",
        "https://downloads.cursor.com/lab/{version}/darwin/arm64/agent-cli-package.tar.gz",
    ),
    (
        "darwin-x86_64",
        "https://downloads.cursor.com/lab/{version}/darwin/x64/agent-cli-package.tar.gz",
    ),
    (
        "linux-aarch64",
        "https://downloads.cursor.com/lab/{version}/linux/arm64/agent-cli-package.tar.gz",
    ),
    (
        "linux-x86_64",
        "https://downloads.cursor.com/lab/{version}/linux/x64/agent-cli-package.tar.gz",
    ),
    (
        "windows-aarch64",
        "https://downloads.cursor.com/lab/{version}/windows/arm64/agent-cli-package.zip",
    ),
    (
        "windows-x86_64",
        "https://downloads.cursor.com/lab/{version}/windows/x64/agent-cli-package.zip",
    ),
];

const ANTIGRAVITY_ARGS: &[&str] = if cfg!(target_os = "linux") {
    &["--uid="]
} else {
    &[]
};

const ANTIGRAVITY_PLATFORMS: &[(&str, &str)] = &[
    (
        "darwin-aarch64",
        "https://dl.google.com/agy-extensions/releases/macos/agy-acp-server-{version}-darwin-arm64.zip",
    ),
    (
        "darwin-x86_64",
        "https://dl.google.com/agy-extensions/releases/macos/agy-acp-server-{version}-darwin-x86_64.zip",
    ),
    (
        "linux-aarch64",
        "https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-{version}-linux-arm64.zip",
    ),
    (
        "linux-x86_64",
        "https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-{version}-linux-x86_64.zip",
    ),
];

pub const ENGINES: &[EngineSpec] = &[
    EngineSpec {
        agent_type: "claude_code",
        aliases: &["claude-code", "claude"],
        name: "Claude Code",
        description: "ACP wrapper for Anthropic's Claude",
        distribution: Distribution::Npx,
        package: "@agentclientprotocol/claude-agent-acp",
        version: "0.89.1",
        cmd: "claude-agent-acp",
        args: &[],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::Claude,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "codex",
        aliases: &[],
        name: "Codex CLI",
        description: "ACP adapter for OpenAI's coding assistant",
        distribution: Distribution::Npx,
        package: "@agentclientprotocol/codex-acp",
        version: "2.2.2",
        cmd: "codex-acp",
        args: &[],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::Codex,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "open_code",
        aliases: &["opencode", "open-code"],
        name: "OpenCode",
        description: "The open source coding agent",
        distribution: Distribution::Binary,
        package: "",
        version: "1.18.35",
        cmd: "opencode",
        args: &["acp"],
        env: &[],
        platforms: OPENCODE_PLATFORMS,
        overlay: OverlayKind::OpenCode,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "gemini",
        aliases: &[],
        name: "Gemini CLI",
        description: "Google's official CLI for Gemini",
        distribution: Distribution::Npx,
        package: "@google/gemini-cli",
        version: "0.60.0",
        cmd: "gemini",
        args: &["--acp", "--skip-trust"],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "openclaw",
        aliases: &["open-claw"],
        name: "OpenClaw",
        description: "Personal AI assistant you run on your own devices",
        distribution: Distribution::Npx,
        package: "openclaw",
        version: "2026.9.8",
        cmd: "openclaw",
        args: &["acp"],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: false,
    },
    EngineSpec {
        agent_type: "cline",
        aliases: &[],
        name: "Cline",
        description: "Autonomous coding agent CLI",
        distribution: Distribution::Npx,
        package: "cline",
        version: "3.0.70",
        cmd: "cline",
        args: &["--acp"],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "hermes",
        aliases: &[],
        name: "Hermes Agent",
        description: "Nous Research's self-improving agent",
        distribution: Distribution::Npx,
        package: "hermes-agent",
        version: "0.21.6",
        cmd: "hermes",
        args: &["acp"],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "codebuddy",
        aliases: &["code-buddy"],
        name: "CodeBuddy",
        description: "Tencent Cloud's official AI coding assistant",
        distribution: Distribution::Npx,
        package: "@tencent-ai/codebuddy-code",
        version: "2.164.0",
        cmd: "codebuddy",
        args: &["--acp"],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "kimi_code",
        aliases: &["kimi", "kimi-code"],
        name: "Kimi Code",
        description: "Moonshot AI's official CLI coding assistant",
        distribution: Distribution::Npx,
        package: "@moonshot-ai/kimi-code",
        version: "2.1.1",
        cmd: "kimi",
        args: &["acp"],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "pi",
        aliases: &[],
        name: "Pi",
        description: "Self-extensible coding agent (ACP via pi-acp)",
        distribution: Distribution::Npx,
        package: "pi-acp",
        version: "0.0.34",
        cmd: "pi-acp",
        args: &[],
        env: &[("PI_ACP_ENABLE_EMBEDDED_CONTEXT", "true")],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "grok",
        aliases: &[],
        name: "Grok",
        description: "xAI's official coding agent and CLI",
        distribution: Distribution::Npx,
        package: "@xai-official/grok",
        version: "1.0.51",
        cmd: "grok",
        args: &["--no-auto-update", "agent", "stdio"],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "cursor",
        aliases: &[],
        name: "Cursor",
        description: "Cursor's coding agent (ACP via cursor-agent acp)",
        distribution: Distribution::Binary,
        package: "",
        version: "2026.10.01-14929f9",
        cmd: "cursor-agent",
        args: &["acp"],
        env: &[],
        platforms: CURSOR_PLATFORMS,
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "deepseek",
        aliases: &[],
        name: "DeepSeek Harness",
        description: "Editor-facing DeepSeek agent (ACP via deepseek-acp)",
        distribution: Distribution::Npx,
        package: "deepseek-acp",
        version: "0.9.0",
        cmd: "deepseek-acp",
        args: &[],
        env: &[],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "qoder",
        aliases: &[],
        name: "Qoder",
        description: "Alibaba's Qoder coding agent CLI",
        distribution: Distribution::Npx,
        package: "@qoder-ai/qodercli",
        version: "1.1.67",
        cmd: "qoder",
        args: &["--acp"],
        env: &[("QODER_EXPOSE_TOKEN_USAGE", "1")],
        platforms: &[],
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
    EngineSpec {
        agent_type: "antigravity",
        aliases: &[],
        name: "Google Antigravity",
        description: "Google's first-party ACP server (needs auth settings)",
        distribution: Distribution::Binary,
        package: "",
        version: "1.3.0",
        cmd: "agy_acp_server",
        args: ANTIGRAVITY_ARGS,
        env: &[],
        platforms: ANTIGRAVITY_PLATFORMS,
        overlay: OverlayKind::None,
        supports_mcp: true,
    },
];

pub fn find(agent_type: &str) -> Option<&'static EngineSpec> {
    let needle = agent_type.trim();
    ENGINES.iter().find(|spec| {
        spec.agent_type.eq_ignore_ascii_case(needle)
            || spec
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(needle))
    })
}

pub fn current_platform() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("darwin-aarch64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("darwin-x86_64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("linux-aarch64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        Some("windows-aarch64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else {
        None
    }
}

#[derive(Clone)]
pub struct Engines {
    config: Arc<Config>,
}

impl Engines {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub fn specs(&self) -> &'static [EngineSpec] {
        ENGINES
    }

    pub fn find(&self, agent_type: &str) -> Option<&'static EngineSpec> {
        find(agent_type)
    }

    /// The command used to spawn the engine. A locally installed adapter wins
    /// over the npx/global fallback; paths are quoted because the data dir
    /// contains `Application Support`.
    pub fn command(&self, spec: &EngineSpec) -> String {
        if let Some(local) = self.local_binary(spec) {
            return with_args(&quote_path(&local.to_string_lossy()), spec.args);
        }
        let base = match spec.agent_type {
            "claude_code" => self.config.claude_command.clone(),
            "codex" => self.config.codex_command.clone(),
            "open_code" => self.config.opencode_command.clone(),
            _ => match spec.distribution {
                Distribution::Npx => format!("npx -y {}@{}", spec.package, spec.version),
                Distribution::Binary => spec.cmd.to_string(),
            },
        };
        with_args(&base, spec.args)
    }

    pub fn install_dir(&self, spec: &EngineSpec) -> PathBuf {
        self.config.data_dir.join("engines").join(spec.agent_type)
    }

    pub fn local_binary(&self, spec: &EngineSpec) -> Option<PathBuf> {
        let dir = self.install_dir(spec);
        let path = match spec.distribution {
            Distribution::Npx => dir.join("node_modules/.bin").join(spec.cmd),
            // Cursor and Antigravity extract a whole tree; their entry point
            // may be nested (`dist-package/cursor-agent`) or renamed
            // (`agy_acp_server.par`).
            Distribution::Binary => return find_binary_in(&dir, spec.cmd),
        };
        path.is_file().then_some(path)
    }

    pub fn installed_version(&self, spec: &EngineSpec) -> Option<String> {
        match spec.distribution {
            Distribution::Npx => {
                let path = self
                    .install_dir(spec)
                    .join("node_modules")
                    .join(spec.package)
                    .join("package.json");
                let raw = std::fs::read_to_string(path).ok()?;
                let value: Value = serde_json::from_str(&raw).ok()?;
                value
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }
            Distribution::Binary => {
                let dir = self.install_dir(spec);
                if let Ok(version) = std::fs::read_to_string(dir.join(".version")) {
                    let version = version.trim();
                    if !version.is_empty() {
                        return Some(version.to_string());
                    }
                }
                let binary = self
                    .local_binary(spec)
                    .unwrap_or_else(|| PathBuf::from(spec.cmd));
                let output = std::process::Command::new(binary)
                    .arg("--version")
                    .output()
                    .ok()?;
                if !output.status.success() {
                    return None;
                }
                parse_version(&String::from_utf8_lossy(&output.stdout))
            }
        }
    }

    pub fn list(&self) -> Vec<Value> {
        self.specs()
            .iter()
            .map(|spec| {
                let command = self.command(spec);
                let available = self.local_binary(spec).is_some() || command_available(&command);
                json!({
                    "agent_type": spec.agent_type,
                    "name": spec.name,
                    "description": spec.description,
                    "available": available,
                    "enabled": true,
                    "installed_version": self.installed_version(spec).map(Value::String).unwrap_or(Value::Null),
                    "registry_version": spec.version,
                    "supports_custom_version": true,
                    "distribution_type": match spec.distribution {
                        Distribution::Npx => "npx",
                        Distribution::Binary => "binary",
                    },
                    "is_acp_adapter": true,
                    "supports_mcp": spec.supports_mcp,
                    "env": {},
                })
            })
            .collect()
    }

    pub fn acp_agent(&self, spec: &EngineSpec, env: &BTreeMap<String, String>) -> Result<AcpAgent> {
        let command = self.command(spec);
        tracing::debug!("spawning {}: {command}", spec.agent_type);
        let mut config = AcpAgent::from_str(&command)
            .with_context(|| format!("parse ACP command for {}", spec.agent_type))?
            .into_config();
        for (name, value) in spec.env {
            config = config.env(*name, *value);
        }
        for (name, value) in env {
            config = config.env(name, value);
        }
        Ok(AcpAgent::new(config))
    }
}

fn find_binary_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if path.is_file() && (file_name == name || file_name.starts_with(&format!("{name}."))) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_binary_in(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

fn with_args(base: &str, args: &[&str]) -> String {
    if args.is_empty() {
        base.to_string()
    } else {
        format!("{} {}", base, args.join(" "))
    }
}

fn quote_path(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn parse_version(raw: &str) -> Option<String> {
    raw.lines()
        .next()
        .map(str::trim)
        .map(|line| line.trim_matches('"'))
        .map(|line| line.trim_start_matches('v'))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}

fn command_available(command: &str) -> bool {
    let Some(program) = command.split_whitespace().next() else {
        return false;
    };
    if program.contains('/') {
        return PathBuf::from(program.trim_matches(['\'', '"'])).is_file();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_builtin_engines() {
        assert_eq!(ENGINES.len(), 15);
        assert!(find("claude_code").is_some());
        assert!(find("opencode").is_some());
        assert!(find("kimi").is_some());
        assert!(find("nope").is_none());
    }

    #[test]
    fn finds_nested_and_suffixed_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("dist-package");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("cursor-agent"), b"x").unwrap();
        assert_eq!(
            find_binary_in(dir.path(), "cursor-agent")
                .unwrap()
                .file_name()
                .unwrap(),
            "cursor-agent"
        );
        let flat = tempfile::tempdir().unwrap();
        std::fs::write(flat.path().join("agy_acp_server.par"), b"x").unwrap();
        assert!(find_binary_in(flat.path(), "agy_acp_server").is_some());
    }

    #[test]
    fn only_three_engines_have_role_overlays() {
        let overlays = ENGINES
            .iter()
            .filter(|spec| spec.overlay != OverlayKind::None)
            .map(|spec| spec.agent_type)
            .collect::<Vec<_>>();
        assert_eq!(overlays, vec!["claude_code", "codex", "open_code"]);
    }
}
