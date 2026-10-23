use crate::config::Config;
use crate::gateway::Gateway;
use crate::registry::{
    generate_agent_token, sanitize_agent_id, write_server_file, AgentRecord, Registry, ServerSpec,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub registry: Arc<Mutex<Registry>>,
    pub gateway: Gateway,
    pub token: Arc<String>,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    missing: Option<Vec<String>>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            missing: None,
        }
    }

    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_unavailable",
            message,
        )
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        });
        if let Some(missing) = self.missing {
            body["missing"] = json!(missing);
        }
        (self.status, Json(body)).into_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSyncRequest {
    #[serde(default, alias = "server_ids")]
    pub server_ids: Vec<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/servers", get(list_servers))
        .route(
            "/v1/servers/{id}",
            get(get_server).put(put_server).delete(delete_server),
        )
        .route("/v1/servers/{id}/enable", post(enable_server))
        .route("/v1/servers/{id}/disable", post(disable_server))
        .route("/v1/agents", get(list_agents))
        .route(
            "/v1/agents/{id}",
            get(get_agent).put(sync_agent).delete(delete_agent),
        )
        .route("/v1/agents/{id}/sync", post(sync_agent_post))
        .route("/v1/gateway", get(gateway_status))
        .route("/v1/gateway/start", post(gateway_start))
        .route("/v1/gateway/stop", post(gateway_stop))
        .route("/v1/gateway/sync", post(gateway_sync))
        .with_state(state)
}

async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let gateway = state.gateway.status().await;
    Ok(Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "gateway": gateway,
    })))
}

async fn list_servers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let servers = lock_registry(&state)?.list_servers().map_err(internal)?;
    Ok(Json(json!({ "servers": servers })))
}

async fn get_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let server = lock_registry(&state)?
        .get_server(&id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found(format!("server not found: {id}")))?;
    Ok(Json(serde_json::to_value(server).map_err(internal)?))
}

async fn put_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut spec): Json<ServerSpec>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    spec.id = id;
    spec.validate()
        .map_err(|error| ApiError::bad_request("invalid_server", error.to_string()))?;

    let record = {
        let mut registry = lock_registry(&state)?;
        write_server_file(&state.config.servers_dir, &spec).map_err(internal)?;
        registry.upsert_server(&spec).map_err(internal)?
    };

    let gateway_error = match state.gateway.register_server(&record).await {
        Ok(()) => {
            let mut registry = lock_registry(&state)?;
            registry.set_synced(&record.id, true).map_err(internal)?;
            None
        }
        Err(error) => Some(error.to_string()),
    };
    let record = lock_registry(&state)?
        .get_server(&record.id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::internal("server disappeared after sync"))?;
    Ok(Json(json!({
        "server": record,
        "gatewayError": gateway_error,
    })))
}

async fn delete_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let removed = {
        let mut registry = lock_registry(&state)?;
        let removed = registry.delete_server(&id).map_err(internal)?;
        registry.remove_server_from_agents(&id).map_err(internal)?;
        removed
    };
    if !removed {
        return Ok(Json(json!({"removed": false})));
    }
    crate::registry::remove_server_file(&state.config.servers_dir, &id).map_err(internal)?;
    let gateway_error = state
        .gateway
        .delete_server(&id)
        .await
        .err()
        .map(|error| error.to_string());
    Ok(Json(
        json!({"removed": true, "gatewayError": gateway_error}),
    ))
}

async fn enable_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    set_server_enabled(state, headers, id, true).await
}

async fn disable_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    set_server_enabled(state, headers, id, false).await
}

async fn set_server_enabled(
    state: AppState,
    headers: HeaderMap,
    id: String,
    enabled: bool,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let changed = lock_registry(&state)?
        .set_enabled(&id, enabled)
        .map_err(internal)?;
    if !changed {
        return Err(ApiError::not_found(format!("server not found: {id}")));
    }
    let gateway_error = state
        .gateway
        .set_server_enabled(&id, enabled)
        .await
        .err()
        .map(|error| error.to_string());
    if gateway_error.is_none() {
        lock_registry(&state)?
            .set_synced(&id, true)
            .map_err(internal)?;
    }
    Ok(Json(
        json!({"enabled": enabled, "gatewayError": gateway_error}),
    ))
}

async fn list_agents(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let agents = lock_registry(&state)?.list_agents().map_err(internal)?;
    Ok(Json(json!({ "agents": agents })))
}

async fn get_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let agent = lock_registry(&state)?
        .get_agent(&id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found(format!("agent not found: {id}")))?;
    Ok(Json(json!({"agent": agent})))
}

async fn sync_agent_post(
    state: State<AppState>,
    headers: HeaderMap,
    path: Path<String>,
    body: Json<AgentSyncRequest>,
) -> Result<Json<Value>, ApiError> {
    sync_agent_inner(state, headers, path, body).await
}

async fn sync_agent(
    state: State<AppState>,
    headers: HeaderMap,
    path: Path<String>,
    body: Json<AgentSyncRequest>,
) -> Result<Json<Value>, ApiError> {
    sync_agent_inner(state, headers, path, body).await
}

async fn sync_agent_inner(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AgentSyncRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    sanitize_agent_id(&id)
        .map_err(|error| ApiError::bad_request("invalid_agent", error.to_string()))?;

    let mut server_ids = body.server_ids;
    server_ids.sort();
    server_ids.dedup();

    let existing = lock_registry(&state)?.get_agent(&id).map_err(internal)?;
    let client_name = existing
        .as_ref()
        .map(|agent| agent.client_name.clone())
        .unwrap_or_else(|| sanitize_agent_id(&id).unwrap_or_else(|_| "agent-unknown".to_string()));
    let token = existing
        .as_ref()
        .map(|agent| agent.token.clone())
        .unwrap_or_else(generate_agent_token);

    if server_ids.is_empty() {
        let _ = state.gateway.delete_client(&client_name).await;
        lock_registry(&state)?.delete_agent(&id).map_err(internal)?;
        return Ok(Json(json!({
            "agentId": id,
            "clientName": client_name,
            "serverIds": [],
            "mcpServers": [],
            "http": Value::Null,
            "stdio": Value::Null,
        })));
    }

    let servers = lock_registry(&state)?.list_servers().map_err(internal)?;
    let available: Vec<&str> = servers.iter().map(|server| server.id.as_str()).collect();
    let missing: Vec<String> = server_ids
        .iter()
        .filter(|id| !available.contains(&id.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        let mut error = ApiError::new(
            StatusCode::CONFLICT,
            "server_missing",
            format!("servers not installed: {}", missing.join(", ")),
        );
        error.missing = Some(missing);
        return Err(error);
    }

    let record = lock_registry(&state)?
        .upsert_agent(&id, &client_name, &token, &server_ids)
        .map_err(internal)?;
    state
        .gateway
        .sync_agent(&record)
        .await
        .map_err(|error| ApiError::unavailable(error.to_string()))?;

    let session = build_agent_session(&state, &record);
    Ok(Json(serde_json::to_value(session).map_err(internal)?))
}

async fn delete_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let existing = lock_registry(&state)?.get_agent(&id).map_err(internal)?;
    if let Some(agent) = &existing {
        let _ = state.gateway.delete_client(&agent.client_name).await;
    }
    let removed = lock_registry(&state)?.delete_agent(&id).map_err(internal)?;
    Ok(Json(json!({"removed": removed})))
}

async fn gateway_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    Ok(Json(
        serde_json::to_value(state.gateway.status().await).map_err(internal)?,
    ))
}

async fn gateway_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .gateway
        .ensure_started()
        .await
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    Ok(Json(
        serde_json::to_value(state.gateway.status().await).map_err(internal)?,
    ))
}

async fn gateway_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    state.gateway.stop().await;
    Ok(Json(
        serde_json::to_value(state.gateway.status().await).map_err(internal)?,
    ))
}

async fn gateway_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .gateway
        .ensure_started()
        .await
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    let (servers, agents) = {
        let registry = lock_registry(&state)?;
        (
            registry.list_servers().map_err(internal)?,
            registry.list_agents().map_err(internal)?,
        )
    };
    let mut warnings = state
        .gateway
        .reconcile_servers(&servers)
        .await
        .map_err(internal)?;
    warnings.extend(
        state
            .gateway
            .reconcile_agents(&agents)
            .await
            .map_err(internal)?,
    );
    Ok(Json(json!({
        "gateway": state.gateway.status().await,
        "warnings": warnings,
    })))
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentSession {
    agent_id: String,
    client_name: String,
    server_ids: Vec<String>,
    endpoint: String,
    token: String,
    http: Value,
    stdio: Value,
    mcp_servers: Vec<Value>,
}

fn build_agent_session(state: &AppState, agent: &AgentRecord) -> AgentSession {
    let endpoint = state.config.gateway_mcp_url();
    let http = json!({
        "type": "http",
        "name": "mcp-layer",
        "url": endpoint,
        "headers": [
            {"name": "Authorization", "value": format!("Bearer {}", agent.token)}
        ],
    });
    let stdio = state.config.npx.as_ref().map(|npx| {
        json!({
            "type": "stdio",
            "name": "mcp-layer",
            "command": npx.to_string_lossy(),
            "args": [
                "-y",
                "mcp-remote",
                endpoint,
                "--allow-http",
                "--header",
                format!("Authorization: Bearer {}", agent.token),
            ],
            "env": [],
        })
    });
    let mcp_servers = match &stdio {
        Some(stdio) => vec![stdio.clone()],
        None => vec![http.clone()],
    };
    AgentSession {
        agent_id: agent.agent_id.clone(),
        client_name: agent.client_name.clone(),
        server_ids: agent.server_ids.clone(),
        endpoint,
        token: agent.token.clone(),
        http,
        stdio: stdio.unwrap_or(Value::Null),
        mcp_servers,
    }
}

fn authorize(headers: &HeaderMap, token: &str) -> Result<(), ApiError> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if value == Some(token) {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

fn lock_registry(state: &AppState) -> Result<std::sync::MutexGuard<'_, Registry>, ApiError> {
    state
        .registry
        .lock()
        .map_err(|_| ApiError::internal("mcp registry lock is poisoned"))
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::internal(error.to_string())
}
