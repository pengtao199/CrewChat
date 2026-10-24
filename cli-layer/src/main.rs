//! CrewChat CLI layer.
//!
//! A small Rust sidecar: the ACP/workbench core.
//! - `:3081` HTTP compatibility surface for AgentOS
//! - `/ws/events` streaming
//! - ACP host (15 CLI engines)
//! - SQLite projects/conversations/turns/roster
//! - thin `model_*` proxy to the model layer

mod api;
mod app;
mod config;
mod domain;
mod infra;
mod runtime;
mod team_mcp;

use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::api::http::AppState;
use crate::app::engines::EngineService;
use crate::app::mcp::McpService;
use crate::app::models::ModelService;
use crate::app::permissions::PermissionService;
use crate::app::readiness::ReadinessService;
use crate::app::roster::RosterService;
use crate::app::sessions::SessionService;
use crate::app::skills::SkillService;
use crate::app::team::TeamService;
use crate::app::workspace::WorkspaceService;
use crate::config::Config;
use crate::domain::ports::{
    AgentRepository, ConversationRepository, EngineSettingsRepository, EventRepository, McpCatalog,
    ModelCatalog, SessionRuntime, SkillCatalog, TeamRepository,
};
use crate::infra::mcp_layer::McpLayerClient;
use crate::infra::model_layer::ModelLayerClient;
use crate::infra::skill_layer::SkillLayerClient;
use crate::infra::store::Store;
use crate::runtime::acp::host::AcpHost;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("team-mcp") {
        return crate::team_mcp::run().await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crewchat_cli=info".into()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let store = Store::open(&config.db_path())?;
    store.init_schema()?;
    crate::app::presets::ensure(&store)?;
    config.write_runtime_file()?;

    let skill: Arc<dyn SkillCatalog> = Arc::new(SkillLayerClient::new());
    let mcp_catalog: Arc<dyn McpCatalog> = Arc::new(McpLayerClient::new());
    let model = Arc::new(ModelLayerClient::new(&config));
    let model_catalog: Arc<dyn ModelCatalog> = model.clone();
    let models = ModelService::new(model_catalog.clone());
    let event_repo: Arc<dyn EventRepository> = Arc::new(store.clone());
    let host = Arc::new(AcpHost::new(
        config.clone(),
        event_repo,
        skill.clone(),
        mcp_catalog.clone(),
    ));
    let runtime: Arc<dyn SessionRuntime> = host.clone();
    let agent_repo: Arc<dyn AgentRepository> = Arc::new(store.clone());
    let conversation_repo: Arc<dyn ConversationRepository> = Arc::new(store.clone());
    let team_repo: Arc<dyn TeamRepository> = Arc::new(store.clone());
    let settings_repo: Arc<dyn EngineSettingsRepository> = Arc::new(store.clone());
    let roster = RosterService::new(agent_repo.clone(), model.clone(), host.engines().clone());
    let workspace = WorkspaceService::new(conversation_repo.clone());
    let skills = SkillService::new(roster.clone(), skill.clone());
    let mcp = McpService::new(roster.clone(), mcp_catalog);
    let engines = EngineService::new(settings_repo, host.engines().clone(), config.clone());
    let readiness = ReadinessService::new(
        roster.clone(),
        engines.clone(),
        model_catalog.clone(),
        skill.clone(),
    );
    let sessions = SessionService::new(
        agent_repo,
        conversation_repo.clone(),
        runtime,
        model_catalog,
        engines.clone(),
        Some(team_repo.clone()),
    );
    let team = TeamService::new(
        roster.clone(),
        conversation_repo,
        team_repo,
        readiness.clone(),
        sessions.clone(),
        Some(host.clone()),
    );
    let state = AppState {
        config: config.clone(),
        models,
        roster,
        workspace,
        skills,
        mcp,
        readiness,
        permissions: PermissionService::new(),
        engines,
        sessions,
        team,
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
