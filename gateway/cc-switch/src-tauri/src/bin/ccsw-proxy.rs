//! Headless cc-switch core: the proxy plus a small local control surface.
//!
//! `serve` runs the same `ProxyService` the desktop app uses, but passes no
//! Tauri handle, so it can be supervised as a plain background process.
//!
//! Mode switching (entering/leaving proxy takeover) has to run *in this
//! process*: `ProxyService::is_running` tracks the server per-process, so a
//! separate control process would see "not running" and try to bind the port
//! that this process already holds. We therefore expose the officially exported
//! `mode::controller` over a loopback-only HTTP surface that our app drives.
//!
//! Everything stateful lives in `~/.cc-switch/`, so the GUI, this process and
//! our app all agree on the same providers, routes and history.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};

use cc_switch_lib::mode::{controller, current};
use cc_switch_lib::{AppState, AppType, Database};

const DEFAULT_CONTROL_PORT: u16 = 15722;

/// Apps whose client config the proxy can take over.
const PROXY_APPS: [AppType; 4] = [
    AppType::Claude,
    AppType::Codex,
    AppType::Gemini,
    AppType::GrokBuild,
];

struct Core {
    state: Arc<AppState>,
}

#[derive(serde::Deserialize)]
struct RouteBody {
    app: String,
    #[serde(default)]
    route: Option<String>,
}

fn parse_app(name: &str) -> Result<AppType, String> {
    name.parse::<AppType>().map_err(|_| format!("unknown app: {name}"))
}

async fn enter(
    State(core): State<Arc<Core>>,
    Json(body): Json<RouteBody>,
) -> Result<Json<serde_json::Value>, String> {
    let app = parse_app(&body.app)?;
    controller::enter_with_route(&core.state, &app, false, body.route.as_deref()).await?;
    Ok(Json(serde_json::json!({ "ok": true, "app": app.as_str() })))
}

async fn exit(
    State(core): State<Arc<Core>>,
    Json(body): Json<RouteBody>,
) -> Result<Json<serde_json::Value>, String> {
    let app = parse_app(&body.app)?;
    controller::exit(&core.state, &app).await?;
    Ok(Json(serde_json::json!({ "ok": true, "app": app.as_str() })))
}

async fn shutdown(State(core): State<Arc<Core>>) -> Json<serde_json::Value> {
    let _ = core.state.proxy_service.stop().await;
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({ "ok": true }))
}

async fn modes() -> Json<serde_json::Value> {
    let modes = PROXY_APPS
        .iter()
        .map(|app| {
            (
                app.as_str().to_string(),
                serde_json::Value::Bool(current::is_proxy(app)),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Json(serde_json::json!({ "proxy_apps": modes }))
}

/// 扫描全部受支持 CLI 的安装分布（复用 cc-switch 的工具层，只读）。
async fn tools() -> Result<Json<serde_json::Value>, String> {
    let all = cc_switch_lib::VALID_TOOLS.map(String::from).to_vec();
    let reports = cc_switch_lib::probe_tool_installations(all).await?;
    serde_json::to_value(reports).map(Json).map_err(|e| e.to_string())
}

/// 每个工具的当前版本与**远程最新版本**（复用 cc-switch 的工具层，含网络请求）。
/// 用于判定"有没有新版"，从而只在有新版时才显示更新。
async fn tool_versions() -> Result<Json<serde_json::Value>, String> {
    let versions = cc_switch_lib::get_tool_versions(None, None).await?;
    serde_json::to_value(versions).map(Json).map_err(|e| e.to_string())
}

#[derive(serde::Deserialize)]
struct LifecycleBody {
    tools: Vec<String>,
    action: String,
}

/// 安装/更新 CLI（复用 cc-switch 的生命周期命令，会真正执行系统命令）。
async fn tools_lifecycle(Json(body): Json<LifecycleBody>) -> Result<Json<serde_json::Value>, String> {
    cc_switch_lib::run_tool_lifecycle_action(body.tools, body.action, None).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn run_control(core: Arc<Core>, port: u16) -> anyhow::Result<()> {
    let router = Router::new()
        .route("/enter", post(enter))
        .route("/exit", post(exit))
        .route("/shutdown", post(shutdown))
        .route("/modes", get(modes))
        .route("/tools", get(tools))
        .route("/tools/versions", get(tool_versions))
        .route("/tools/lifecycle", post(tools_lifecycle))
        .with_state(core);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("ccsw-proxy control surface on http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut control_port = DEFAULT_CONTROL_PORT;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "serve" => {}
            "--control-port" => {
                control_port = args
                    .next()
                    .expect("--control-port needs a value")
                    .parse()
                    .expect("--control-port must be a number");
            }
            other => anyhow::bail!("unknown argument: {other} (expected: serve)"),
        }
    }

    let db = Arc::new(Database::init()?);
    let state = Arc::new(AppState::new(db));

    let info = state.proxy_service.start().await.map_err(anyhow::Error::msg)?;
    println!("ccsw-proxy listening on http://{}:{}", info.address, info.port);

    let core = Arc::new(Core { state });

    // Keep the process alive for as long as either task is running.
    tokio::select! {
        result = run_control(core, control_port) => result,
        // Stay alive until the supervisor signals us; SQLite tolerates the
        // shutdown because every write is a committed transaction.
        _ = futures::future::pending::<()>() => Ok(()),
    }
}
