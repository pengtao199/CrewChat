//! Global terminal links: make the complete MCP library visible to every
//! installed CLI, without touching per-Agent proxies.

use crate::config::Config;
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalLink {
    pub app: &'static str,
    pub path: String,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Clone, Copy)]
enum Format {
    McpServers,
    OpenCode,
    Toml,
}

struct Target {
    app: &'static str,
    path: PathBuf,
    format: Format,
}

pub fn status(_config: &Config) -> Vec<GlobalLink> {
    targets()
        .into_iter()
        .map(|target| {
            let linked = match target.format {
                Format::McpServers => json_has_key(&target.path, "mcpServers", "crewchat-mcp"),
                Format::OpenCode => json_has_key(&target.path, "mcp", "crewchat-mcp"),
                Format::Toml => toml_has_key(&target.path, "mcp_servers", "crewchat-mcp"),
            };
            GlobalLink {
                app: target.app,
                path: target.path.to_string_lossy().to_string(),
                state: if linked { "linked" } else { "missing" }.to_string(),
                error: None,
            }
        })
        .collect()
}

fn json_has_key(path: &Path, section: &str, key: &str) -> bool {
    read_json(path)
        .ok()
        .and_then(|value| value.get(section).cloned())
        .and_then(|value| value.get(key).cloned())
        .is_some()
}

fn toml_has_key(path: &Path, section: &str, key: &str) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.parse::<toml::Value>().ok())
        .and_then(|value| value.get(section).cloned())
        .and_then(|value| value.get(key).cloned())
        .is_some()
}

pub fn sync(config: &Config, endpoint: &str, token: &str) -> Vec<GlobalLink> {
    let Some(npx) = config.npx.as_ref() else {
        return vec![GlobalLink {
            app: "all",
            path: String::new(),
            state: "skipped".to_string(),
            error: Some("npx is not available; cannot create the stdio bridge".to_string()),
        }];
    };
    let bridge = bridge_entry(npx, endpoint, token);
    let mut links = Vec::new();
    for target in targets() {
        let result = match target.format {
            Format::McpServers => write_json(&target.path, "mcpServers", bridge.clone()),
            Format::OpenCode => write_opencode(&target.path, &bridge),
            Format::Toml => write_toml(&target.path, &bridge),
        };
        match result {
            Ok(()) => links.push(GlobalLink {
                app: target.app,
                path: target.path.to_string_lossy().to_string(),
                state: "linked".to_string(),
                error: None,
            }),
            Err(error) => links.push(GlobalLink {
                app: target.app,
                path: target.path.to_string_lossy().to_string(),
                state: "skipped".to_string(),
                error: Some(error.to_string()),
            }),
        }
    }
    links
}

fn bridge_entry(npx: &Path, endpoint: &str, token: &str) -> Value {
    json!({
        "command": npx.to_string_lossy(),
        "args": [
            "-y", "mcp-remote", endpoint,
            "--allow-http",
            "--header", format!("Authorization: Bearer {token}")
        ],
        "env": {}
    })
}

fn targets() -> Vec<Target> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let opencode = std::env::var_os("OPENCODE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg.join("opencode/opencode.json"));
    let mut targets = vec![
        Target {
            app: "claude_code",
            path: home.join(".claude.json"),
            format: Format::McpServers,
        },
        Target {
            app: "open_code",
            path: opencode,
            format: Format::OpenCode,
        },
        Target {
            app: "gemini",
            path: home.join(".gemini/settings.json"),
            format: Format::McpServers,
        },
        Target {
            app: "cursor",
            path: home.join(".cursor/mcp.json"),
            format: Format::McpServers,
        },
        Target {
            app: "cline",
            path: home.join(".cline/data/settings/cline_mcp_settings.json"),
            format: Format::McpServers,
        },
        Target {
            app: "openclaw",
            path: home.join(".openclaw/openclaw.json"),
            format: Format::McpServers,
        },
        Target {
            app: "codebuddy",
            path: home.join(".codebuddy.json"),
            format: Format::McpServers,
        },
        Target {
            app: "codebuddy",
            path: home.join(".codebuddy/settings.json"),
            format: Format::McpServers,
        },
        Target {
            app: "kimi_code",
            path: home.join(".kimi/mcp.json"),
            format: Format::McpServers,
        },
        Target {
            app: "deepseek",
            path: home.join(".deepseek/mcp.json"),
            format: Format::McpServers,
        },
        Target {
            app: "qoder",
            path: home.join(".qoder/settings.json"),
            format: Format::McpServers,
        },
        Target {
            app: "antigravity",
            path: home.join(".antigravity/mcp_config.json"),
            format: Format::McpServers,
        },
        Target {
            app: "codex",
            path: home.join(".codex/config.toml"),
            format: Format::Toml,
        },
        Target {
            app: "grok",
            path: home.join(".grok/config.toml"),
            format: Format::Toml,
        },
    ];
    targets.retain(|target| target.path.parent().is_some_and(Path::exists));
    targets
}

fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).with_context(|| format!("invalid JSON at {}", path.display()))
}

fn write_json(path: &Path, key: &str, entry: Value) -> Result<()> {
    let mut root = read_json(path)?;
    let Some(object) = root.as_object_mut() else {
        anyhow::bail!("root of {} is not an object", path.display());
    };
    let section = object
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(section) = section.as_object_mut() else {
        anyhow::bail!("{} is not an object in {}", key, path.display());
    };
    section.insert("crewchat-mcp".to_string(), entry);
    write_atomic(path, &serde_json::to_vec_pretty(&root)?)
}

fn write_opencode(path: &Path, bridge: &Value) -> Result<()> {
    let mut root = read_json(path)?;
    let Some(object) = root.as_object_mut() else {
        anyhow::bail!("root of {} is not an object", path.display());
    };
    let section = object
        .entry("mcp")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(section) = section.as_object_mut() else {
        anyhow::bail!("mcp is not an object in {}", path.display());
    };
    let command = bridge
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut args = vec![json!(bridge
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("npx"))];
    args.extend(command);
    section.insert(
        "crewchat-mcp".to_string(),
        json!({"type":"local","command":args,"enabled":true}),
    );
    write_atomic(path, &serde_json::to_vec_pretty(&root)?)
}

fn write_toml(path: &Path, bridge: &Value) -> Result<()> {
    let raw = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let mut root: toml::Value = if raw.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        toml::from_str(&raw).with_context(|| format!("invalid TOML at {}", path.display()))?
    };
    let Some(root_table) = root.as_table_mut() else {
        anyhow::bail!("root of {} is not a table", path.display());
    };
    let section = root_table
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let Some(section) = section.as_table_mut() else {
        anyhow::bail!("mcp_servers is not a table in {}", path.display());
    };
    let mut entry = toml::map::Map::new();
    entry.insert(
        "command".to_string(),
        toml::Value::String(
            bridge
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("npx")
                .to_string(),
        ),
    );
    let args = bridge
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(|value| toml::Value::String(value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    entry.insert("args".to_string(), toml::Value::Array(args));
    section.insert("crewchat-mcp".to_string(), toml::Value::Table(entry));
    write_atomic(path, toml::to_string_pretty(&root)?.as_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("crewchat.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
