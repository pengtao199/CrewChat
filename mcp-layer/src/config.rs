use anyhow::{bail, Context, Result};
use rand::RngCore;
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub servers_dir: PathBuf,
    pub registry_path: PathBuf,
    pub runtime_dir: PathBuf,
    pub runtime_path: PathBuf,
    pub gateway_bin: PathBuf,
    pub gateway_db: PathBuf,
    pub gateway_admin_token_path: PathBuf,
    pub gateway_log: PathBuf,
    pub gateway_port: u16,
    pub npx: Option<PathBuf>,
    pub host: String,
    pub port: u16,
    pub token: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let root = env_path("MCP_LAYER_ROOT").unwrap_or_else(|| home_dir().join(".agents/mcp"));
        let host = env_string("MCP_LAYER_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
        let port = env_string("MCP_LAYER_PORT")
            .unwrap_or_else(|| "3461".to_string())
            .parse::<u16>()
            .context("MCP_LAYER_PORT must be a port number")?;
        let gateway_port = env_string("MCP_LAYER_GATEWAY_PORT")
            .unwrap_or_else(|| "3462".to_string())
            .parse::<u16>()
            .context("MCP_LAYER_GATEWAY_PORT must be a port number")?;
        let token = env_string("MCP_LAYER_TOKEN")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(generate_token);
        let gateway_bin = env_path("MCP_LAYER_GATEWAY_BIN").unwrap_or_else(default_gateway_bin);
        let npx = env_path("MCP_LAYER_NPX").or_else(|| find_in_path("npx"));

        Self::at(root, host, port, token, gateway_port, gateway_bin, npx)
    }

    /// Deterministic constructor for tests and embedding.
    pub fn at(
        root: PathBuf,
        host: String,
        port: u16,
        token: String,
        gateway_port: u16,
        gateway_bin: PathBuf,
        npx: Option<PathBuf>,
    ) -> Result<Self> {
        if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
            bail!("mcp-layer only listens on loopback; got {host}");
        }
        if port == gateway_port {
            bail!("mcp-layer control port and gateway port must differ");
        }

        let servers_dir = root.join("servers");
        let runtime_dir = root.join("runtime");
        fs::create_dir_all(&servers_dir).context("create MCP servers directory")?;
        fs::create_dir_all(&runtime_dir).context("create MCP runtime directory")?;

        Ok(Self {
            registry_path: root.join("registry.sqlite"),
            runtime_path: root.join("runtime.json"),
            gateway_db: runtime_dir.join("gateway.db"),
            gateway_admin_token_path: runtime_dir.join("admin-token"),
            gateway_log: runtime_dir.join("gateway.log"),
            root,
            servers_dir,
            runtime_dir,
            gateway_bin,
            gateway_port,
            npx,
            host,
            port,
            token,
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn gateway_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.gateway_port)
    }

    pub fn gateway_mcp_url(&self) -> String {
        format!("{}/mcp", self.gateway_url())
    }

    pub fn gateway_group_url(&self, group: &str) -> String {
        format!("{}/v0/groups/{group}/mcp", self.gateway_url())
    }

    /// Write the 0600 runtime file a local client reads to find the daemon.
    pub fn write_runtime(&self) -> Result<()> {
        let body = serde_json::json!({
            "pid": std::process::id(),
            "host": self.host,
            "port": self.port,
            "url": self.url(),
            "token": self.token,
            "gatewayUrl": self.gateway_url(),
            "gatewayMcpUrl": self.gateway_mcp_url(),
        });
        let bytes = serde_json::to_vec_pretty(&body)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&self.runtime_path)
            .context("open mcp-layer runtime file")?;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

fn default_gateway_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let beside = parent.join("crewchat-mcp-gateway");
            if beside.is_file() {
                return beside;
            }
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let local = manifest.join("bin").join("crewchat-mcp-gateway");
    if local.is_file() {
        return local;
    }
    let vendored = manifest
        .join("vendor")
        .join("gateway")
        .join("crewchat-mcp-gateway");
    if vendored.is_file() {
        return vendored;
    }
    PathBuf::from("crewchat-mcp-gateway")
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

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn ensure_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
