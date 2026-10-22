//! CrewChat model layer.
//!
//! Two loopback listeners:
//! - `:3456` gateway (`/v1/models`, `/v1/messages`, Chat / Responses)
//! - `:3458` CCR-compatible control plane (`/api/ccr/rpc`)
//!
//! The process is a sidecar: AgentOS starts it, codeg keeps talking to
//! `CCR_WEB_URL`, and every agent session injects its own model through its
//! per-role CLI home.

mod app;
mod config;
mod control;
mod gateway;
mod import;
mod model_id;
mod profile;
mod quota;
mod store;
mod translate;
mod upstream;

use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::app::App;
use crate::config::Config;
use crate::store::Store;
use crate::upstream::Upstream;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crewchat_model=info".into()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let store = Store::open(&config.db_path())?;
    store.init_schema()?;

    if config.import_on_start {
        match import::run(&config, &store).await {
            Ok(imported) => {
                info!(
                    "imported {} providers from {} ({} claude-visible models)",
                    imported
                        .config
                        .get("Providers")
                        .and_then(|value| value.as_array())
                        .map(|list| list.len())
                        .unwrap_or(0),
                    imported.source,
                    imported.claude_models.len()
                );
            }
            Err(error) => {
                warn!("import skipped: {error}");
            }
        }
    } else {
        info!("import disabled (MODEL_LAYER_IMPORT_ON_START=0)");
    }

    config.write_runtime_file()?;

    let app = App {
        config: config.clone(),
        store,
        upstream: Arc::new(Upstream::new()?),
    };

    let gateway_router = gateway::router(app.clone());
    let control_router = control::router(app.clone());

    let gateway_listener = TcpListener::bind((config.host.as_str(), config.gateway_port)).await?;
    let control_listener = TcpListener::bind((config.host.as_str(), config.control_port)).await?;

    info!("gateway listening on {}", config.gateway_url());
    info!("control listening on {}", config.control_url());

    let gateway_task = tokio::spawn(async move {
        if let Err(error) = axum::serve(gateway_listener, gateway_router).await {
            error!("gateway stopped: {error}");
        }
    });
    let control_task = tokio::spawn(async move {
        if let Err(error) = axum::serve(control_listener, control_router).await {
            error!("control stopped: {error}");
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("shutting down"),
        _ = gateway_task => warn!("gateway task ended"),
        _ = control_task => warn!("control task ended"),
    }
    Ok(())
}
