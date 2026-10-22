//! Environment and on-disk locations for the model layer.
//!
//! The service is deliberately configured by env only: AgentOS (or a test
//! harness) is the only launcher, and the same env can point it at a scratch
//! data directory while the real CCR keeps running.

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
    /// `~/.claude-code-router` by default; the import source.
    pub ccr_home: PathBuf,
    pub ccr_web_url: Option<String>,
    pub ccr_web_token: Option<String>,
    /// `ccr-disabled-models.json` owned by codeg, if it exists.
    pub disabled_models_path: Option<PathBuf>,
    pub import_on_start: bool,
    /// Rewrite `~/.claude/settings.json` on every model-page save. Off by
    /// default so side-by-side testing does not move the user's global CLI.
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

        let ccr_home = env_path("CCR_HOME").unwrap_or_else(|| home.join(".claude-code-router"));
        let (ccr_web_url, ccr_web_token) = discover_ccr(&ccr_home);

        let control_token = env_string("MODEL_LAYER_CONTROL_TOKEN")
            .or_else(|| env_string("CCR_WEB_AUTH_TOKEN"))
            .unwrap_or_else(random_token);

        let disabled_models_path = env_path("MODEL_LAYER_DISABLED_MODELS")
            .or_else(|| env_path("CODEG_DATA_DIR").map(|dir| dir.join("ccr-disabled-models.json")))
            .or_else(|| {
                let legacy =
                    home.join("Library/Application Support/codeg/ccr-disabled-models.json");
                legacy.is_file().then_some(legacy)
            });

        let import_on_start = env_bool("MODEL_LAYER_IMPORT_ON_START").unwrap_or(true);
        let apply_profile_on_save = env_bool("MODEL_LAYER_APPLY_PROFILE").unwrap_or(false);
        Ok(Self {
            data_dir,
            host,
            gateway_port,
            control_port,
            control_token,
            ccr_home,
            ccr_web_url,
            ccr_web_token,
            disabled_models_path,
            import_on_start,
            apply_profile_on_save,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("model.db")
    }

    /// The last Claude-visible catalog. `[1m]` is not in the provider JSON, so
    /// a stopped CCR would otherwise make the next import forget which models
    /// carry the suffix.
    pub fn claude_models_path(&self) -> PathBuf {
        self.data_dir.join("claude-models.json")
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

    /// Persist the endpoint + token so AgentOS can pass `CCR_WEB_URL` /
    /// `CCR_WEB_AUTH_TOKEN` to codeg without knowing our defaults.
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

/// CCR writes `service.json` with the management URL and its web token in the
/// query string. We read it only to import while CCR is still alive; we never
/// overwrite it.
fn discover_ccr(ccr_home: &std::path::Path) -> (Option<String>, Option<String>) {
    let env_url = env_string("CCR_WEB_URL");
    let env_token = env_string("CCR_WEB_TOKEN");
    if env_url.is_some() {
        return (env_url, env_token);
    }
    let raw = std::fs::read_to_string(ccr_home.join("service.json")).ok();
    let Some(raw) = raw else {
        return (None, env_token);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (None, env_token);
    };
    let Some(url) = value.get("url").and_then(|v| v.as_str()) else {
        return (None, env_token);
    };
    let mut base = url.to_string();
    let mut token = env_token;
    if let Ok(mut parsed) = reqwest::Url::parse(url) {
        for (key, value) in parsed.query_pairs() {
            if key == "ccr_web_token" && token.is_none() {
                token = Some(value.to_string());
            }
        }
        parsed.set_query(None);
        base = parsed.as_str().trim_end_matches('/').to_string();
    }
    (Some(base), token)
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
            ccr_home: PathBuf::from("/tmp/ccr"),
            ccr_web_url: None,
            ccr_web_token: None,
            disabled_models_path: None,
            import_on_start: false,
            apply_profile_on_save: false,
        };
        assert_eq!(cfg.db_path(), PathBuf::from("/tmp/x/model.db"));
        assert_eq!(cfg.gateway_url(), "http://127.0.0.1:3456");
    }
}
