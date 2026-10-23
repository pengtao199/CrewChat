//! ACP `session/update` → CrewChat event envelope.
//!
//! AgentOS already parses `content_delta`, `thinking`, `tool_call`,
//! `tool_call_update`, `turn_complete` and `status_changed`; keep that shape.

use std::sync::atomic::{AtomicU64, Ordering};

pub use crate::domain::event::EventEnvelope;
use agent_client_protocol::schema::v1::{ContentBlock, SessionNotification, SessionUpdate};
use serde_json::{json, Value};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EventEnvelope>,
    seq: std::sync::Arc<AtomicU64>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            tx,
            seq: std::sync::Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.tx.subscribe()
    }

    /// Stamp the envelope, broadcast it, and return the stamped copy so the
    /// caller can persist the same seq for replay.
    pub fn publish(&self, mut event: EventEnvelope) -> EventEnvelope {
        event.seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(event.clone());
        event
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

pub fn from_notification(connection_id: &str, notification: &SessionNotification) -> EventEnvelope {
    let (kind, payload) = match &notification.update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            ("content_delta", json!({"text": text_of(&chunk.content)}))
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            ("thinking", json!({"text": text_of(&chunk.content)}))
        }
        SessionUpdate::UserMessageChunk(chunk) => {
            ("user_message", json!({"text": text_of(&chunk.content)}))
        }
        SessionUpdate::ToolCall(tool) => {
            let value = serde_json::to_value(tool).unwrap_or(Value::Null);
            (
                "tool_call",
                json!({
                    "tool_call_id": value.get("toolCallId").and_then(Value::as_str).unwrap_or(""),
                    "title": value.get("title").and_then(Value::as_str).unwrap_or("tool"),
                    "status": value.get("status").and_then(Value::as_str).unwrap_or(""),
                    "raw": value,
                }),
            )
        }
        SessionUpdate::ToolCallUpdate(update) => {
            let value = serde_json::to_value(update).unwrap_or(Value::Null);
            (
                "tool_call_update",
                json!({
                    "tool_call_id": value.get("toolCallId").and_then(Value::as_str).unwrap_or(""),
                    "title": value.get("title").cloned().unwrap_or(Value::Null),
                    "status": value.get("status").cloned().unwrap_or(Value::Null),
                    "raw_output": value.get("rawOutput").cloned().unwrap_or(Value::Null),
                    "raw": value,
                }),
            )
        }
        SessionUpdate::Plan(plan) => (
            "plan",
            json!({"plan": serde_json::to_value(plan).unwrap_or(Value::Null)}),
        ),
        SessionUpdate::UsageUpdate(usage) => (
            "usage",
            json!({"usage": serde_json::to_value(usage).unwrap_or(Value::Null)}),
        ),
        SessionUpdate::CurrentModeUpdate(mode) => (
            "status_changed",
            json!({"status": serde_json::to_value(mode).unwrap_or(Value::Null)}),
        ),
        other => (
            "other",
            json!({"session_update": serde_json::to_value(other).unwrap_or(Value::Null)}),
        ),
    };

    EventEnvelope {
        kind: kind.to_string(),
        connection_id: connection_id.to_string(),
        seq: 0,
        payload,
    }
}

fn text_of(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::Image(_) => "[image]".to_string(),
        _ => String::new(),
    }
}

pub fn turn_complete(connection_id: &str, stop_reason: Value) -> EventEnvelope {
    EventEnvelope {
        kind: "turn_complete".to_string(),
        connection_id: connection_id.to_string(),
        seq: 0,
        payload: json!({"stop_reason": stop_reason}),
    }
}
