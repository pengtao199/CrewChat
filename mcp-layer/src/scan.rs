//! Discover MCP servers already configured on this machine.
//!
//! This is discovery, not a legacy-data migration: every supported CLI config
//! is read once, entries are normalized into the canonical MCP spec, and new
//! ids are written into `~/.agents/mcp/servers`. Existing canonical entries are
//! never overwritten.

use crate::registry::{write_server_file, Registry, ServerSpec};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub discovered: usize,
    pub imported: Vec<String>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn scan_local(registry: &mut Registry, servers_dir: &Path) -> Result<ScanReport> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let mut report = ScanReport::default();
    let mut seen = BTreeSet::new();

    for source in json_sources(&home) {
        if !source.path.is_file() {
            continue;
        }
        match read_json_servers(&source.path, source.keys) {
            Ok(entries) => {
                for (name, value) in entries {
                    import_one(
                        registry,
                        servers_dir,
                        &name,
                        &value,
                        source.label,
                        &mut seen,
                        &mut report,
                    );
                }
            }
            Err(error) => report
                .warnings
                .push(format!("{}: {error}", source.path.display())),
        }
    }
    for source in toml_sources(&home) {
        if !source.path.is_file() {
            continue;
        }
        match read_toml_servers(&source.path) {
            Ok(entries) => {
                for (name, value) in entries {
                    import_one(
                        registry,
                        servers_dir,
                        &name,
                        &value,
                        source.label,
                        &mut seen,
                        &mut report,
                    );
                }
            }
            Err(error) => report
                .warnings
                .push(format!("{}: {error}", source.path.display())),
        }
    }

    Ok(report)
}

struct JsonSource {
    label: &'static str,
    path: PathBuf,
    keys: &'static [&'static str],
}

struct TomlSource {
    label: &'static str,
    path: PathBuf,
}

fn json_sources(home: &Path) -> Vec<JsonSource> {
    let opencode = std::env::var_os("OPENCODE_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .map(|dir| dir.join("opencode/opencode.json"))
        })
        .unwrap_or_else(|| home.join(".config/opencode/opencode.json"));
    vec![
        JsonSource {
            label: "claude_code",
            path: home.join(".claude.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "claude_code",
            path: home.join(".claude/settings.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "open_code",
            path: opencode,
            keys: &["mcp", "mcpServers"],
        },
        JsonSource {
            label: "gemini",
            path: home.join(".gemini/settings.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "cursor",
            path: home.join(".cursor/mcp.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "cline",
            path: home.join(".cline/data/settings/cline_mcp_settings.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "openclaw",
            path: home.join(".openclaw/openclaw.json"),
            keys: &["mcpServers", "mcp"],
        },
        JsonSource {
            label: "codebuddy",
            path: home.join(".codebuddy.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "codebuddy",
            path: home.join(".codebuddy/settings.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "kimi_code",
            path: home.join(".kimi/mcp.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "deepseek",
            path: home.join(".deepseek/mcp.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "qoder",
            path: home.join(".qoder/settings.json"),
            keys: &["mcpServers"],
        },
        JsonSource {
            label: "antigravity",
            path: home.join(".antigravity/mcp_config.json"),
            keys: &["mcpServers"],
        },
    ]
}

fn toml_sources(home: &Path) -> Vec<TomlSource> {
    vec![
        TomlSource {
            label: "codex",
            path: home.join(".codex/config.toml"),
        },
        TomlSource {
            label: "grok",
            path: home.join(".grok/config.toml"),
        },
    ]
}

fn read_json_servers(path: &Path, keys: &[&str]) -> Result<Vec<(String, Value)>> {
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON at {}", path.display()))?;
    let mut entries = Vec::new();
    for key in keys {
        let Some(object) = root.get(*key).and_then(Value::as_object) else {
            continue;
        };
        for (name, value) in object {
            if value.is_object() {
                entries.push((name.clone(), value.clone()));
            }
        }
    }
    Ok(entries)
}

fn read_toml_servers(path: &Path) -> Result<Vec<(String, Value)>> {
    let raw = fs::read_to_string(path)?;
    let root: toml::Value =
        toml::from_str(&raw).with_context(|| format!("invalid TOML at {}", path.display()))?;
    let Some(table) = root.get("mcp_servers").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::new();
    for (name, value) in table {
        if let Some(object) = value.as_table() {
            entries.push((name.clone(), json!({ "toml": object })));
        }
    }
    Ok(entries)
}

fn import_one(
    registry: &mut Registry,
    servers_dir: &Path,
    name: &str,
    value: &Value,
    source: &str,
    seen: &mut BTreeSet<String>,
    report: &mut ScanReport,
) {
    let Some(spec) = normalize(name, value) else {
        report
            .warnings
            .push(format!("{source}: ignored incomplete MCP '{name}'"));
        return;
    };
    report.discovered += 1;
    if !seen.insert(spec.id.clone()) {
        report.skipped.push(spec.id);
        return;
    }
    if registry.get_server(&spec.id).ok().flatten().is_some() {
        report.skipped.push(spec.id);
        return;
    }
    if let Err(error) = write_server_file(servers_dir, &spec) {
        report
            .warnings
            .push(format!("{source}: write {}: {error}", spec.id));
        return;
    }
    match registry.upsert_server(&spec) {
        Ok(_) => report.imported.push(spec.id),
        Err(error) => report
            .warnings
            .push(format!("{source}: register {}: {error}", spec.id)),
    }
}

fn slugify_server_id(name: &str) -> Option<String> {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

fn normalize(name: &str, value: &Value) -> Option<ServerSpec> {
    let object = value.as_object()?;
    let mut spec = ServerSpec {
        id: slugify_server_id(name)?,
        name: name.to_string(),
        transport: "stdio".to_string(),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        enabled: object
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        command: None,
        args: Vec::new(),
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
        bearer_token: None,
        session_mode: None,
    };

    let command = object.get("command");
    if let Some(command) = command.and_then(Value::as_str) {
        spec.command = Some(command.to_string());
    } else if let Some(array) = command.and_then(Value::as_array) {
        let mut values = array.iter().filter_map(Value::as_str).map(str::to_string);
        spec.command = values.next();
        spec.args.extend(values);
    }
    if let Some(args) = object.get("args").and_then(Value::as_array) {
        spec.args
            .extend(args.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(environment) = object
        .get("env")
        .or_else(|| object.get("environment"))
        .and_then(Value::as_object)
    {
        for (key, value) in environment {
            if let Some(value) = value.as_str() {
                spec.env.insert(key.clone(), value.to_string());
            }
        }
    }

    let url = object
        .get("url")
        .or_else(|| object.get("serverUrl"))
        .and_then(Value::as_str);
    if let Some(url) = url {
        spec.transport = if object
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|value| value.contains("sse"))
            || url.ends_with("/sse")
        {
            "sse".to_string()
        } else {
            "http".to_string()
        };
        spec.url = Some(url.to_string());
        if let Some(headers) = object.get("headers").and_then(Value::as_object) {
            for (key, value) in headers {
                if let Some(value) = value.as_str() {
                    spec.headers.insert(key.clone(), value.to_string());
                }
            }
        }
        if let Some(token) = object
            .get("bearerToken")
            .or_else(|| object.get("bearer_token"))
            .and_then(Value::as_str)
        {
            spec.bearer_token = Some(token.to_string());
        }
    }

    // Codex stores the MCP table under `[mcp_servers.<name>]`.
    if let Some(table) = object.get("toml").and_then(|value| value.as_object()) {
        if let Some(command) = table.get("command").and_then(Value::as_str) {
            spec.command = Some(command.to_string());
        }
        if let Some(args) = table.get("args").and_then(Value::as_array) {
            spec.args
                .extend(args.iter().filter_map(Value::as_str).map(str::to_string));
        }
        if let Some(env) = table.get("env").and_then(Value::as_object) {
            for (key, value) in env {
                if let Some(value) = value.as_str() {
                    spec.env.insert(key.clone(), value.to_string());
                }
            }
        }
        if let Some(url) = table.get("url").and_then(Value::as_str) {
            spec.transport = "http".to_string();
            spec.url = Some(url.to_string());
        }
        if let Some(headers) = table
            .get("http_headers")
            .or_else(|| table.get("headers"))
            .and_then(Value::as_object)
        {
            for (key, value) in headers {
                if let Some(value) = value.as_str() {
                    spec.headers.insert(key.clone(), value.to_string());
                }
            }
        }
    }

    if spec.command.is_none() && spec.url.is_none() {
        return None;
    }
    Some(spec)
}
