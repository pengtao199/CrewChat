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

    pub fn command(&self, kind: EngineKind) -> &str {
        match kind {
            EngineKind::ClaudeCode => &self.config.claude_command,
            EngineKind::Codex => &self.config.codex_command,
            EngineKind::OpenCode => &self.config.opencode_command,
        }
    }

    /// Build the ACP agent transport with the per-session environment.
    pub fn acp_agent(&self, kind: EngineKind, env: &BTreeMap<String, String>) -> Result<AcpAgent> {
        let command = self.command(kind);
        let mut config = AcpAgent::from_str(command)
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
            json!({
                "agent_type": kind.agent_type(),
                "name": kind.name(),
                "description": kind.description(),
                "available": command_available(command),
                "enabled": true,
                "installed_version": Value::Null,
                "registry_version": Value::Null,
                "supports_custom_version": false,
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
