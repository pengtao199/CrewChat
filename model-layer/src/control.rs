//! CCR-compatible control plane. codeg's model page, `body_model_face` and the
//! per-role overlays read the catalog through this RPC, so the shapes matter
//! more than the implementation.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use crate::app::App;
use crate::config::now;
use crate::upstream::Upstream;

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/ccr/rpc", post(rpc))
        .with_state(app)
}

async fn rpc(State(app): State<App>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authorized(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": {"message": "invalid x-ccr-web-auth"}})),
        )
            .into_response();
    }

    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let args = body
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let result = match method.as_str() {
        "getConfig" => app
            .store
            .build_config()
            .map(|mut config| {
                if let Some(object) = config.as_object_mut() {
                    object.insert(
                        "routerEndpoint".into(),
                        Value::String(app.config.gateway_url()),
                    );
                    if let Some(gateway) = object.get_mut("gateway").and_then(Value::as_object_mut)
                    {
                        gateway.insert("host".into(), Value::String(app.config.host.clone()));
                        gateway.insert("port".into(), json!(app.config.gateway_port));
                    }
                }
                config
            })
            .map_err(|error| error.to_string()),
        "saveConfig" => save_config(&app, args.first()).await,
        "probeProvider" => probe_provider(args.first()).await,
        "getProviderPresets" => Ok(presets()),
        "getProviderAccountSnapshots" => {
            let provider = args.first().and_then(Value::as_str);
            app.store
                .quota_snapshots(provider)
                .map(|snapshots| json!(snapshots))
                .map_err(|error| error.to_string())
        }
        "getGatewayStatus" => Ok(json!({
            "state": "running",
            "endpoint": app.config.gateway_url(),
            "coreEndpoint": app.config.gateway_url(),
            "lastStartedAt": now(),
            "pid": std::process::id(),
        })),
        "startGateway" | "stopGateway" | "restartGateway" => Ok(json!({
            "state": "running",
            "endpoint": app.config.gateway_url(),
        })),
        "applyProfile" => Ok(json!({
            "applied": false,
            "reason": "model-layer does not write global CLI configs; per-agent overlays inject the model",
        })),
        other => Err(format!("unknown method '{other}'")),
    };

    match result {
        Ok(value) => Json(json!({"ok": true, "value": value})).into_response(),
        Err(error) => Json(json!({
            "ok": false,
            "error": {"message": error}
        }))
        .into_response(),
    }
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(value) = headers.get("x-ccr-web-auth") else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    // Constant-time compare is overkill for a loopback management token, but
    // comparing the whole string (not a prefix) keeps the check honest.
    value == app.config.control_token
}

async fn save_config(app: &App, config: Option<&Value>) -> Result<Value, String> {
    let config = config.ok_or_else(|| "saveConfig requires the config argument".to_string())?;
    if !config.is_object() {
        return Err("saveConfig config must be an object".to_string());
    }
    if let Some(provided) = config.get("configRevision").and_then(Value::as_str) {
        let current = app.store.current_revision().map_err(|e| e.to_string())?;
        if provided != current {
            return Err(format!(
                "Configuration changed since it was loaded (configRevision {provided} != {current})"
            ));
        }
    }
    app.store.apply_config(config).map_err(|e| e.to_string())?;
    let revision = app.store.bump_revision().map_err(|e| e.to_string())?;
    Ok(json!({"configRevision": revision}))
}

async fn probe_provider(request: Option<&Value>) -> Result<Value, String> {
    let request = request.ok_or_else(|| "probeProvider requires a request".to_string())?;
    let base_url = request
        .get("baseUrl")
        .or_else(|| request.get("base_url"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if base_url.is_empty() {
        return Err("probeProvider requires baseUrl".to_string());
    }
    let api_key = request
        .get("apiKey")
        .or_else(|| request.get("api_key"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let models = match Upstream::probe_models(&base_url, api_key).await {
        Ok(models) => models,
        Err(error) => {
            return Ok(json!({
                "normalizedBaseUrl": base_url,
                "detectedProtocol": Value::Null,
                "models": [],
                "protocols": [{
                    "protocol": "openai_chat_completions",
                    "endpoint": base_url,
                    "supported": false,
                    "status": error.status.as_u16(),
                    "message": error.message,
                }]
            }));
        }
    };

    let requests = request
        .get("models")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "normalizedBaseUrl": base_url,
        "detectedProtocol": "openai_chat_completions",
        "models": if models.is_empty() { requests } else { models },
        "protocols": [{
            "protocol": "openai_chat_completions",
            "endpoint": base_url,
            "supported": true,
            "status": 200,
        }]
    }))
}

fn presets() -> Value {
    json!([
        {
            "id": "opencode-go",
            "name": "OpenCode Go",
            "aliases": ["opencode", "opencode-zen"],
            "endpoints": [{
                "baseUrl": "https://opencode.ai/zen/go/v1",
                "label": "OpenCode Go",
                "protocols": ["openai_responses", "openai_chat_completions"],
                "websiteUrl": "https://opencode.ai"
            }]
        },
        {
            "id": "openai",
            "name": "OpenAI",
            "endpoints": [{
                "baseUrl": "https://api.openai.com/v1",
                "label": "OpenAI",
                "protocols": ["openai_responses", "openai_chat_completions"]
            }]
        },
        {
            "id": "openrouter",
            "name": "OpenRouter",
            "endpoints": [{
                "baseUrl": "https://openrouter.ai/api/v1",
                "label": "OpenRouter",
                "protocols": ["openai_chat_completions", "anthropic_messages"]
            }]
        },
        {
            "id": "anthropic",
            "name": "Anthropic",
            "endpoints": [{
                "baseUrl": "https://api.anthropic.com/v1",
                "label": "Anthropic",
                "protocols": ["anthropic_messages"]
            }]
        },
        {
            "id": "custom",
            "name": "Custom",
            "endpoints": []
        }
    ])
}
