//! CLI engine registry. Phase 1 ships Claude/Codex/OpenCode only.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use agent_client_protocol::AcpAgent;
use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl EngineKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude_code" | "claude-code" | "claude" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "open_code" | "opencode" | "open-code" => Some(Self::OpenCode),
            _ => None,
        }
    }

    pub fn agent_type(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::OpenCode => "open_code",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Anthropic Claude Code, via its ACP adapter",
            Self::Codex => "OpenAI Codex, via its ACP adapter",
            Self::OpenCode => "OpenCode, via its ACP subcommand",
        }
    }

    pub fn distribution(self) -> &'static str {
        match self {
            Self::ClaudeCode | Self::Codex => "npx",
            Self::OpenCode => "binary",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    Npx,
    Binary,
}

#[derive(Clone, Copy, Debug)]
pub struct InstallSpec {
    pub agent_type: &'static str,
    pub package: &'static str,
    pub cmd: &'static str,
    pub default_version: &'static str,
    pub registry_version: &'static str,
    pub args: &'static [&'static str],
    pub kind: InstallKind,
}

pub fn install_spec(kind: EngineKind) -> InstallSpec {
    match kind {
        EngineKind::ClaudeCode => InstallSpec {
            agent_type: "claude_code",
            package: "@agentclientprotocol/claude-agent-acp",
            cmd: "claude-agent-acp",
            default_version: "0.89.1",
            registry_version: "0.89.1",
            args: &[],
            kind: InstallKind::Npx,
        },
        EngineKind::Codex => InstallSpec {
            agent_type: "codex",
            package: "@agentclientprotocol/codex-acp",
            cmd: "codex-acp",
            default_version: "2.2.2",
            registry_version: "2.2.2",
            args: &[],
            kind: InstallKind::Npx,
        },
        EngineKind::OpenCode => InstallSpec {
            agent_type: "open_code",
            package: "anomalyco/opencode",
            cmd: "opencode",
            default_version: "1.18.35",
            registry_version: "1.18.35",
            args: &["acp"],
            kind: InstallKind::Binary,
        },
    }
}

/// Pi's ACP adapter is installed through npm like the other npx engines.
pub fn pi_install_spec() -> InstallSpec {
    InstallSpec {
        agent_type: "pi",
        package: "pi-acp",
        cmd: "pi-acp",
        default_version: "0.0.34",
        registry_version: "0.0.34",
        args: &[],
        kind: InstallKind::Npx,
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

    pub fn kind(&self, agent_type: &str) -> Option<EngineKind> {
        EngineKind::parse(agent_type)
    }

    /// The command used to spawn the engine. A locally installed adapter wins
    /// over the npx/global fallback; the path is quoted because the data dir
    /// contains `Application Support`.
    pub fn command(&self, kind: EngineKind) -> String {
        if let Some(local) = self.local_binary(kind) {
            let spec = install_spec(kind);
            if spec.args.is_empty() {
                return quote_path(&local.to_string_lossy());
            }
            return format!(
                "{} {}",
                quote_path(&local.to_string_lossy()),
                spec.args.join(" ")
            );
        }
        match kind {
            EngineKind::ClaudeCode => self.config.claude_command.clone(),
            EngineKind::Codex => self.config.codex_command.clone(),
            EngineKind::OpenCode => self.config.opencode_command.clone(),
        }
    }

    pub fn install_dir(&self, kind: EngineKind) -> PathBuf {
        self.config
            .data_dir
            .join("engines")
            .join(install_spec(kind).agent_type)
    }

    pub fn local_binary(&self, kind: EngineKind) -> Option<PathBuf> {
        let dir = self.install_dir(kind);
        let path = match install_spec(kind).kind {
            InstallKind::Npx => dir.join("node_modules/.bin").join(install_spec(kind).cmd),
            InstallKind::Binary => dir.join(install_spec(kind).cmd),
        };
        path.is_file().then_some(path)
    }

    pub fn installed_version(&self, kind: EngineKind) -> Option<String> {
        let spec = install_spec(kind);
        let dir = self.install_dir(kind);
        match spec.kind {
            InstallKind::Npx => {
                let package_path = dir
                    .join("node_modules")
                    .join(spec.package)
                    .join("package.json");
                let raw = std::fs::read_to_string(package_path).ok()?;
                let value: Value = serde_json::from_str(&raw).ok()?;
                value
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }
            InstallKind::Binary => {
                let binary = self
                    .local_binary(kind)
                    .unwrap_or_else(|| PathBuf::from(spec.cmd));
                let output = std::process::Command::new(binary)
                    .arg("--version")
                    .output()
                    .ok()?;
                if !output.status.success() {
                    return None;
                }
                let text = String::from_utf8_lossy(&output.stdout);
                text.split_whitespace()
                    .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
                    .map(|part| part.trim_start_matches('v').to_string())
            }
        }
    }

    /// Build the ACP agent transport with the per-session environment.
    pub fn acp_agent(&self, kind: EngineKind, env: &BTreeMap<String, String>) -> Result<AcpAgent> {
        let command = self.command(kind);
        tracing::debug!("spawning {}: {command}", kind.agent_type());
        let mut config = AcpAgent::from_str(&command)
            .with_context(|| format!("parse ACP command for {}", kind.agent_type()))?
            .into_config();
        for (name, value) in env {
            config = config.env(name, value);
        }
        Ok(AcpAgent::new(config))
    }

    pub fn list(&self) -> Vec<Value> {
        [
            EngineKind::ClaudeCode,
            EngineKind::Codex,
            EngineKind::OpenCode,
        ]
        .into_iter()
        .map(|kind| {
            let command = self.command(kind);
            let available = self.local_binary(kind).is_some() || command_available(&command);
            json!({
                "agent_type": kind.agent_type(),
                "name": kind.name(),
                "description": kind.description(),
                "available": available,
                "enabled": true,
                "installed_version": self.installed_version(kind).map(Value::String).unwrap_or(Value::Null),
                "registry_version": install_spec(kind).registry_version,
                "supports_custom_version": true,
                "distribution_type": kind.distribution(),
                "is_acp_adapter": true,
                "env": {},
            })
        })
        .collect()
    }
}

fn command_available(command: &str) -> bool {
    let Some(program) = command.split_whitespace().next() else {
        return false;
    };
    if program.contains('/') {
        return PathBuf::from(program).is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file()
    })
}

fn quote_path(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}
