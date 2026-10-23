use anyhow::Result;
use crewchat_mcp::config::Config;
use crewchat_mcp::gateway::Gateway;
use crewchat_mcp::http::{router, AppState};
use crewchat_mcp::registry::Registry;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "crewchat-mcp\n\n\
             Local MCP layer daemon: registry, per-Agent gateway access, and session config.\n\n\
             Environment:\n\
               MCP_LAYER_ROOT              default ~/.agents/mcp\n\
               MCP_LAYER_HOST              default 127.0.0.1 (loopback only)\n\
               MCP_LAYER_PORT              default 3461\n\
               MCP_LAYER_TOKEN             optional; generated and written to runtime.json\n\
               MCP_LAYER_GATEWAY_PORT      default 3462\n\
               MCP_LAYER_GATEWAY_BIN       default mcp-layer/bin/crewchat-mcp-gateway\n\
               MCP_LAYER_NPX               optional absolute npx path for stdio bridge"
        );
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let registry = Registry::open(&config.registry_path)?;
    let gateway = Gateway::new(config.clone());
    let state = AppState {
        config: Arc::new(config.clone()),
        registry: Arc::new(Mutex::new(registry)),
        gateway: gateway.clone(),
        token: Arc::new(config.token.clone()),
    };

    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port)).await?;
    config.write_runtime()?;

    println!("crewchat-mcp listening on {}", config.url());
    println!("runtime file: {}", config.runtime_path.display());
    println!("gateway: {}", config.gateway_url());

    let auto_start = gateway.clone();
    tokio::spawn(async move {
        if let Err(error) = auto_start.ensure_started().await {
            tracing::warn!("mcp gateway is not ready: {error}");
        }
    });

    let shutdown_gateway = gateway.clone();
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown_gateway.stop().await;
        })
        .await?;
    Ok(())
}
