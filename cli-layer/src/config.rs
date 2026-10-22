//! Environment and on-disk locations for the CLI layer.

use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub model_layer_url: String,
    pub model_layer_gateway: String,
    pub model_layer_token: Option<String>,
    /// The ACP adapter commands, overridable for local testing.
    pub claude_command: String,
    pub codex_command: String,
    pub opencode_command: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let data_dir = env_path("CLI_LAYER_DATA_DIR")
            .unwrap_or_else(|| home.join("Library/Application Support/AgentOS/cli"));
        let host = env_string("CLI_LAYER_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
        let port = env_u16("CLI_LAYER_PORT").unwrap_or(3081);
        let token = env_string("CLI_LAYER_TOKEN").unwrap_or_else(random_token);

        let model_layer_url =
            env_string("MODEL_LAYER_URL").unwrap_or_else(|| "http://127.0.0.1:3458".to_string());
        let model_layer_gateway = env_string("MODEL_LAYER_GATEWAY_URL")
            .unwrap_or_else(|| "http://127.0.0.1:3456".to_string());
        let model_layer_token = env_string("MODEL_LAYER_TOKEN").or_else(|| {
            std::fs::read_to_string(home.join("Library/Application Support/AgentOS/model/token"))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        });

        let claude_command = env_string("CLI_LAYER_CLAUDE_COMMAND")
            .unwrap_or_else(|| "npx -y @agentclientprotocol/claude-agent-acp@latest".to_string());
        let codex_command = env_string("CLI_LAYER_CODEX_COMMAND")
            .unwrap_or_else(|| "npx -y @agentclientprotocol/codex-acp@latest".to_string());
        let opencode_command =
            env_string("CLI_LAYER_OPENCODE_COMMAND").unwrap_or_else(|| "opencode acp".to_string());

        Ok(Self {
            data_dir,
            host,
            port,
            token,
            model_layer_url,
            model_layer_gateway,
            model_layer_token,
            claude_command,
            codex_command,
            opencode_command,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("cli-layer.db")
    }

    pub fn runtime_path(&self) -> PathBuf {
        self.data_dir.join("cli-layer.json")
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn write_runtime_file(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("create {}", self.data_dir.display()))?;
        let body = serde_json::json!({
            "baseUrl": self.base_url(),
            "token": self.token,
            "pid": std::process::id(),
            "updatedAt": now(),
        });
        let path = self.runtime_path();
        std::fs::write(&path, serde_json::to_vec_pretty(&body)?)
            .with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_path(key: &str) -> Option<PathBuf> {
    env_string(key).map(PathBuf::from)
}

fn env_u16(key: &str) -> Option<u16> {
    env_string(key)?.parse().ok()
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
