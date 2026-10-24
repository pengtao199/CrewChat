//! Permission modes advertised by each ACP engine.
//!
//! The IDs are session-mode IDs, not the four per-request approval options.
//! ACP adapters advertise their own modes; this catalog is the product fallback
//! and the source for the Agents page before a session is started.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolRule {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub path_prefix: String,
    pub action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    Allow,
    Ask,
    Deny,
}

impl PermissionAction {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

impl ToolRule {
    pub fn action(&self) -> Option<PermissionAction> {
        PermissionAction::parse(&self.action)
    }

    pub fn matches(&self, tool: &str, kind: &str, paths: &[String]) -> bool {
        if !self.tool.is_empty() && !self.tool.eq_ignore_ascii_case(tool) {
            return false;
        }
        if !self.kind.is_empty() && !self.kind.eq_ignore_ascii_case(kind) {
            return false;
        }
        if !self.path_prefix.is_empty()
            && !paths.iter().any(|path| path.starts_with(&self.path_prefix))
        {
            return false;
        }
        true
    }
}

/// First matching rule wins. An invalid action is treated as a hard deny.
pub fn match_tool_rule<'a>(
    rules: &'a [ToolRule],
    tool: &str,
    kind: &str,
    paths: &[String],
) -> Option<&'a ToolRule> {
    rules.iter().find(|rule| rule.matches(tool, kind, paths))
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionMode {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionFace {
    pub engine: String,
    pub supports_session_mode: bool,
    pub supports_auto_approve: bool,
    pub modes: Vec<PermissionMode>,
}

impl PermissionFace {
    #[cfg(test)]
    pub fn has_mode(&self, id: &str) -> bool {
        self.modes.iter().any(|mode| mode.id == id)
    }
}

pub fn face(engine: &str) -> PermissionFace {
    PermissionFace {
        engine: engine.to_string(),
        supports_session_mode: matches!(engine, "claude_code" | "codex" | "open_code"),
        supports_auto_approve: true,
        modes: modes(engine),
    }
}

fn modes(engine: &str) -> Vec<PermissionMode> {
    let mut modes = vec![mode(
        "",
        "Engine default",
        "Keep the engine's own default permission mode.",
        "default",
    )];
    match engine {
        "claude_code" => modes.extend([
            mode(
                "acceptEdits",
                "Accept edits",
                "Automatically accept file edits.",
                "native",
            ),
            mode("plan", "Plan", "Plan before making changes.", "native"),
            mode(
                "auto",
                "Auto",
                "Let Claude decide permission requests.",
                "native",
            ),
            mode(
                "bypassPermissions",
                "Bypass permissions",
                "Accept all permissions.",
                "native",
            ),
        ]),
        "codex" => modes.extend([
            mode(
                "read-only",
                "Read-only",
                "Requires approval to edit files and access the internet.",
                "native",
            ),
            mode(
                "workspace-write",
                "Workspace access",
                "Edit workspace files; ask before writing outside it.",
                "native",
            ),
            mode(
                "agent",
                "Auto review",
                "Only ask for actions detected as potentially unsafe.",
                "native",
            ),
            mode(
                "agent-full-access",
                "Full access",
                "Unrestricted filesystem and network access.",
                "native",
            ),
        ]),
        "open_code" => modes.extend([
            mode(
                "build",
                "Build",
                "Execute tools using OpenCode permissions.",
                "native",
            ),
            mode(
                "plan",
                "Plan",
                "Read-only agent for exploring and planning.",
                "native",
            ),
        ]),
        _ => {}
    }
    modes
}

fn mode(id: &str, name: &str, description: &str, source: &str) -> PermissionMode {
    PermissionMode {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        source: source.to_string(),
    }
}

/// Resolve a stored Agent mode to the ACP session mode id.
///
/// `Ok(None)` means "leave the adapter default alone". `Err` means the mode is
/// not safe to apply and readiness must reject it.
pub fn resolve_mode(engine: &str, requested: &str) -> Result<Option<String>, String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Ok(None);
    }
    let resolved = match engine {
        "claude_code" => match requested {
            "acceptEdits" | "plan" | "auto" | "bypassPermissions" => Some(requested),
            "dontAsk" => {
                return Err("Claude ACP does not advertise the dontAsk session mode".to_string())
            }
            other => return Err(format!("unknown Claude permission mode '{other}'")),
        },
        "codex" => match requested {
            "read-only" | "workspace-write" | "agent" | "agent-full-access" => Some(requested),
            "dontAsk" => return Err("Codex ACP does not expose a dontAsk session mode".to_string()),
            other => return Err(format!("unknown Codex permission mode '{other}'")),
        },
        "open_code" => match requested {
            "build" | "plan" => Some(requested),
            other => {
                return Err(format!(
                    "OpenCode does not support ACP session mode '{other}'"
                ))
            }
        },
        other => {
            return Err(format!(
                "engine '{other}' does not expose ACP permission modes"
            ))
        }
    };
    Ok(resolved.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::{face, resolve_mode};

    #[test]
    fn exposes_native_modes_per_engine() {
        let claude = face("claude_code");
        assert!(claude.has_mode("acceptEdits"));
        assert!(claude.has_mode("bypassPermissions"));
        assert!(!claude.has_mode("dontAsk"));

        let codex = face("codex");
        assert!(codex.has_mode("workspace-write"));
        assert!(codex.has_mode("agent-full-access"));

        let opencode = face("open_code");
        assert!(opencode.has_mode("plan"));
        assert!(!opencode.has_mode("acceptEdits"));
    }

    #[test]
    fn accepts_native_modes_and_rejects_old_aliases() {
        assert_eq!(
            resolve_mode("codex", "workspace-write").unwrap().as_deref(),
            Some("workspace-write")
        );
        assert_eq!(
            resolve_mode("claude_code", "acceptEdits")
                .unwrap()
                .as_deref(),
            Some("acceptEdits")
        );
        assert!(resolve_mode("codex", "acceptEdits").is_err());
        assert!(resolve_mode("codex", "plan").is_err());
        assert!(resolve_mode("claude_code", "dontAsk").is_err());
        assert!(resolve_mode("claude_code", "default").is_err());
        assert!(resolve_mode("open_code", "acceptEdits").is_err());
        assert_eq!(resolve_mode("claude_code", "").unwrap(), None);
    }
}
