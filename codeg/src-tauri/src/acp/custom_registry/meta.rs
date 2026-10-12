// custom_registry submodule: launch-metadata building (split from acp/custom_registry.rs —
// mechanical move, see NOTICE.md).
use super::*;

use std::collections::{BTreeMap};
use crate::acp::registry::{AcpAgentMeta, AgentDistribution, BinaryDirEntry, PlatformBinary};
use crate::intern::{intern, leak_slice};
use crate::models::agent::{AgentType};


/// Validate a definition and convert it into leaked launch metadata.
///
/// Every string that ends up in [`AcpAgentMeta`] is interned, so calling this
/// twice with equal input leaks only the slices (which [`hydrate`] avoids via
/// fingerprinting). **Call [`validate`] instead when the metadata is not kept**
/// — those slice leaks are not deduplicated, so building a throwaway meta on a
/// repeatable request would grow the process without bound.
pub fn build_meta(def: &CustomAgentDef) -> Result<AcpAgentMeta, CustomAgentError> {
    validate(def)?;
    let agent_type = AgentType::Custom(intern(&def.registry_id));
    let version = intern(effective_version(def));

    let distribution = match def.distribution_kind {
        CustomDistributionKind::Npx => {
            let npx = def
                .spec
                .npx
                .as_ref()
                .ok_or(CustomAgentError::MissingChannel("npx"))?;
            if npx.package.trim().is_empty() {
                return Err(CustomAgentError::MissingPackage("npx"));
            }
            AgentDistribution::Npx {
                version,
                package: intern(npx.package.trim()),
                cmd: intern(&resolved_cmd(npx.cmd.as_deref(), &npx.package)),
                args: intern_args(&npx.args),
                env: intern_env(&npx.env),
                node_required: npx.node_required.as_deref().map(intern),
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
            let cmd = intern(&resolved_cmd(uvx.cmd.as_deref(), &uvx.package));
            let args = intern_args(&uvx.args);
            AgentDistribution::Uvx {
                version,
                package: intern(uvx.package.trim()),
                cmd,
                args,
                env: intern_env(&uvx.env),
                uv_required: uvx.uv_required.as_deref().map(intern),
                python: uvx.python.as_deref().map(intern),
                // The user's own install of the CLI (pipx, `uv tool install`,
                // …) is a legitimate launcher for a custom agent — same
                // recipe as the uvx entry script, used when the uv runner is
                // absent.
                system_cmd: Some((cmd, args)),
            }
        }
        CustomDistributionKind::Binary => build_binary_distribution(&def.spec, version)?,
    };

    Ok(AcpAgentMeta {
        agent_type,
        // MCP is forwarded over the ACP wire (`session/new`'s `mcpServers`) —
        // for a custom agent, the codeg-mcp companion is the whole of it. On
        // by default; the user turns it off for an agent that rejects server
        // entries and fails to connect (see `CustomAgentDef::supports_mcp`).
        // Built-ins declare the same flag as a constant, where OpenClaw is the
        // only opt-out (see `only_builtin_openclaw_opts_out_of_mcp`).
        supports_mcp: def.supports_mcp,
        name: intern(def.name.trim()),
        description: intern(def.description.trim()),
        distribution,
    })
}

fn resolved_cmd(explicit: Option<&str>, package: &str) -> String {
    explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| derive_command_name(package))
}

fn intern_args(args: &[String]) -> &'static [&'static str] {
    leak_slice(args.iter().map(|a| intern(a)).collect::<Vec<_>>())
}

fn intern_env(env: &BTreeMap<String, String>) -> &'static [(&'static str, &'static str)] {
    leak_slice(
        env.iter()
            .map(|(k, v)| (intern(k), intern(v)))
            .collect::<Vec<_>>(),
    )
}

/// Registry-style in-archive launch paths are `./name`, `name.exe`, or a
/// nested `dir/name`. Strip the `./` and report whether the remainder is
/// nested (which forces whole-tree extraction instead of copy-out).
pub(super) fn split_archive_entry(raw: &str) -> (String, bool) {
    let trimmed = raw.trim().trim_start_matches("./").replace('\\', "/");
    let nested = trimmed.contains('/');
    (trimmed, nested)
}

fn build_binary_distribution(
    spec: &CustomAgentSpec,
    version: &'static str,
) -> Result<AgentDistribution, CustomAgentError> {
    if spec.binary.is_empty() {
        return Err(CustomAgentError::MissingChannel("binary"));
    }
    let current = crate::acp::registry::current_platform();
    let here = spec
        .binary
        .get(current)
        .ok_or_else(|| CustomAgentError::UnsupportedPlatform(current.to_string()))?;
    if here.archive.trim().is_empty() {
        return Err(CustomAgentError::MissingArchiveUrl(current.to_string()));
    }

    let (entry_path, nested) = split_archive_entry(&here.cmd);
    // `cmd` is both the PATH probe name and the file copied out of a
    // single-binary archive, so it must be the bare file name.
    let file_name = entry_path.rsplit('/').next().unwrap_or(&entry_path);
    let cmd = intern(if file_name.is_empty() {
        // A spec with no `cmd` at all: fall back to the archive's file stem.
        archive_stem(&here.archive)
    } else {
        file_name
    });

    // A nested entry means the tree must stay intact. Take each family's own
    // path when it has one so a Windows `.cmd` shim is honoured, and reuse the
    // other when only one side declares a nested layout.
    let dir_entry = if nested {
        let unix = spec
            .binary
            .iter()
            .find(|(p, _)| !p.starts_with("windows"))
            .map(|(_, b)| split_archive_entry(&b.cmd).0)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| entry_path.clone());
        let windows = spec
            .binary
            .iter()
            .find(|(p, _)| p.starts_with("windows"))
            .map(|(_, b)| split_archive_entry(&b.cmd).0)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| entry_path.clone());
        Some(BinaryDirEntry {
            unix: intern(&unix),
            windows: intern(&windows),
            // The ACP registry has no field for this, and codeg cannot know
            // which files an arbitrary agent's entry execs — so a custom
            // agent's tree is validated by its entry alone.
            required_siblings: crate::acp::registry::PlatformFiles::NONE,
        })
    } else {
        None
    };

    let mut platforms: Vec<PlatformBinary> = spec
        .binary
        .iter()
        .filter(|(_, b)| !b.archive.trim().is_empty())
        .map(|(platform, b)| PlatformBinary {
            platform: intern(platform),
            url: intern(b.archive.trim()),
            sha256: b
                .sha256
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(intern),
        })
        .collect();
    platforms.sort_by(|a, b| a.platform.cmp(b.platform));

    Ok(AgentDistribution::Binary {
        version,
        cmd,
        args: intern_args(&here.args),
        env: intern_env(&here.env),
        platforms: leak_slice(platforms),
        dir_entry,
    })
}

/// Last-resort command name for a binary spec with no `cmd`: the archive file
/// name with its (possibly compound) extension removed.
fn archive_stem(url: &str) -> &str {
    let file = url
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .split(['?', '#'])
        .next()
        .unwrap_or(url);
    file.split('.').next().unwrap_or(file)
}
