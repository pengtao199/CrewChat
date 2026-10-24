use std::sync::Arc;

use crate::domain::event::EventEnvelope;
use crate::infra::store::Store;
use crate::runtime::acp::host::sink::EventSink;
use agent_client_protocol::schema::v1::McpServer;
use serde_json::json;

#[test]
fn decodes_mcp_layer_stdio_payload_for_session_new() {
    let payload = json!({
        "type": "stdio",
        "name": "mcp-layer",
        "command": "/opt/homebrew/bin/npx",
        "args": [
            "-y", "mcp-remote", "http://127.0.0.1:41001/",
            "--allow-http", "--header", "Authorization: Bearer test"
        ],
        "env": []
    });
    let server: McpServer = serde_json::from_value(payload).unwrap();
    assert!(matches!(server, McpServer::Stdio(_)));
}

#[test]
fn event_sink_attaches_conversation_and_speaker() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("events.db")).unwrap();
    store.init_schema().unwrap();
    let sink = EventSink::new(Arc::new(store.clone()));
    sink.register("conn", Some("conv".to_string()), Some("role".to_string()));
    sink.emit(EventEnvelope {
        kind: "content_delta".to_string(),
        connection_id: "conn".to_string(),
        conversation_id: None,
        speaker_role_id: None,
        seq: 0,
        payload: json!({"text": "hi"}),
    });

    let rows = store.events_for_conversation("conv", 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["conversation_id"], "conv");
    assert_eq!(rows[0]["speaker_role_id"], "role");
}

#[test]
fn decodes_mcp_layer_http_payload_for_session_new() {
    let payload = json!({
        "type": "http",
        "name": "mcp-layer",
        "url": "http://127.0.0.1:41001/",
        "headers": [
            { "name": "Authorization", "value": "Bearer test" }
        ]
    });
    let server: McpServer = serde_json::from_value(payload).unwrap();
    assert!(matches!(server, McpServer::Http(_)));
}
