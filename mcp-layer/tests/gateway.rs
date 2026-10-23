use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use crewchat_mcp::config::Config;
use crewchat_mcp::gateway::Gateway;
use crewchat_mcp::registry::{AgentRecord, Registry, ServerSpec};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct FakeState {
    clients: Arc<Mutex<Vec<Value>>>,
    server_posts: Arc<Mutex<Vec<Value>>>,
    deleted_servers: Arc<Mutex<Vec<String>>>,
}

fn spec() -> ServerSpec {
    ServerSpec {
        id: "echo".to_string(),
        name: "Echo".to_string(),
        transport: "stdio".to_string(),
        description: "test".to_string(),
        enabled: true,
        command: Some("/usr/bin/node".to_string()),
        args: vec!["server.js".to_string()],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
        bearer_token: None,
        session_mode: None,
    }
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn list_servers() -> Json<Value> {
    Json(json!([]))
}

async fn register_server(
    State(state): State<FakeState>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    state.server_posts.lock().unwrap().push(body.clone());
    (StatusCode::CREATED, Json(body))
}

async fn delete_server(State(state): State<FakeState>, Path(name): Path<String>) -> StatusCode {
    state.deleted_servers.lock().unwrap().push(name);
    StatusCode::NO_CONTENT
}

async fn set_enabled(Path(_name): Path<String>) -> StatusCode {
    StatusCode::OK
}

async fn list_clients(State(state): State<FakeState>) -> Json<Value> {
    Json(json!(state.clients.lock().unwrap().clone()))
}

async fn create_client(
    State(state): State<FakeState>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    state.clients.lock().unwrap().push(body.clone());
    (StatusCode::CREATED, Json(body))
}

async fn delete_client(Path(_name): Path<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn fake_gateway() -> (u16, FakeState) {
    let state = FakeState::default();
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/v0/servers", get(list_servers).post(register_server))
        .route("/api/v0/servers/{name}", delete(delete_server))
        .route("/api/v0/servers/{name}/enable", post(set_enabled))
        .route("/api/v0/servers/{name}/disable", post(set_enabled))
        .route("/api/v0/clients", get(list_clients).post(create_client))
        .route("/api/v0/clients/{name}", delete(delete_client))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, state)
}

fn config(root: &std::path::Path, gateway_port: u16) -> Config {
    Config::at(
        root.to_path_buf(),
        "127.0.0.1".to_string(),
        0,
        "test-token".to_string(),
        gateway_port,
        root.join("missing-gateway"),
        None,
    )
    .unwrap()
}

#[tokio::test]
async fn gateway_registers_servers_and_syncs_agent_clients() {
    let (port, fake) = fake_gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), port);
    let gateway = Gateway::attach(config, "admin-token".to_string());

    let mut registry = Registry::open(&dir.path().join("registry.sqlite")).unwrap();
    let record = registry.upsert_server(&spec()).unwrap();
    gateway.register_server(&record).await.unwrap();
    let posts = fake.server_posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["name"], "echo");
    assert_eq!(posts[0]["transport"], "stdio");

    let agent = AgentRecord {
        agent_id: "role-1".to_string(),
        client_name: "agent-role-1".to_string(),
        token: "agent-token-12345678".to_string(),
        server_ids: vec!["echo".to_string()],
        updated_at: "now".to_string(),
    };
    gateway.sync_agent(&agent).await.unwrap();
    let clients = fake.clients.lock().unwrap().clone();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["name"], "agent-role-1");
    assert_eq!(clients[0]["access_token"], "agent-token-12345678");
    assert_eq!(clients[0]["allow_list"], json!(["echo"]));

    gateway.delete_server("echo").await.unwrap();
    assert_eq!(
        fake.deleted_servers.lock().unwrap().clone(),
        vec!["echo".to_string()]
    );
}

#[tokio::test]
async fn http_api_exposes_agent_session_config() {
    use crewchat_mcp::http::{router, AppState};
    use std::sync::Mutex;

    let (port, _fake) = fake_gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), port);
    let registry = Registry::open(&dir.path().join("registry.sqlite")).unwrap();
    let state = AppState {
        config: Arc::new(config.clone()),
        registry: Arc::new(Mutex::new(registry)),
        gateway: Gateway::attach(config.clone(), "admin-token".to_string()),
        token: Arc::new("control-token".to_string()),
    };
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let control_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{control_port}");
    let auth = "Bearer control-token";

    let response = client
        .put(format!("{base}/v1/servers/echo"))
        .header("authorization", auth)
        .json(&spec())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());

    let response = client
        .put(format!("{base}/v1/agents/role-1"))
        .header("authorization", auth)
        .json(&json!({"serverIds": ["echo"]}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["agentId"], "role-1");
    assert_eq!(body["serverIds"], json!(["echo"]));
    assert_eq!(body["http"]["type"], "http");
    assert_eq!(body["mcpServers"][0]["type"], "http");
    assert!(body["endpoint"].as_str().unwrap().ends_with("/mcp"));

    let response = client
        .delete(format!("{base}/v1/agents/role-1"))
        .header("authorization", auth)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
}
