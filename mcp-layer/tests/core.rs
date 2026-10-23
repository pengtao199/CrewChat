use crewchat_mcp::registry::{
    generate_agent_token, sanitize_agent_id, validate_id, Registry, ServerSpec,
};
use std::collections::BTreeMap;

fn stdio_spec(id: &str) -> ServerSpec {
    ServerSpec {
        id: id.to_string(),
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

#[test]
fn server_spec_validates_and_builds_gateway_payload() {
    let spec = stdio_spec("echo");
    spec.validate().unwrap();
    let payload = spec.gateway_payload();
    assert_eq!(payload["name"], "echo");
    assert_eq!(payload["transport"], "stdio");
    assert_eq!(payload["command"], "/usr/bin/node");

    let mut remote = spec.clone();
    remote.id = "remote".to_string();
    remote.transport = "http".to_string();
    remote.command = None;
    remote.url = Some("http://127.0.0.1:9000/mcp".to_string());
    remote.headers.insert("X-Test".to_string(), "1".to_string());
    remote.validate().unwrap();
    let payload = remote.gateway_payload();
    assert_eq!(payload["transport"], "streamable_http");
    assert_eq!(payload["url"], "http://127.0.0.1:9000/mcp");
    assert_eq!(payload["headers"]["X-Test"], "1");
}

#[test]
fn registry_round_trips_servers_and_agents() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = Registry::open(&dir.path().join("registry.sqlite")).unwrap();
    let spec = stdio_spec("echo");
    let server = registry.upsert_server(&spec).unwrap();
    assert_eq!(server.id, "echo");
    assert_eq!(registry.list_servers().unwrap().len(), 1);

    let token = generate_agent_token();
    let agent = registry
        .upsert_agent("agent-1", "agent-agent-1", &token, &["echo".to_string()], 0)
        .unwrap();
    assert_eq!(agent.server_ids, vec!["echo"]);
    assert_eq!(registry.list_agents().unwrap().len(), 1);

    registry.remove_server_from_agents("echo").unwrap();
    assert!(registry
        .get_agent("agent-1")
        .unwrap()
        .unwrap()
        .server_ids
        .is_empty());

    assert!(registry.delete_server("echo").unwrap());
    assert!(registry.get_server("echo").unwrap().is_none());
    assert!(registry.delete_agent("agent-1").unwrap());
}

#[test]
fn ids_are_strict_but_agent_ids_are_sanitized() {
    assert!(validate_id("backend-api").is_ok());
    assert!(validate_id("../escape").is_err());
    assert!(validate_id("a/b").is_err());
    assert_eq!(sanitize_agent_id("role 1/2").unwrap(), "agent-role-1-2");
    assert!(sanitize_agent_id("").is_err());
}

#[test]
fn spec_hash_is_stable() {
    let spec = stdio_spec("echo");
    assert_eq!(spec.hash().unwrap(), spec.hash().unwrap());
    let mut changed = spec.clone();
    changed.args.push("--flag".to_string());
    assert_ne!(spec.hash().unwrap(), changed.hash().unwrap());
}
