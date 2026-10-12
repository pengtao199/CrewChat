// custom_registry submodule: distribution specs + CustomAgentDef (split from acp/custom_registry.rs —
// mechanical move, see NOTICE.md).

use std::collections::{BTreeMap};
use serde::{Deserialize, Serialize};


/// Which distribution channel a custom agent launches through. Stored as a
/// column so an agent that publishes BOTH a binary and an npx package (the
/// ACP registry has two such entries) has a stable, user-chosen answer rather
/// than an implicit preference that could flip between releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomDistributionKind {
    Npx,
    Uvx,
    Binary,
}

impl CustomDistributionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CustomDistributionKind::Npx => "npx",
            CustomDistributionKind::Uvx => "uvx",
            CustomDistributionKind::Binary => "binary",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "npx" => Some(CustomDistributionKind::Npx),
            "uvx" => Some(CustomDistributionKind::Uvx),
            "binary" => Some(CustomDistributionKind::Binary),
            _ => None,
        }
    }
}

/// npx channel — mirrors the registry's `distribution.npx`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NpxSpec {
    /// Full npm spec including the version, e.g. `@qwen-code/qwen-code@0.21.0`.
    pub package: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Console-script name the package installs (e.g. `qwen` for
    /// `@qwen-code/qwen-code`). The ACP registry does NOT carry this, so it is
    /// resolved from npm metadata when adding through the picker and falls
    /// back to [`derive_command_name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    /// Minimum Node.js version, surfaced by preflight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_required: Option<String>,
}

/// uvx channel — mirrors the registry's `distribution.uvx`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UvxSpec {
    /// `uvx --from` spec, e.g. `fast-agent-acp==0.9.24`.
    pub package: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Console-script entry point. See [`NpxSpec::cmd`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_required: Option<String>,
    /// Interpreter to pin via `uvx --python <ver>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
}

/// One platform's binary release — mirrors the registry's
/// `distribution.binary.<platform>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BinaryPlatformSpec {
    /// Archive download URL.
    #[serde(alias = "url")]
    pub archive: String,
    /// Launch path INSIDE the archive, registry-style (`./goose`,
    /// `dist-package/cursor-agent`, `amp-acp.exe`).
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Hex SHA-256 of the archive. Verified before use when present — custom
    /// agents download from user-supplied URLs, unlike built-ins whose URLs are
    /// repository constants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// The stored `spec_json`: the ACP registry `distribution` object, verbatim.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomAgentSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npx: Option<NpxSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uvx: Option<UvxSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub binary: BTreeMap<String, BinaryPlatformSpec>,
}

/// Where a definition came from. A `Manual` definition's `version` is whatever
/// the user typed (or the fallback), so there is no meaningful "registry
/// version" to compare the installed one against — the version-status display
/// keys off this to show the local version alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomAgentSource {
    #[default]
    Registry,
    Manual,
}

impl CustomAgentSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            CustomAgentSource::Registry => "registry",
            CustomAgentSource::Manual => "manual",
        }
    }

    /// Unknown strings fall back to `Registry` — the value that keeps the
    /// display behavior rows had before the column existed.
    pub fn parse(s: &str) -> Self {
        match s {
            "manual" => CustomAgentSource::Manual,
            _ => CustomAgentSource::Registry,
        }
    }
}

fn is_registry_source(source: &CustomAgentSource) -> bool {
    *source == CustomAgentSource::Registry
}

/// A full custom-agent definition as stored in the `custom_agent` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomAgentDef {
    pub registry_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Display/pin version. Defaults to [`FALLBACK_VERSION`] when neither the
    /// registry nor the user supplied one.
    pub version: String,
    pub distribution_kind: CustomDistributionKind,
    pub spec: CustomAgentSpec,
    /// The agent's icon, for display only — never used to launch anything.
    ///
    /// Normally a `data:image/…;base64,…` URL: the add path downloads the
    /// registry's SVG once (see `commands::custom_agents::inline_icon`) and the
    /// manual form reads an uploaded file, so rendering needs no network and
    /// works offline and in the server build alike. A plain `https://` URL is
    /// still accepted (definitions stored before inlining existed, or a
    /// hand-written one) and is passed through to the webview as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// User declaration that this agent reads the shared `.agents/skills`
    /// store (`~/.agents/skills` plus project-local `.agents/skills`) — the
    /// cross-agent convention OpenCode, Gemini, Cline, Codex, pi, and Cursor
    /// already follow. codeg cannot detect where an arbitrary ACP agent loads
    /// skills from, so this stays off until the user turns it on; together
    /// with [`Self::skills_dir`] it is the gate that puts the agent into
    /// every skills matrix (see `skill_storage_spec`).
    #[serde(default)]
    pub skills_shared_store: bool,
    /// Absolute path of a directory this agent loads skills from — its own,
    /// dedicated store for agents that do not follow the shared convention.
    /// Declared by the user like [`Self::skills_shared_store`], and the two
    /// compose: either alone is enough to reach the skills surfaces, and when
    /// both are set the dedicated directory is listed first so linking targets
    /// it (the pi/Cursor ordering). Normalized at save time (`~` expanded,
    /// must be absolute) by `commands::custom_agents::normalize_skills_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_dir: Option<String>,
    /// [`CustomAgentSource::Registry`] is skipped when serializing so
    /// registry-added definitions keep their pre-column JSON shape (the def
    /// JSON participates in cache fingerprints).
    #[serde(default, skip_serializing_if = "is_registry_source")]
    pub source: CustomAgentSource,
    /// Optional user-supplied command that prints the locally installed
    /// version (e.g. `qwen --version`). `None` means "run the agent command
    /// with `--version`". Consulted by the system-install version probe only;
    /// never used to launch the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_probe: Option<String>,
    /// User declaration that the agent accepts MCP servers over the ACP wire
    /// (`session/new`'s `mcpServers`) — which, for a custom agent, means the
    /// built-in codeg-mcp companion (delegation, live feedback, task tools).
    ///
    /// Not every ACP agent does: some reject any entry in that field and fail
    /// session creation outright, so a custom agent that codeg cannot connect
    /// to at all is turned off here (OpenClaw is the built-in precedent — see
    /// [`AcpAgentMeta::supports_mcp`], the single gate both feed).
    ///
    /// Defaults ON, and is skipped when serializing at that default, so
    /// definitions stored before this field existed keep both their behaviour
    /// and their fingerprint.
    #[serde(default = "default_supports_mcp", skip_serializing_if = "is_default_supports_mcp")]
    pub supports_mcp: bool,
}

/// See [`CustomAgentDef::supports_mcp`] — the wire default is "forward MCP",
/// which is what every agent but OpenClaw has accepted.
fn default_supports_mcp() -> bool {
    true
}

fn is_default_supports_mcp(supports_mcp: &bool) -> bool {
    *supports_mcp
}

/// Version stamped on a definition that carries none. Used as a cache key and
/// as the displayed "registry version", never as an install pin (npx/uvx pin
/// through the `package` spec, which embeds its own version).
pub const FALLBACK_VERSION: &str = "custom";

/// Derive a plausible console-script name from a package spec when the
/// definition does not state one.
///
/// `@qwen-code/qwen-code@0.21.0` → `qwen-code`; `fast-agent-acp==0.9.24` →
/// `fast-agent-acp`; `hermes-agent[acp,mcp]==0.19.0` → `hermes-agent`. This is
/// a guess — many packages install a bin whose name differs from the package
/// (e.g. `@github/copilot` installs `copilot`) — so the add path prefers the
/// authoritative value from npm metadata and only falls back here.
pub fn derive_command_name(package: &str) -> String {
    let mut s = package;
    // Strip a PEP 508 version specifier / extras (`pkg[a,b]==1.2.3`).
    for sep in ["==", ">=", "<=", "~=", "!=", ">", "<"] {
        if let Some(idx) = s.find(sep) {
            s = &s[..idx];
        }
    }
    if let Some(idx) = s.find('[') {
        s = &s[..idx];
    }
    // Strip an npm version suffix, being careful not to eat a leading scope
    // `@` (`@scope/name@1.2.3` — the version `@` is the one after the slash).
    let search_from = if s.starts_with('@') {
        s.find('/').map(|i| i + 1).unwrap_or(0)
    } else {
        0
    };
    if let Some(idx) = s[search_from..].find('@') {
        s = &s[..search_from + idx];
    }
    // Drop the npm scope.
    if let Some(idx) = s.rfind('/') {
        s = &s[idx + 1..];
    }
    s.trim().to_string()
}
