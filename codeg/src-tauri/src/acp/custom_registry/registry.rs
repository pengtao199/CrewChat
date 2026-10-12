// custom_registry submodule: process-global launch registry (split from acp/custom_registry.rs —
// mechanical move, see NOTICE.md).
use super::*;

use std::collections::{HashMap};
use std::sync::{OnceLock, RwLock};
use crate::acp::registry::{AcpAgentMeta, AgentDistribution};
use crate::intern::{intern, leak};
use crate::models::agent::{AgentType};


struct Entry {
    fingerprint: String,
    meta: &'static AcpAgentMeta,
    /// Display icon, kept beside the launch metadata rather than inside
    /// [`AcpAgentMeta`]: it is presentation, not launch data, and every one of
    /// the twelve built-in metas would otherwise need a field it never uses.
    icon: Option<&'static str>,
    /// Whether the user declared the agent reads the shared `.agents/skills`
    /// store. Beside the meta for the same reason as `icon`: it drives the
    /// skills surfaces, not the launch.
    skills_shared_store: bool,
    /// The declared dedicated skills directory, if any. See
    /// [`CustomAgentDef::skills_dir`].
    skills_dir: Option<&'static str>,
    /// Registry-added vs hand-written. Display data like `icon`; drives the
    /// version-status presentation, not the launch.
    source: CustomAgentSource,
    /// Optional version-probe command. See [`CustomAgentDef::version_probe`].
    version_probe: Option<&'static str>,
}

fn registry() -> &'static RwLock<HashMap<&'static str, Entry>> {
    static REGISTRY: OnceLock<RwLock<HashMap<&'static str, Entry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn fingerprint(def: &CustomAgentDef) -> String {
    serde_json::to_string(def).unwrap_or_default()
}

/// Replace the published registry with `defs`.
///
/// Definitions that fail validation are skipped with a warning rather than
/// failing the whole hydrate — one malformed row must not take down every
/// other custom agent (or app startup). Returns the errors for callers that
/// want to surface them.
pub fn hydrate(defs: &[CustomAgentDef]) -> Vec<(String, CustomAgentError)> {
    let mut errors = Vec::new();
    let mut next: HashMap<&'static str, Entry> = HashMap::new();
    let previous = registry().read().unwrap_or_else(|e| e.into_inner());

    for def in defs {
        let id = intern(&def.registry_id);
        let fp = fingerprint(def);
        // Unchanged definition — reuse the already-leaked meta.
        if let Some(prev) = previous.get(id).filter(|p| p.fingerprint == fp) {
            next.insert(
                id,
                Entry {
                    fingerprint: fp,
                    meta: prev.meta,
                    icon: prev.icon,
                    skills_shared_store: prev.skills_shared_store,
                    skills_dir: prev.skills_dir,
                    source: prev.source,
                    version_probe: prev.version_probe,
                },
            );
            continue;
        }
        match build_meta(def) {
            Ok(meta) => {
                next.insert(
                    id,
                    Entry {
                        fingerprint: fp,
                        meta: leak(meta),
                        icon: def
                            .icon_url
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(intern),
                        skills_shared_store: def.skills_shared_store,
                        skills_dir: def
                            .skills_dir
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(intern),
                        source: def.source,
                        version_probe: def
                            .version_probe
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(intern),
                    },
                );
            }
            Err(e) => {
                tracing::warn!(
                    "[ACP] skipping custom agent {:?}: {e}",
                    def.registry_id.as_str()
                );
                errors.push((def.registry_id.clone(), e));
            }
        }
    }

    drop(previous);
    *registry().write().unwrap_or_else(|e| e.into_inner()) = next;
    errors
}

/// Launch metadata for a registered custom agent, or `None` when the id is not
/// registered (never registered, or deleted while conversations still point at
/// it).
pub fn get(registry_id: &str) -> Option<&'static AcpAgentMeta> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(registry_id)
        .map(|e| e.meta)
}

/// Display name for a registered custom agent.
pub fn display_name(registry_id: &str) -> Option<&'static str> {
    get(registry_id).map(|m| m.name)
}

/// A registered custom agent's skills declaration — the shared-store flag and
/// the dedicated directory, together. See [`CustomAgentDef::skills_shared_store`]
/// and [`CustomAgentDef::skills_dir`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CustomSkillsDecl {
    pub shared_store: bool,
    pub dir: Option<&'static str>,
}

/// The skills declaration for `registry_id`. Everything-off for unregistered
/// ids — a deleted agent must drop out of the skills surfaces rather than keep
/// a phantom column.
pub fn skills_decl(registry_id: &str) -> CustomSkillsDecl {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(registry_id)
        .map(|e| CustomSkillsDecl {
            shared_store: e.skills_shared_store,
            dir: e.skills_dir,
        })
        .unwrap_or_default()
}

/// Display icon for a registered custom agent — a `data:` URL in the normal
/// case. See [`CustomAgentDef::icon_url`].
pub fn icon_for(registry_id: &str) -> Option<&'static str> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(registry_id)
        .and_then(|e| e.icon)
}

/// Where a registered definition came from. `None` when the id is not
/// registered.
pub fn source_of(registry_id: &str) -> Option<CustomAgentSource> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(registry_id)
        .map(|e| e.source)
}

/// The user-declared version-probe command for a registered definition, if
/// any. See [`CustomAgentDef::version_probe`].
pub fn version_probe_of(registry_id: &str) -> Option<&'static str> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(registry_id)
        .and_then(|e| e.version_probe)
}

/// True when `registry_id` currently resolves to launch metadata.
pub fn is_registered(registry_id: &str) -> bool {
    get(registry_id).is_some()
}

/// Placeholder metadata for a custom agent id that is NOT registered — either
/// the user deleted the definition while conversations still reference it, or
/// the registry has not been hydrated yet.
///
/// [`crate::acp::registry::get_agent_meta`] is synchronous and infallible, so
/// it cannot report the miss; this keeps read-only consumers (settings list,
/// display names, cache paths) working while guaranteeing the agent cannot be
/// launched — the empty package/command is rejected up front by
/// `connection::build_agent`, which checks [`is_registered`] and fails with an
/// actionable error instead of spawning nothing.
pub fn unregistered_meta(registry_id: &'static str) -> AcpAgentMeta {
    AcpAgentMeta {
        agent_type: AgentType::Custom(registry_id),
        supports_mcp: true,
        name: registry_id,
        description: "Unregistered custom ACP agent",
        distribution: AgentDistribution::Npx {
            version: FALLBACK_VERSION,
            package: "",
            cmd: "",
            args: &[],
            env: &[],
            node_required: None,
        },
    }
}

/// Every registered custom agent, sorted by id for a stable UI order.
pub fn all() -> Vec<AgentType> {
    let mut ids: Vec<&'static str> = registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .copied()
        .collect();
    ids.sort_unstable();
    ids.into_iter().map(AgentType::Custom).collect()
}

/// Serializes tests that call [`hydrate`].
///
/// `hydrate` replaces a process-global map and cargo runs tests in threads, so
/// two tests publishing different registries — or one calling `hydrate(&[])` to
/// clean up — would otherwise clobber each other. Lives outside the test module
/// because tests in other modules (e.g. chat-command agent parsing) need the
/// same lock. Tests that only READ a uniquely-named id need no guard.
#[cfg(test)]
pub fn hydrate_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
