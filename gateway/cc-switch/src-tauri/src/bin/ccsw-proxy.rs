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

use std::collections::BTreeMap;
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

/// [crewchat] 只做一件事：把盘上的客户端配置收敛到当前契约，**不改模式、不改路由**。
///
/// 调用方（多 CLI 编排器）在每个 CLI 起进程之前调它。返回 `changed` 说明这次真的
/// 重写了盘上的配置，`false` 说明本来就是收敛的。之所以单独开一个端点而不是复用
/// `/enter`：进入模式是「切到某家」，收敛是「确认盘上就是我以为的那样」——两件事，
/// 后者必须能在不改变用户选择的前提下反复调用。
async fn ensure(
    State(core): State<Arc<Core>>,
    Json(body): Json<RouteBody>,
) -> Result<Json<serde_json::Value>, String> {
    let app = parse_app(&body.app)?;
    controller::ensure_projection(&core.state, &app).await?;
    Ok(Json(serde_json::json!({ "ok": true, "app": app.as_str() })))
}

/// [crewchat] 这个代理进程的身份：它用的是哪份数据目录、哪个库、哪些端口、谁在跑。
///
/// 调用方用它判断「端口上那个代理是不是我起的」。只探端口是不够的：端口上可能是
/// 另一个 cc-switch 实例（默认数据目录），连上去会读到别人的供应商表，还会被当成
/// 自己那份在用。身份对不上就必须拒连，而不是静默用错库。
async fn identity(State(_core): State<Arc<Core>>) -> Json<serde_json::Value> {
    // 报**解析后**的路径，不是环境变量原文：调用方要拿它和自己期望的目录比，
    // 没设环境变量时解析结果才是真正生效的那个。
    let dir = cc_switch_lib::get_app_config_dir();
    let db = dir.join("cc-switch.db").display().to_string();
    Json(serde_json::json!({
        "ok": true,
        "pid": std::process::id(),
        "home": dir.display().to_string(),
        "db": db,
        "exe": std::env::current_exe().ok().map(|p| p.display().to_string()).unwrap_or_default(),
    }))
}

#[derive(serde::Deserialize)]
struct StackRegisterBody {
    app: String,
    provider_id: String,
    model: String,
}

/// [crewchat] 登记「共存」app 为 Stack 成员，返回发布模型 id。
///
/// 与 `/stack/route` 的区别：不改写该 app 的客户端配置文件——它的配置由编排层每次
/// spawn 时自己生成（见 AGENTS/bodies.md）。CC Switch 只做 key 分配、成员登记、
/// 「代理 + 聚合」模式落定；反代据此按发布 id 逐请求路由，真实 key 留在网关侧。
async fn stack_register(
    State(core): State<Arc<Core>>,
    Json(body): Json<StackRegisterBody>,
) -> Result<Json<serde_json::Value>, String> {
    let app = parse_app(&body.app)?;
    let model_id =
        controller::register_stack_member(&core.state, &app, &body.provider_id, &body.model)
            .await?;
    Ok(Json(serde_json::json!({ "ok": true, "model_id": model_id })))
}

#[derive(serde::Deserialize)]
struct StackRouteBody {
    app: String,
    provider_id: String,
    #[serde(default)]
    model: String,
}

/// [crewchat] 把某 CLI 的一家供应商加入 Stack 名单，返回该模型的**发布 id**。
///
/// Stack 模式下每个发布 id 自带供应商（claude `ccs-claude-<key>--<model>`、
/// codex `ccs-<key>/<model>`），代理按 id **逐请求**路由 —— 因此同一个 CLI 能同时
/// 用多家/多模型，而不是被一条「当前路由」锁死。这正是「一个引擎跑多个模型」。
async fn stack_route(
    State(core): State<Arc<Core>>,
    Json(body): Json<StackRouteBody>,
) -> Result<Json<serde_json::Value>, String> {
    let app = parse_app(&body.app)?;
    if !cc_switch_lib::mode::stack::supports_stack(&app) {
        return Err(format!("{} 不支持 Stack 模式", app.as_str()));
    }
    let active = controller::stack_views(&core.state, &app)
        .map(|view| view.active)
        .unwrap_or(false);
    // 每次都收敛一次，不靠「首次进入」这个一次性副作用。以前只在 `!active` 时写 CLI
    // 配置：只要记账说已经在代理模式，之后就只更新名单、再也不校验盘上的文件。配置一旦
    // 被客户端刷新或被别的程序改回，就永远补不回来——多 CLI 并行时必然出现「一个能用、
    // 一个一直报未登录」。写盘本身幂等（write_proxy 会拿盘上实际内容判收敛），所以
    // 多这一次没有副作用。
    controller::enter_with_route(&core.state, &app, true, Some(&body.provider_id)).await?;
    if !active {
        log::info!("[STACK] {} 首次进入聚合模式", app.as_str());
    }
    // 其余家：加入名单（幂等；默认那家已在名单里）。
    controller::set_stack_member(&core.state, &app, &body.provider_id, true)
        .await
        .map_err(|error| error.message)?;
    let stack = cc_switch_lib::mode::state::stack(
        &cc_switch_lib::live::engine::DeviceStore::for_device(),
        app.as_str(),
    )
    .map_err(|error| error.to_string())?;
    let key = stack
        .key_of(&body.provider_id)
        .ok_or_else(|| format!("供应商不在 Stack 名单: {}", body.provider_id))?;
    let model_id = cc_switch_lib::mode::stack::encode(&app, key, &body.model, false);
    Ok(Json(serde_json::json!({ "ok": true, "model_id": model_id })))
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

// ----- 模型能力 -------------------------------------------------------------
//
// 复用 cc-switch 已有的模型元数据层，不在 Python 侧重写：
// - `services::model_fetch`：成熟的 /v1/models 客户端（版本段阶梯 + 按协议鉴权）
// - `model_capabilities`：视觉能力判定（精确名称登记表 + 显式声明优先）
// - `services::model_pricing`：models.dev 价格目录

#[derive(serde::Deserialize)]
struct FetchModelsBody {
    base_url: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    is_full_url: bool,
    #[serde(default)]
    models_url_override: Option<String>,
    #[serde(default)]
    api_format: Option<String>,
    #[serde(default)]
    request_headers: Option<BTreeMap<String, String>>,
}

/// 从供应商端点拉取可用模型列表。手动添加模型时的辅助（不是主路径）。
async fn models_fetch(Json(body): Json<FetchModelsBody>) -> Result<Json<serde_json::Value>, String> {
    let user_agent = axum::http::HeaderValue::from_static("ccsw-proxy/1.0");
    let models = cc_switch_lib::services::model_fetch::fetch_models(
        &body.base_url,
        &body.api_key,
        body.is_full_url,
        body.models_url_override.as_deref(),
        Some(user_agent),
        body.api_format.as_deref(),
        body.request_headers.as_ref(),
    )
    .await?;
    serde_json::to_value(models).map(Json).map_err(|e| e.to_string())
}

#[derive(serde::Deserialize)]
struct CapabilitiesBody {
    models: Vec<String>,
}

/// 判定一批模型的图像输入能力。`unknown` 表示登记表里没有结论，调用方应按
/// fail-open 处理（不要据此禁用）。
async fn models_capabilities(
    Json(body): Json<CapabilitiesBody>,
) -> Result<Json<serde_json::Value>, String> {
    use cc_switch_lib::model_capabilities::{resolve_image_input_capability, ImageInputCapability};

    let out: Vec<serde_json::Value> = body
        .models
        .iter()
        .map(|id| {
            let capability = resolve_image_input_capability(id, None, true);
            let vision = match capability {
                ImageInputCapability::Supported => Some(true),
                ImageInputCapability::Unsupported => Some(false),
                ImageInputCapability::Unknown => None,
            };
            serde_json::json!({ "model": id, "vision": vision })
        })
        .collect();
    Ok(Json(serde_json::json!({ "models": out })))
}

/// 本地 models.dev 价格目录（每百万 token 成本），供模型列表展示成本。
async fn models_catalog(State(core): State<Arc<Core>>) -> Result<Json<serde_json::Value>, String> {
    let pricing =
        cc_switch_lib::services::model_pricing::list_model_pricing(&core.state.db)
            .map_err(|e| e.to_string())?;
    serde_json::to_value(pricing).map(Json).map_err(|e| e.to_string())
}

async fn run_control(core: Arc<Core>, port: u16) -> anyhow::Result<()> {
    let router = Router::new()
        .route("/enter", post(enter))
        .route("/exit", post(exit))
        .route("/ensure", post(ensure))
        .route("/identity", get(identity))
        .route("/stack/route", post(stack_route))
        .route("/stack/register", post(stack_register))
        .route("/shutdown", post(shutdown))
        .route("/modes", get(modes))
        .route("/tools", get(tools))
        .route("/tools/versions", get(tool_versions))
        .route("/tools/lifecycle", post(tools_lifecycle))
        .route("/models/fetch", post(models_fetch))
        .route("/models/capabilities", post(models_capabilities))
        .route("/models/catalog", get(models_catalog))
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
