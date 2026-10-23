use anyhow::Result;
use crewchat_skill::config::Config;
use crewchat_skill::http::{router, AppState};
use crewchat_skill::store::Store;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "crewchat-skill\n\n\
             Local skill layer daemon.\n\n\
             Environment:\n\
               SKILL_LAYER_ROOT           default ~/.agents\n\
               SKILL_LAYER_HOST           default 127.0.0.1 (loopback only)\n\
               SKILL_LAYER_PORT           default 3460\n\
               SKILL_LAYER_TOKEN          optional; generated and written to runtime.json\n\
               SKILL_LAYER_SESSION_ROOT   optional; materialize only below this directory"
        );
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let store = Store::open(config.clone())?;
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        token: Arc::new(config.token.clone()),
    };

    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port)).await?;
    config.write_runtime()?;

    println!("crewchat-skill listening on {}", config.url());
    println!("runtime file: {}", config.runtime_path.display());

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
