//! Environment and on-disk locations for the model layer.
//!
//! The service is deliberately configured by env only: AgentOS (or a test
//! harness) is the only launcher, and the same env can point it at a scratch
//! data directory while another model layer keeps running.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// One service instance.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub host: String,
    pub gateway_port: u16,
    pub control_port: u16,
    pub control_token: String,
    /// Rewrite `~/.claude/settings.json` on every model-page save. Off by
    /// default so a model edit does not move the user's global CLI.
    pub apply_profile_on_save: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;

        let data_dir = env_path("MODEL_LAYER_DATA_DIR")
            .unwrap_or_else(|| home.join("Library/Application Support/AgentOS/model"));
        let host = env_string("MODEL_LAYER_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
        let gateway_port = env_u16("MODEL_LAYER_GATEWAY_PORT").unwrap_or(3456);
        let control_port = env_u16("MODEL_LAYER_CONTROL_PORT").unwrap_or(3458);

        let control_token = env_string("MODEL_LAYER_CONTROL_TOKEN").unwrap_or_else(random_token);
        let apply_profile_on_save = env_bool("MODEL_LAYER_APPLY_PROFILE").unwrap_or(false);
        Ok(Self {
            data_dir,
            host,
            gateway_port,
            control_port,
            control_token,
            apply_profile_on_save,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("model.db")
    }

    pub fn runtime_path(&self) -> PathBuf {
        self.data_dir.join("model-layer.json")
    }

    pub fn gateway_url(&self) -> String {
        format!("http://{}:{}", self.host, self.gateway_port)
    }

    pub fn control_url(&self) -> String {
        format!("http://{}:{}", self.host, self.control_port)
    }

    /// Persist the endpoint + token so AgentOS and cli-layer do not need to
    /// guess our defaults.
    pub fn write_runtime_file(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("create {}", self.data_dir.display()))?;
        let body = serde_json::json!({
            "gatewayUrl": self.gateway_url(),
            "controlUrl": self.control_url(),
            "controlToken": self.control_token,
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

fn env_bool(key: &str) -> Option<bool> {
    let value = env_string(key)?;
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_path_is_under_data_dir() {
        let cfg = Config {
            data_dir: PathBuf::from("/tmp/x"),
            host: "127.0.0.1".into(),
            gateway_port: 3456,
            control_port: 3458,
            control_token: "t".into(),
            apply_profile_on_save: false,
        };
        assert_eq!(cfg.db_path(), PathBuf::from("/tmp/x/model.db"));
        assert_eq!(cfg.gateway_url(), "http://127.0.0.1:3456");
    }
}
