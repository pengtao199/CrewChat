//! Turn execution and history shaping.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::time::timeout;

use crate::config::now;
use crate::domain::agent::Agent;
use crate::domain::event::EventEnvelope;
use crate::domain::ports::{ConversationRepository, EventSubscription};

#[derive(Debug)]
pub(super) struct TurnOutcome {
    pub ok: bool,
    pub phase: String,
    pub error: String,
    pub text: String,
    pub blocks: Vec<Value>,
    pub usage: Value,
    pub artifacts: Vec<String>,
    pub events_seen: u64,
}

struct ToolCall {
    id: String,
    name: String,
    input_preview: String,
    status: String,
    started_at: String,
    started: Instant,
    output: Option<String>,
}

#[derive(Default)]
struct TurnAccumulator {
    text: String,
    thinking: String,
    usage: Value,
    artifacts: Vec<String>,
    events_seen: u64,
    tools: Vec<ToolCall>,
    tool_index: HashMap<String, usize>,
}

impl TurnAccumulator {
    fn apply(&mut self, event: &EventEnvelope) {
        self.events_seen += 1;
        match event.kind.as_str() {
            "content_delta" => {
                if let Some(delta) = event.payload.get("text").and_then(Value::as_str) {
                    self.text.push_str(delta);
                }
            }
            "thinking" => {
                if let Some(delta) = event.payload.get("text").and_then(Value::as_str) {
                    self.thinking.push_str(delta);
                }
            }
            "usage" => self.usage = event.payload.get("usage").cloned().unwrap_or(Value::Null),
            "tool_call" => self.upsert_tool(&event.payload, false),
            "tool_call_update" => self.upsert_tool(&event.payload, true),
            _ => {}
        }
    }

    fn upsert_tool(&mut self, payload: &Value, update: bool) {
        collect_artifacts(payload, &mut self.artifacts);
        let id = payload
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let name = payload
            .get("tool_name")
            .or_else(|| payload.get("name"))
            .or_else(|| payload.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let status = payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let input_preview = payload
            .get("input_preview")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let output = preview(
            payload
                .get("raw_output")
                .or_else(|| payload.get("output_preview")),
        );

        if let Some(index) = self.tool_index.get(&id).copied() {
            let tool = &mut self.tools[index];
            if !name.is_empty() {
                tool.name = name;
            }
            if !status.is_empty() {
                tool.status = status;
            }
            if !input_preview.is_empty() {
                tool.input_preview = input_preview;
            }
            if output.is_some() {
                tool.output = output;
            }
            return;
        }
        if update {
            // Updates can arrive before the initial call; keep a placeholder.
        }
        self.tool_index.insert(id.clone(), self.tools.len());
        self.tools.push(ToolCall {
            id,
            name,
            input_preview,
            status,
            started_at: payload
                .get("started_at")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(now),
            started: Instant::now(),
            output,
        });
    }

    fn blocks(self) -> Vec<Value> {
        let mut blocks = Vec::new();
        if !self.thinking.is_empty() {
            blocks.push(json!({"type": "thinking", "text": self.thinking}));
        }
        for tool in &self.tools {
            let duration = tool.started.elapsed().as_millis() as u64;
            blocks.push(json!({
                "type": "tool_use",
                "tool_call_id": tool.id,
                "tool_name": tool.name,
                "name": tool.name,
                "input_preview": tool.input_preview,
                "status": tool.status,
                "started_at": tool.started_at,
                "duration_ms": duration,
            }));
            let terminal = tool.status.eq_ignore_ascii_case("completed")
                || tool.status.eq_ignore_ascii_case("failed")
                || tool.status.eq_ignore_ascii_case("error");
            if tool.output.is_some() || terminal {
                blocks.push(json!({
                    "type": "tool_result",
                    "tool_call_id": tool.id,
                    "tool_name": tool.name,
                    "output_preview": tool.output.clone().unwrap_or_default(),
                    "status": tool.status,
                    "is_error": tool.status.to_ascii_lowercase().contains("fail")
                        || tool.status.to_ascii_lowercase().contains("error"),
                    "duration_ms": duration,
                }));
            }
        }
        if !self.text.is_empty() {
            blocks.push(json!({"type": "text", "text": self.text}));
        }
        blocks
    }
}

pub(super) async fn wait_for_turn(
    connection_id: &str,
    mut events: Box<dyn EventSubscription>,
    timeout_seconds: u64,
    fail_on_permission: bool,
) -> TurnOutcome {
    timeout(Duration::from_secs(timeout_seconds), async {
        let mut accumulator = TurnAccumulator::default();
        loop {
            let Some(event) = events.recv().await else {
                return finish(
                    accumulator,
                    false,
                    "protocol",
                    "event stream closed".to_string(),
                );
            };
            if event.connection_id != connection_id {
                continue;
            }
            accumulator.apply(&event);
            match event.kind.as_str() {
                "permission_request" if fail_on_permission => {
                    return finish(
                        accumulator,
                        false,
                        "permission",
                        "the prompt raised a permission request".to_string(),
                    );
                }
                "turn_complete" => {
                    if let Some(error) = event.payload.get("error").and_then(Value::as_str) {
                        return finish(accumulator, false, "protocol", error.to_string());
                    }
                    let stopped = event
                        .payload
                        .get("stop_reason")
                        .map(Value::to_string)
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if stopped.contains("cancel") {
                        return finish(
                            accumulator,
                            false,
                            "cancelled",
                            "the turn was cancelled".to_string(),
                        );
                    }
                    return finish(accumulator, true, "complete", String::new());
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| TurnOutcome {
        ok: false,
        phase: "timeout".to_string(),
        error: format!("the prompt did not finish within {timeout_seconds} seconds"),
        text: String::new(),
        blocks: Vec::new(),
        usage: Value::Null,
        artifacts: Vec::new(),
        events_seen: 0,
    })
}

fn finish(accumulator: TurnAccumulator, ok: bool, phase: &str, error: String) -> TurnOutcome {
    let text = accumulator.text.clone();
    let usage = accumulator.usage.clone();
    let artifacts = accumulator.artifacts.clone();
    let events_seen = accumulator.events_seen;
    TurnOutcome {
        ok,
        phase: phase.to_string(),
        error,
        text,
        blocks: accumulator.blocks(),
        usage,
        artifacts,
        events_seen,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn turn_result(
    conversations: &dyn ConversationRepository,
    outcome: TurnOutcome,
    model: Option<String>,
    elapsed: Duration,
    connection_id: Option<String>,
    conversation_id: Option<&str>,
    speaker_role_id: Option<&str>,
) -> Value {
    let TurnOutcome {
        ok,
        phase,
        error,
        text,
        mut blocks,
        usage,
        artifacts,
        events_seen,
    } = outcome;
    if !ok && !error.is_empty() {
        blocks.push(json!({"type": "turn_error", "message": error}));
    }
    if let Some(conversation_id) = conversation_id {
        let _ = conversations.append_turn_with_meta(
            conversation_id,
            "assistant",
            speaker_role_id,
            &json!(blocks),
            if usage.is_null() { None } else { Some(&usage) },
            Some(elapsed.as_millis() as u64),
            model.as_deref(),
        );
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

fn preview(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.chars().take(500).collect());
    }
    let text = serde_json::to_string(value).ok()?;
    Some(text.chars().take(500).collect())
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
