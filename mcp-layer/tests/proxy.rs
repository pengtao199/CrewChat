use crewchat_mcp::config::Config;
use crewchat_mcp::proxy::{endpoint, render_proxy_config, ProxyManager};
use crewchat_mcp::registry::{AgentRecord, Registry, ServerSpec};
use mcp_proxy::config::TransportType;
use mcp_proxy::ProxyConfig;
use std::collections::BTreeMap;

fn spec(id: &str) -> ServerSpec {
    ServerSpec {
        id: id.to_string(),
        name: id.to_string(),
        transport: "stdio".to_string(),
        description: String::new(),
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

#[test]
fn renders_a_proxy_config_that_mcp_proxy_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::at(
        dir.path().to_path_buf(),
        "127.0.0.1".to_string(),
        0,
        "control".to_string(),
        None,
    )
    .unwrap();
    let mut registry = Registry::open(&dir.path().join("registry.sqlite")).unwrap();
    let server = registry.upsert_server(&spec("echo")).unwrap();
    let agent = AgentRecord {
        agent_id: "role-1".to_string(),
        client_name: "agent-role-1".to_string(),
        token: "agent-token-12345678".to_string(),
        server_ids: vec!["echo".to_string()],
        port: 0,
        updated_at: String::new(),
    };
    let toml = render_proxy_config(&agent, &[server], 41001).unwrap();
    let path = config.proxy_dir.join("test.toml");
    std::fs::write(&path, toml).unwrap();
    let loaded = ProxyConfig::load(&path).unwrap();
    assert_eq!(loaded.proxy.name, "mcp-layer");
    assert_eq!(loaded.proxy.listen.port, 41001);
    assert_eq!(loaded.backends.len(), 1);
    assert_eq!(loaded.backends[0].name, "echo");
    assert!(matches!(loaded.backends[0].transport, TransportType::Stdio));
    assert_eq!(endpoint(41001), "http://127.0.0.1:41001/");
}

#[tokio::test]
async fn proxy_manager_rejects_empty_agent_selection() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::at(
        dir.path().to_path_buf(),
        "127.0.0.1".to_string(),
        0,
        "control".to_string(),
        None,
    )
    .unwrap();
    let manager = ProxyManager::new(config);
    let agent = AgentRecord {
        agent_id: "role-1".to_string(),
        client_name: "agent-role-1".to_string(),
        token: "agent-token-12345678".to_string(),
        server_ids: Vec::new(),
        port: 0,
        updated_at: String::new(),
    };
    assert!(manager.start_agent(&agent, &[]).await.is_err());
    assert!(manager.status().await.is_empty());
}

#[tokio::test]
async fn proxy_manager_starts_a_stdio_backend() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake_mcp.py");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    method = msg.get("method")
    rid = msg.get("id")
    if method == "initialize":
        out = {"jsonrpc":"2.0","id":rid,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0.0.1"}}}
    elif method == "tools/list":
        out = {"jsonrpc":"2.0","id":rid,"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}}]}}
    elif method == "resources/list":
        out = {"jsonrpc":"2.0","id":rid,"result":{"resources":[]}}
    elif method == "prompts/list":
        out = {"jsonrpc":"2.0","id":rid,"result":{"prompts":[]}}
    elif method == "ping":
        out = {"jsonrpc":"2.0","id":rid,"result":{}}
    elif method == "tools/call":
        out = {"jsonrpc":"2.0","id":rid,"result":{"content":[{"type":"text","text":"ok"}],"isError":False}}
    else:
        if rid is None:
            continue
        out = {"jsonrpc":"2.0","id":rid,"error":{"code":-32601,"message":"method not found"}}
    print(json.dumps(out), flush=True)
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let config = Config::at(
        dir.path().to_path_buf(),
        "127.0.0.1".to_string(),
        0,
        "control".to_string(),
        None,
    )
    .unwrap();
    let mut registry = Registry::open(&dir.path().join("registry.sqlite")).unwrap();
    let mut server = spec("echo");
    server.command = Some("python3".to_string());
    server.args = vec![script.to_string_lossy().to_string()];
    let server = registry.upsert_server(&server).unwrap();
    let manager = ProxyManager::new(config);
    let agent = AgentRecord {
        agent_id: "role-1".to_string(),
        client_name: "agent-role-1".to_string(),
        token: "agent-token-12345678".to_string(),
        server_ids: vec!["echo".to_string()],
        port: 0,
        updated_at: String::new(),
    };
    let port = manager.start_agent(&agent, &[server]).await.unwrap();
    assert!(port > 0);
    let status = manager.status().await;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].state, "running");
    manager.stop_agent("role-1").await;
}
