// custom_registry submodule: definition validation (split from acp/custom_registry.rs —
// mechanical move, see NOTICE.md).
use super::*;

use crate::models::agent::{is_valid_custom_agent_id};


/// Whether `s` is usable as ONE directory name.
///
/// The version label becomes a path segment of the binary cache
/// (`binary_cache::binary_dir`), which codeg also *deletes* when reinstalling —
/// so a definition must not be able to steer it out of the cache. Mirrors
/// `acp_transcript::safe_component`, and deliberately rejects a Windows drive
/// or alternate-data-stream prefix (`C:`, `f:s`) too, which `Path::is_absolute`
/// would not catch on a unix build.
fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        // Also rules out "." and "..", which the byte check alone allows.
        && !s.starts_with('.')
}

/// Whether an in-archive launch path stays inside the extracted tree.
///
/// `binary_cache` joins this onto the install directory and then makes the
/// result executable, so an absolute or `..`-bearing entry would chmod and
/// launch a file of the definition's choosing anywhere on disk. Checked against
/// the NORMALIZED form (see [`split_archive_entry`]), which has already folded
/// `\` into `/`, so a Windows-style `..\..\x` is caught on a unix build too.
fn is_safe_archive_entry(raw: &str) -> bool {
    let (path, _) = split_archive_entry(raw);
    if path.is_empty() {
        // No entry at all — the command name falls back to the archive stem.
        return true;
    }
    // A leading `/` is the unix absolute form and also what a UNC `\\host\share`
    // normalizes to; `:` is a drive or stream prefix.
    if path.starts_with('/') || path.contains(':') {
        return false;
    }
    path.split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// The version label [`build_meta`] would stamp on a definition: its own, or
/// [`FALLBACK_VERSION`] when it carries none.
pub(super) fn effective_version(def: &CustomAgentDef) -> &str {
    let trimmed = def.version.trim();
    if trimmed.is_empty() {
        FALLBACK_VERSION
    } else {
        trimmed
    }
}

/// Check a definition **without building (and therefore leaking) anything**.
///
/// [`build_meta`] promotes strings to `&'static` by leaking them, so it must
/// only run where the result is kept — hydration, which is fingerprint-gated.
/// Callers that merely want to know whether a definition is usable (the
/// settings list renders a "why not" for each row, on every refresh) call this
/// instead. `build_meta` runs the same checks by calling it first, so the two
/// can never disagree about what is valid.
pub fn validate(def: &CustomAgentDef) -> Result<(), CustomAgentError> {
    // Checked BEFORE the slug validation: the registry ids ("claude-acp",
    // "deepseek-acp", …) are a separate namespace from the built-in WIRE names
    // `is_valid_custom_agent_id` blocks, but a few ids ("cursor") live in
    // both — and the collision diagnosis ("the built-in covers this, delete
    // the entry") is the actionable one wherever the two overlap. A custom
    // definition under any built-in registry id could never be launched or
    // listed distinctly (`from_registry_id` resolves built-ins first).
    if crate::acp::registry::builtin_acp_agents()
        .iter()
        .any(|agent| crate::acp::registry::registry_id_for(*agent) == def.registry_id)
    {
        return Err(CustomAgentError::CollidesWithBuiltin(
            def.registry_id.clone(),
        ));
    }
    if !is_valid_custom_agent_id(&def.registry_id) {
        return Err(CustomAgentError::InvalidId(def.registry_id.clone()));
    }
    if def.name.trim().is_empty() {
        return Err(CustomAgentError::EmptyName);
    }
    let version = effective_version(def);
    if !is_safe_path_component(version) {
        return Err(CustomAgentError::UnsafeVersion(version.to_string()));
    }
    match def.distribution_kind {
        CustomDistributionKind::Npx => {
            let npx = def
                .spec
                .npx
                .as_ref()
                .ok_or(CustomAgentError::MissingChannel("npx"))?;
            if npx.package.trim().is_empty() {
                return Err(CustomAgentError::MissingPackage("npx"));
            }
        }
        CustomDistributionKind::Uvx => {
            let uvx = def
                .spec
                .uvx
                .as_ref()
                .ok_or(CustomAgentError::MissingChannel("uvx"))?;
            if uvx.package.trim().is_empty() {
                return Err(CustomAgentError::MissingPackage("uvx"));
            }
        }
        CustomDistributionKind::Binary => {
            if def.spec.binary.is_empty() {
                return Err(CustomAgentError::MissingChannel("binary"));
            }
            let current = crate::acp::registry::current_platform();
            let here = def
                .spec
                .binary
                .get(current)
                .ok_or_else(|| CustomAgentError::UnsupportedPlatform(current.to_string()))?;
            if here.archive.trim().is_empty() {
                return Err(CustomAgentError::MissingArchiveUrl(current.to_string()));
            }
            // EVERY platform's entry, not just this machine's: a nested layout
            // makes `build_binary_distribution` borrow another family's path
            // for the `dir_entry` it publishes.
            for (platform, binary) in &def.spec.binary {
                if !is_safe_archive_entry(&binary.cmd) {
                    return Err(CustomAgentError::UnsafeArchiveEntry(platform.clone()));
                }
            }
        }
    }
    Ok(())
}
