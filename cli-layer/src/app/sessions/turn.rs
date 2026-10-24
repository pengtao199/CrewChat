//! Turn execution and result shaping.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::time::timeout;

use crate::domain::agent::Agent;
use crate::domain::ports::{ConversationRepository, EventSubscription};

pub(super) async fn wait_for_turn(
    connection_id: &str,
    mut events: Box<dyn EventSubscription>,
    timeout_seconds: u64,
) -> (bool, String, String, String, Value, Vec<String>, u64) {
    timeout(Duration::from_secs(timeout_seconds), async {
        let mut text = String::new();
        let mut usage = Value::Null;
        let mut artifacts = Vec::new();
        let mut events_seen = 0u64;
        loop {
            let Some(event) = events.recv().await else {
                return (
                    false,
                    "protocol".to_string(),
                    "event stream closed".to_string(),
                    text,
                    usage,
                    artifacts,
                    events_seen,
                );
            };
            if event.connection_id != connection_id {
                continue;
            }
            events_seen += 1;
            match event.kind.as_str() {
                "content_delta" => {
                    if let Some(delta) = event.payload.get("text").and_then(Value::as_str) {
                        text.push_str(delta);
                    }
                }
                "usage" => usage = event.payload.get("usage").cloned().unwrap_or(Value::Null),
                "tool_call" | "tool_call_update" => {
                    collect_artifacts(&event.payload, &mut artifacts);
                }
                "permission_request" => {
                    return (
                        false,
                        "permission".to_string(),
                        "the prompt raised a permission request".to_string(),
                        text,
                        usage,
                        artifacts,
                        events_seen,
                    )
                }
                "turn_complete" => {
                    if let Some(error) = event.payload.get("error").and_then(Value::as_str) {
                        return (
                            false,
                            "protocol".to_string(),
                            error.to_string(),
                            text,
                            usage,
                            artifacts,
                            events_seen,
                        );
                    }
                    let stopped = event
                        .payload
                        .get("stop_reason")
                        .map(Value::to_string)
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if stopped.contains("cancel") {
                        return (
                            false,
                            "cancelled".to_string(),
                            "the turn was cancelled".to_string(),
                            text,
                            usage,
                            artifacts,
                            events_seen,
                        );
                    }
                    return (
                        true,
                        "complete".to_string(),
                        String::new(),
                        text,
                        usage,
                        artifacts,
                        events_seen,
                    );
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        (
            false,
            "timeout".to_string(),
            format!("the prompt did not finish within {timeout_seconds} seconds"),
            String::new(),
            Value::Null,
            Vec::new(),
            0,
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn turn_result(
    conversations: &dyn ConversationRepository,
    ok: bool,
    phase: String,
    error: String,
    text: String,
    usage: Value,
    artifacts: Vec<String>,
    events_seen: u64,
    model: Option<String>,
    elapsed: Duration,
    connection_id: Option<String>,
    conversation_id: Option<&str>,
    speaker_role_id: Option<&str>,
) -> Value {
    if ok {
        if let Some(conversation_id) = conversation_id {
            let _ = conversations.append_turn_with_speaker(
                conversation_id,
                "assistant",
                speaker_role_id,
                &json!([{"type": "text", "text": text}]),
            );
        }
    }
    let mut value = test_result(
        ok,
        &phase,
        error,
        model,
        elapsed,
        Some(usage),
        connection_id,
    );
    value["text"] = json!(text);
    value["artifacts"] = json!(artifacts);
    value["events"] = json!(events_seen);
    value
}

pub(super) fn collect_artifacts(payload: &Value, artifacts: &mut Vec<String>) {
    for value in [payload.get("raw"), payload.get("raw_output")]
        .into_iter()
        .flatten()
    {
        let locations = value
            .get("locations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for location in locations {
            if let Some(path) = location.get("path").and_then(Value::as_str) {
                if !path.is_empty() && !artifacts.iter().any(|item| item == path) {
                    artifacts.push(path.to_string());
                }
            }
        }
        if let Some(path) = value.get("path").and_then(Value::as_str) {
            if !path.is_empty() && !artifacts.iter().any(|item| item == path) {
                artifacts.push(path.to_string());
            }
        }
    }
}

pub(super) fn connection_id_of(value: &Value) -> String {
    value
        .get("connectionId")
        .or_else(|| value.get("connection_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(super) fn model_label(agent: &Agent) -> Option<String> {
    if agent.model.trim().is_empty() {
        return None;
    }
    if agent.model_provider.trim().is_empty() {
        return Some(agent.model.clone());
    }
    Some(format!("{} / {}", agent.model_provider, agent.model))
}

pub(super) fn test_result(
    ok: bool,
    phase: &str,
    error: String,
    model: Option<String>,
    elapsed: Duration,
    usage: Option<Value>,
    connection_id: Option<String>,
) -> Value {
    json!({
        "ok": ok,
        "phase": phase,
        "error": if ok { Value::Null } else { json!(error) },
        "model": model,
        "durationMs": elapsed.as_millis() as u64,
        "usage": usage.unwrap_or(Value::Null),
        "connectionId": connection_id,
    })
}
