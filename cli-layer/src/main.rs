//! CrewChat CLI layer.
//!
//! A small Rust sidecar that replaces codeg's ACP/workbench core:
//! - `:3081` HTTP compatibility surface for AgentOS
//! - `/ws/events` streaming
//! - ACP host (15 CLI engines)
//! - SQLite projects/conversations/turns/roster
//! - thin `ccr_*` proxy to the model layer

mod api;
mod app;
mod config;
mod domain;
mod infra;
mod runtime;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::api::http::AppState;
use crate::app::engines::EngineService;
use crate::app::roster::RosterService;
use crate::app::skills::SkillService;
use crate::app::workspace::WorkspaceService;
use crate::config::Config;
use crate::domain::ports::{AgentRepository, ConversationRepository, EngineSettingsRepository};
use crate::infra::model_layer::ModelLayerClient;
use crate::infra::skill_layer::SkillLayerClient;
use crate::infra::store::Store;
use crate::runtime::acp::host::AcpHost;

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

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--import-codeg") {
        let path = args
            .windows(2)
            .find(|pair| pair[0] == "--codeg")
            .map(|pair| PathBuf::from(&pair[1]))
            .unwrap_or_else(default_codeg_path);
        let report = store.import_codeg(&path)?;
        println!(
            "imported {} folders, {} conversations, {} roles from {}",
            report.folders,
            report.conversations,
            report.roles,
            path.display()
        );
        return Ok(());
    }

    config.write_runtime_file()?;

    let skill = Arc::new(SkillLayerClient::new());
    let host = AcpHost::new(config.clone(), store.clone(), skill.clone());
    let model = Arc::new(ModelLayerClient::new(&config));
    let agent_repo: Arc<dyn AgentRepository> = Arc::new(store.clone());
    let conversation_repo: Arc<dyn ConversationRepository> = Arc::new(store.clone());
    let settings_repo: Arc<dyn EngineSettingsRepository> = Arc::new(store.clone());
    let roster = RosterService::new(agent_repo, model.clone(), host.engines().clone());
    let workspace = WorkspaceService::new(conversation_repo);
    let skills = SkillService::new(roster.clone(), skill.clone());
    let engines = EngineService::new(settings_repo, host.engines().clone());
    let state = AppState {
        config: config.clone(),
        store,
        host,
        model,
        roster,
        workspace,
        skills,
        engines,
    };

    let router = crate::api::http::router(state);
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

fn default_codeg_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join("Library/Application Support/AgentOS/core/codeg.db")
}
