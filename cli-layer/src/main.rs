//! CrewChat CLI layer.
//!
//! A small Rust sidecar that replaces codeg's ACP/workbench core:
//! - `:3081` HTTP compatibility surface for AgentOS
//! - `/ws/events` streaming
//! - ACP host (Claude/Codex/OpenCode)
//! - SQLite projects/conversations/turns/roster
//! - thin `ccr_*` proxy to the model layer

mod acp;
mod config;
mod engines;
mod http;
mod model_proxy;
mod roles;
mod store;
mod ws;

use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::acp::host::AcpHost;
use crate::config::Config;
use crate::http::AppState;
use crate::model_proxy::ModelLayerClient;
use crate::store::Store;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crewchat_cli=info".into()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let store = Store::open(&config.db_path())?;
    store.init_schema()?;
    config.write_runtime_file()?;

    let host = AcpHost::new(config.clone(), store.clone());
    let model = Arc::new(ModelLayerClient::new(&config));
    let state = AppState {
        config: config.clone(),
        store,
        host,
        model,
    };

    let router = http::router(state);
    let listener = TcpListener::bind((config.host.as_str(), config.port)).await?;
    info!("cli-layer listening on {}", config.base_url());
    info!("model layer proxy -> {}", config.model_layer_url);

    tokio::select! {
        result = axum::serve(listener, router) => {
            if let Err(error) = result {
                warn!("cli-layer stopped: {error}");
            }
        }
        _ = tokio::signal::ctrl_c() => info!("shutting down"),
    }
    Ok(())
}
