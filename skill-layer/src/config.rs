use anyhow::{bail, Context, Result};
use rand::RngCore;
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub store_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub registry_path: PathBuf,
    pub runtime_path: PathBuf,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub session_root: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let root = env_path("SKILL_LAYER_ROOT").unwrap_or_else(|| home_dir().join(".agents"));
        let host = env_string("SKILL_LAYER_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
        let port = env_string("SKILL_LAYER_PORT")
            .unwrap_or_else(|| "3460".to_string())
            .parse::<u16>()
            .context("SKILL_LAYER_PORT must be a port number")?;
        let token = env_string("SKILL_LAYER_TOKEN")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(generate_token);
        let session_root = env_path("SKILL_LAYER_SESSION_ROOT");
        Self::at(root, host, port, token, session_root)
    }

    /// Deterministic constructor for tests and embedding.
    pub fn at(
        root: PathBuf,
        host: String,
        port: u16,
        token: String,
        session_root: Option<PathBuf>,
    ) -> Result<Self> {
        if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
            bail!("skill-layer only listens on loopback; got {host}");
        }
        let store_dir = root.join("skill-store");
        let skills_dir = root.join("skills");
        let meta_dir = root.join("skill-layer");
        fs::create_dir_all(&store_dir).context("create skill-store directory")?;
        fs::create_dir_all(&skills_dir).context("create skills directory")?;
        fs::create_dir_all(&meta_dir).context("create skill-layer metadata directory")?;

        Ok(Self {
            registry_path: meta_dir.join("registry.sqlite"),
            runtime_path: meta_dir.join("runtime.json"),
            root,
            store_dir,
            skills_dir,
            host,
            port,
            token,
            session_root,
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// Write the 0600 runtime file a local client reads to find the daemon.
    pub fn write_runtime(&self) -> Result<()> {
        let body = serde_json::json!({
            "pid": std::process::id(),
            "host": self.host,
            "port": self.port,
            "url": self.url(),
            "token": self.token,
        });
        let bytes = serde_json::to_vec_pretty(&body)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&self.runtime_path)
            .context("open skill-layer runtime file")?;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
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

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
