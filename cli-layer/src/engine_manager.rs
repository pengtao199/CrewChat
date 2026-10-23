//! CLI engine lifecycle: versions, npx installs, binary downloads, diagnostics.
//!
//! This is the backend for the CLI page. It deliberately keeps the install
//! cache inside the app data dir so a failed install never touches the user's
//! global npm state (except `acp_install_pi_binary`, which is explicitly a
//! global npm install, matching codeg).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::config::Config;
use crate::engines::{install_spec, pi_install_spec, EngineKind, InstallKind, InstallSpec};

const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const PI_CODING_AGENT_PACKAGE: &str = "@earendil-works/pi-coding-agent";

pub fn spec_for(agent_type: &str) -> Option<InstallSpec> {
    match agent_type {
        "claude_code" | "claude-code" | "claude" => Some(install_spec(EngineKind::ClaudeCode)),
        "codex" => Some(install_spec(EngineKind::Codex)),
        "open_code" | "opencode" | "open-code" => Some(install_spec(EngineKind::OpenCode)),
        "pi" => Some(pi_install_spec()),
        _ => None,
    }
}

pub fn install_dir(config: &Config, spec: InstallSpec) -> PathBuf {
    config.data_dir.join("engines").join(spec.agent_type)
}

pub fn local_binary(config: &Config, spec: InstallSpec) -> Option<PathBuf> {
    let dir = install_dir(config, spec);
    let path = match spec.kind {
        InstallKind::Npx => dir.join("node_modules/.bin").join(spec.cmd),
        InstallKind::Binary => dir.join(spec.cmd),
    };
    path.is_file().then_some(path)
}

pub fn installed_version(config: &Config, spec: InstallSpec) -> Option<String> {
    match spec.kind {
        InstallKind::Npx => {
            let path = install_dir(config, spec)
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
        InstallKind::Binary => {
            let binary = local_binary(config, spec).unwrap_or_else(|| PathBuf::from(spec.cmd));
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

pub async fn latest_version(_config: &Config, agent_type: &str) -> Result<Option<String>> {
    let spec = spec_for(agent_type).ok_or_else(|| anyhow!("unsupported engine '{agent_type}'"))?;
    match spec.kind {
        InstallKind::Npx => npm_latest(spec.package).await,
        InstallKind::Binary => github_latest("anomalyco/opencode").await,
    }
}

pub async fn prepare_npx(
    config: &Config,
    agent_type: &str,
    version: Option<&str>,
    clean_first: bool,
) -> Result<String> {
    let spec = spec_for(agent_type).ok_or_else(|| anyhow!("unsupported engine '{agent_type}'"))?;
    if spec.kind != InstallKind::Npx {
        bail!(
            "{} is a binary engine; use acp_download_agent_binary",
            spec.agent_type
        );
    }
    let dir = install_dir(config, spec);
    if clean_first {
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let version = version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(spec.default_version);
    let package = format!("{}@{}", spec.package, version);
    let output = run(
        "npm",
        &[
            "install",
            "--prefix",
            &dir.to_string_lossy(),
            "--no-audit",
            "--no-fund",
            "--loglevel=error",
            &package,
        ],
        INSTALL_TIMEOUT,
    )
    .await?;
    if !output.0 {
        bail!("npm install failed: {}", tail(&output.2));
    }
    Ok(installed_version(config, spec)
        .unwrap_or_else(|| version.trim_start_matches('v').to_string()))
}

pub async fn download_binary(
    config: &Config,
    agent_type: &str,
    version: Option<&str>,
) -> Result<String> {
    let spec = spec_for(agent_type).ok_or_else(|| anyhow!("unsupported engine '{agent_type}'"))?;
    if spec.kind != InstallKind::Binary {
        bail!(
            "{} is an npx engine; use acp_prepare_npx_agent",
            spec.agent_type
        );
    }
    let version = match version.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.trim_start_matches('v').to_string(),
        None => latest_version(config, agent_type)
            .await?
            .ok_or_else(|| anyhow!("no latest release for {}", spec.agent_type))?,
    };
    let (archive, kind) = platform_archive()?;
    let url =
        format!("https://github.com/anomalyco/opencode/releases/download/v{version}/{archive}");
    let client = reqwest::Client::builder()
        .timeout(INSTALL_TIMEOUT)
        .build()?;
    let bytes = client
        .get(&url)
        .header("user-agent", "crewchat-cli")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let dir = install_dir(config, spec);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let archive_path = dir.join(archive);
    std::fs::write(&archive_path, &bytes)?;

    let status = match kind {
        "zip" => {
            Command::new("unzip")
                .args([
                    "-o",
                    &archive_path.to_string_lossy(),
                    "-d",
                    &dir.to_string_lossy(),
                ])
                .status()
                .await?
        }
        _ => {
            Command::new("tar")
                .args([
                    "-xzf",
                    &archive_path.to_string_lossy(),
                    "-C",
                    &dir.to_string_lossy(),
                ])
                .status()
                .await?
        }
    };
    if !status.success() {
        bail!("failed to extract {archive}");
    }
    let _ = std::fs::remove_file(&archive_path);
    let binary = find_binary(&dir, spec.cmd)
        .ok_or_else(|| anyhow!("archive did not contain {}", spec.cmd))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(version)
}

pub async fn install_pi_binary() -> Result<String> {
    let output = run(
        "npm",
        &[
            "install",
            "-g",
            &format!("{PI_CODING_AGENT_PACKAGE}@latest"),
        ],
        INSTALL_TIMEOUT,
    )
    .await?;
    if !output.0 {
        bail!("npm install -g pi failed: {}", tail(&output.2));
    }
    Ok(String::new())
}

pub async fn diagnostics(config: &Config, agent_type: &str) -> Value {
    let spec = spec_for(agent_type);
    let node = version_of("node").await;
    let npm = version_of("npm").await;
    let mut checks = Vec::new();
    checks.push(check(
        "Node",
        node.clone().unwrap_or_else(|| "not found".into()),
        node.is_some(),
        "Install Node.js 20+ so npx adapters can run.",
    ));
    checks.push(check(
        "npm",
        npm.clone().unwrap_or_else(|| "not found".into()),
        npm.is_some(),
        "Install npm (bundled with Node.js).",
    ));

    let mut engine_ok = false;
    if let Some(spec) = spec {
        let local = local_binary(config, spec);
        let installed = installed_version(config, spec);
        let (value, available) = if let Some(version) = installed {
            (version, true)
        } else if local.is_some() {
            ("installed (version unknown)".to_string(), true)
        } else if command_available(spec.cmd) {
            ("available in PATH".to_string(), true)
        } else {
            ("not installed".to_string(), false)
        };
        engine_ok = available;
        checks.push(check(
            "Adapter",
            value,
            available,
            "Run acp_prepare_npx_agent or acp_download_agent_binary.",
        ));
    }

    let writable = std::fs::create_dir_all(&config.data_dir).is_ok()
        && std::fs::write(config.data_dir.join(".doctor"), b"ok").is_ok();
    let _ = std::fs::remove_file(config.data_dir.join(".doctor"));
    checks.push(check(
        "Data directory",
        config.data_dir.to_string_lossy().to_string(),
        writable,
        "Check permissions on the app data directory.",
    ));

    let level = if node.is_none() || npm.is_none() {
        "error"
    } else if !engine_ok {
        "warn"
    } else {
        "ok"
    };
    let summary = match level {
        "ok" => "CLI runtime looks ready.",
        "warn" => "The adapter is not installed yet.",
        _ => "Node.js or npm is missing.",
    };
    let plain_text = checks
        .iter()
        .map(|check| {
            format!(
                "{}: {}",
                check["label"].as_str().unwrap_or(""),
                check["value"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    json!({
        "verdict": {"level": level, "code": level, "summary": summary},
        "sections": [{"title": "CLI runtime", "checks": checks}],
        "plain_text": plain_text,
    })
}

fn check(label: &str, value: String, ok: bool, hint: &str) -> Value {
    json!({
        "label": label,
        "value": value,
        "status": if ok { "ok" } else { "warn" },
        "hint": hint,
    })
}

async fn version_of(program: &str) -> Option<String> {
    let output = run(program, &["--version"], Duration::from_secs(5))
        .await
        .ok()?;
    if !output.0 {
        return None;
    }
    parse_version(&output.1)
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

async fn npm_latest(package: &str) -> Result<Option<String>> {
    let output = run(
        "npm",
        &["view", package, "version", "--json"],
        PROBE_TIMEOUT,
    )
    .await?;
    if !output.0 {
        return Ok(None);
    }
    Ok(parse_version(&output.1))
}

async fn github_latest(repo: &str) -> Result<Option<String>> {
    let client = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build()?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("user-agent", "crewchat-cli")
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let value: Value = response.json().await?;
    Ok(value
        .get("tag_name")
        .and_then(Value::as_str)
        .map(|tag| tag.trim_start_matches('v').to_string()))
}

fn platform_archive() -> Result<(&'static str, &'static str)> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(("opencode-darwin-arm64.zip", "zip"))
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok(("opencode-darwin-x64.zip", "zip"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok(("opencode-linux-arm64.tar.gz", "tar"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(("opencode-linux-x64.tar.gz", "tar"))
    } else {
        bail!("unsupported platform for the OpenCode binary")
    }
}

fn find_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_binary(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

fn command_available(command: &str) -> bool {
    let Some(program) = command.split_whitespace().next() else {
        return false;
    };
    if program.contains('/') {
        return Path::new(program.trim_matches(['\'', '"'])).is_file();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

async fn run(program: &str, args: &[&str], timeout: Duration) -> Result<(bool, String, String)> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .with_context(|| format!("{program} timed out"))??;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().rev().take(8).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_npm_and_binary_versions() {
        assert_eq!(parse_version("v1.2.3\n"), Some("1.2.3".to_string()));
        assert_eq!(parse_version("\"2.0.0\"\n"), Some("2.0.0".to_string()));
        assert_eq!(parse_version(""), None);
    }
}
