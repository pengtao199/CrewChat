//! Anthropic Messages ↔ OpenAI Chat Completions translation.
//!
//! This is the subset Claude Code actually exercises: text, images, tools,
//! tool results, tool_choice, streaming deltas, finish reasons and usage. The
//! Responses surface is pass-through for now; Claude talks Chat here because
//! every OpenAI-compatible provider we currently use accepts it.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Map, Value};

/// Translate one Anthropic `/v1/messages` body into a Chat Completions body.
pub fn anthropic_to_chat(body: &Value) -> Result<Value, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "request body must be a JSON object".to_string())?;

    let mut out = Map::new();
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    out.insert("model".into(), Value::String(model));
    if let Some(max_tokens) = object.get("max_tokens") {
        out.insert("max_tokens".into(), max_tokens.clone());
    }
    if let Some(stream) = object.get("stream") {
        out.insert("stream".into(), stream.clone());
        if stream.as_bool() == Some(true) {
            out.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }
    for key in ["temperature", "top_p"] {
        if let Some(value) = object.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    if let Some(stop) = object.get("stop_sequences") {
        out.insert("stop".into(), stop.clone());
    }

    let mut messages = Vec::new();
    if let Some(system) = object.get("system").and_then(system_text) {
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
    }
    if let Some(input) = object.get("messages").and_then(Value::as_array) {
        for message in input {
            translate_message(message, &mut messages)?;
        }
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        let tools: Vec<Value> = tools.iter().filter_map(translate_tool).collect();
        if !tools.is_empty() {
            out.insert("tools".into(), Value::Array(tools));
        }
    }
    if let Some(choice) = object.get("tool_choice").and_then(translate_tool_choice) {
        out.insert("tool_choice".into(), choice);
        if object
            .get("tool_choice")
            .and_then(|v| v.get("disable_parallel_tool_use"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            out.insert("parallel_tool_calls".into(), Value::Bool(false));
        }
    }

    Ok(Value::Object(out))
}

fn translate_message(message: &Value, out: &mut Vec<Value>) -> Result<(), String> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| "message.role is required".to_string())?;
    let content = message.get("content").unwrap_or(&Value::Null);

    match role {
        "assistant" => {
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            match content {
                Value::String(value) => text.push_str(value),
                Value::Array(blocks) => {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(value) = block.get("text").and_then(Value::as_str) {
                                    text.push_str(value);
                                }
                            }
                            Some("tool_use") => {
                                let id =
                                    block.get("id").and_then(Value::as_str).unwrap_or_default();
                                let name = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let input =
                                    block.get("input").cloned().unwrap_or_else(|| json!({}));
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": serde_json::to_string(&input)
                                            .unwrap_or_else(|_| "{}".to_string()),
                                    }
                                }));
                            }
                            // `thinking` / `redacted_thinking` are not part of
                            // the Chat wire format; drop them rather than
                            // corrupting the text.
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            let mut entry = Map::new();
            entry.insert("role".into(), Value::String("assistant".into()));
            if text.is_empty() {
                entry.insert("content".into(), Value::Null);
            } else {
                entry.insert("content".into(), Value::String(text));
            }
            if !tool_calls.is_empty() {
                entry.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            out.push(Value::Object(entry));
        }
        "user" => {
            let mut parts = Vec::new();
            let mut tool_messages = Vec::new();
            match content {
                Value::String(value) => parts.push(json!({"type": "text", "text": value})),
                Value::Array(blocks) => {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    if !text.is_empty() {
                                        parts.push(json!({"type": "text", "text": text}));
                                    }
                                }
                            }
                            Some("image") => {
                                if let Some(url) = image_url(block) {
                                    parts.push(json!({
                                        "type": "image_url",
                                        "image_url": {"url": url}
                                    }));
                                }
                            }
                            Some("tool_result") => {
                                let id = block
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let mut text = flatten_content(block.get("content"));
                                if block
                                    .get("is_error")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false)
                                {
                                    text = format!("[tool error] {text}");
                                }
                                tool_messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": text,
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            // OpenAI requires tool messages immediately after the assistant
            // message that declared the calls, so emit them before any user
            // text that shared the same Anthropic content block.
            out.extend(tool_messages);
            if !parts.is_empty() {
                let only_text = parts.len() == 1
                    && parts[0].get("type").and_then(Value::as_str) == Some("text");
                if only_text {
                    out.push(json!({
                        "role": "user",
                        "content": parts[0].get("text").cloned().unwrap_or(Value::Null)
                    }));
                } else {
                    out.push(json!({"role": "user", "content": parts}));
                }
            }
        }
        "system" => {
            let text = system_text(content).unwrap_or_default();
            if !text.is_empty() {
                out.push(json!({"role": "system", "content": text}));
            }
        }
        _ => {}
    }
    Ok(())
}

fn translate_tool(tool: &Value) -> Option<Value> {
    let name = tool.get("name").and_then(Value::as_str)?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parameters = tool
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    Some(json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    }))
}

fn translate_tool_choice(choice: &Value) -> Option<Value> {
    match choice.get("type").and_then(Value::as_str)? {
        "auto" => Some(Value::String("auto".into())),
        "any" => Some(Value::String("required".into())),
        "none" => Some(Value::String("none".into())),
        "tool" => choice
            .get("name")
            .and_then(Value::as_str)
            .map(|name| json!({"type": "function", "function": {"name": name}})),
        _ => None,
    }
}

fn system_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut out = Vec::new();
            for block in blocks {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !text.starts_with("x-anthropic-billing-header:") {
                        out.push(text.to_string());
                    }
                }
            }
            (!out.is_empty()).then(|| out.join("\n\n"))
        }
        _ => None,
    }
}

fn flatten_content(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        block
                            .get("type")
                            .and_then(Value::as_str)
                            .filter(|kind| *kind == "image")
                            .map(|_| "[image]".to_string())
                    })
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn image_url(block: &Value) -> Option<String> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str)? {
        "base64" => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            Some(format!("data:{media_type};base64,{data}"))
        }
        "url" => source
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Translate a non-streaming Chat response into an Anthropic message.
pub fn chat_to_anthropic(body: &Value, requested_model: &str) -> Value {
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());
    let message = choice.and_then(|choice| choice.get("message"));
    let finish = choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str);

    let mut content = Vec::new();
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    if let Some(tool_calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for (index, call) in tool_calls.iter().enumerate() {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("toolu_{index}"));
            let name = call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let arguments = call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    }

    let usage = body.get("usage").cloned().unwrap_or_else(|| json!({}));
    json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": requested_model,
        "content": content,
        "stop_reason": stop_reason(finish),
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": usage.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
            "output_tokens": usage.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
        }
    })
}

fn stop_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("content_filter") => "end_turn",
        _ => "end_turn",
    }
}

/// Rough input-token estimate for `/v1/messages/count_tokens`. Claude Code only
/// uses this for its context meter; a bad estimate is better than blocking the
/// session, and the upstream usage replaces it once a turn completes.
pub fn estimate_tokens(body: &Value) -> i64 {
    let chars = body.to_string().chars().count() as i64;
    (chars / 4).max(1)
}

pub fn error_body(kind: &str, message: impl Into<String>) -> Value {
    json!({
        "type": "error",
        "error": {
            "type": kind,
            "message": message.into(),
        }
    })
}

// ---------------------------------------------------------------- streaming

struct ToolBlock {
    block_index: Option<usize>,
    id: String,
    name: String,
    pending: String,
}

struct SseState {
    requested_model: String,
    started: bool,
    text_index: Option<usize>,
    next_index: usize,
    tools: BTreeMap<usize, ToolBlock>,
    stop_reason: Option<&'static str>,
    input_tokens: i64,
    output_tokens: i64,
    finished: bool,
}

impl SseState {
    fn new(requested_model: String) -> Self {
        Self {
            requested_model,
            started: false,
            text_index: None,
            next_index: 0,
            tools: BTreeMap::new(),
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            finished: false,
        }
    }

    fn handle(&mut self, value: &Value) -> Vec<Bytes> {
        let mut events = Vec::new();
        if !self.started {
            events.push(sse(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                        "type": "message",
                        "role": "assistant",
                        "model": self.requested_model,
                        "content": [],
                        "stop_reason": Value::Null,
                        "stop_sequence": Value::Null,
                        "usage": {"input_tokens": self.input_tokens, "output_tokens": 0}
                    }
                }),
            ));
            self.started = true;
        }

        if let Some(usage) = value.get("usage") {
            self.input_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(self.input_tokens);
            self.output_tokens = usage
                .get("completion_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(self.output_tokens);
        }

        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return events;
        };

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    self.text_delta(text, &mut events);
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let arguments = call
                        .get("function")
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.tool_delta(index, id, name, arguments, &mut events);
                }
            }
        }

        if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(stop_reason(Some(finish)));
        }

        events
    }

    fn text_delta(&mut self, text: &str, events: &mut Vec<Bytes>) {
        if self.text_index.is_none() {
            let index = self.next_index;
            self.next_index += 1;
            self.text_index = Some(index);
            events.push(sse(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {"type": "text", "text": ""}
                }),
            ));
        }
        events.push(sse(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": self.text_index.unwrap(),
                "delta": {"type": "text_delta", "text": text}
            }),
        ));
    }

    fn close_text(&mut self, events: &mut Vec<Bytes>) {
        if let Some(index) = self.text_index.take() {
            events.push(sse(
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": index
                }),
            ));
        }
    }

    fn tool_delta(
        &mut self,
        index: usize,
        id: &str,
        name: &str,
        arguments: &str,
        events: &mut Vec<Bytes>,
    ) {
        {
            let tool = self.tools.entry(index).or_insert_with(|| ToolBlock {
                block_index: None,
                id: String::new(),
                name: String::new(),
                pending: String::new(),
            });
            if !id.is_empty() {
                tool.id = id.to_string();
            }
            if !name.is_empty() {
                tool.name = name.to_string();
            }
            if let Some(block_index) = tool.block_index {
                if !arguments.is_empty() {
                    events.push(sse(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": block_index,
                            "delta": {"type": "input_json_delta", "partial_json": arguments}
                        }),
                    ));
                }
                return;
            }
            if !arguments.is_empty() {
                tool.pending.push_str(arguments);
            }
            if tool.name.is_empty() {
                // Anthropic needs the tool name at block start; wait for the
                // chunk that carries it and keep the argument chunks buffered.
                return;
            }
        }

        let (name, id, pending) = {
            let tool = self.tools.get(&index).expect("tool was just inserted");
            (tool.name.clone(), tool.id.clone(), tool.pending.clone())
        };
        self.close_text(events);
        let block_index = self.next_index;
        self.next_index += 1;
        let id = if id.is_empty() {
            format!("toolu_{block_index}")
        } else {
            id
        };
        if let Some(tool) = self.tools.get_mut(&index) {
            tool.block_index = Some(block_index);
            tool.pending.clear();
        }
        events.push(sse(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": {}
                }
            }),
        ));
        if !pending.is_empty() {
            events.push(sse(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {"type": "input_json_delta", "partial_json": pending}
                }),
            ));
        }
    }

    fn finish(&mut self) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = Vec::new();
        if !self.started {
            events.push(sse(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                        "type": "message",
                        "role": "assistant",
                        "model": self.requested_model,
                        "content": [],
                        "stop_reason": Value::Null,
                        "stop_sequence": Value::Null,
                        "usage": {"input_tokens": self.input_tokens, "output_tokens": 0}
                    }
                }),
            ));
            self.started = true;
        }
        self.close_text(&mut events);
        let indices: Vec<usize> = self.tools.keys().copied().collect();
        for index in indices {
            if let Some(tool) = self.tools.get_mut(&index) {
                if tool.block_index.is_none() && !tool.name.is_empty() {
                    let block_index = self.next_index;
                    self.next_index += 1;
                    let id = if tool.id.is_empty() {
                        format!("toolu_{block_index}")
                    } else {
                        tool.id.clone()
                    };
                    tool.block_index = Some(block_index);
                    events.push(sse(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": block_index,
                            "content_block": {
                                "type": "tool_use",
                                "id": id,
                                "name": tool.name,
                                "input": {}
                            }
                        }),
                    ));
                    if !tool.pending.is_empty() {
                        let pending = std::mem::take(&mut tool.pending);
                        events.push(sse(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": block_index,
                                "delta": {"type": "input_json_delta", "partial_json": pending}
                            }),
                        ));
                    }
                }
                if let Some(block_index) = tool.block_index {
                    events.push(sse(
                        "content_block_stop",
                        json!({
                            "type": "content_block_stop",
                            "index": block_index
                        }),
                    ));
                }
            }
        }
        events.push(sse(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": self.stop_reason.unwrap_or("end_turn"),
                    "stop_sequence": Value::Null
                },
                "usage": {"output_tokens": self.output_tokens}
            }),
        ));
        events.push(sse("message_stop", json!({"type": "message_stop"})));
        events
    }
}

pub fn sse(event: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

/// Translate one upstream Chat SSE stream into Anthropic SSE.
pub fn chat_stream(
    mut response: reqwest::Response,
    requested_model: String,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut state = SseState::new(requested_model);
        let mut buffer: Vec<u8> = Vec::new();
        let _ = &mut buffer;
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(error) => {
                    yield Ok(sse("error", error_body("api_error", error.to_string())));
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some((position, delimiter)) = find_event_boundary(&buffer) {
                let event = buffer.drain(..position).collect::<Vec<u8>>();
                let _ = buffer.drain(..delimiter);
                let text = String::from_utf8_lossy(&event);
                for line in text.lines() {
                    let Some(data) = line.trim().strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        for event in state.finish() {
                            yield Ok(event);
                        }
                        return;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    for event in state.handle(&value) {
                        yield Ok(event);
                    }
                }
            }
        }
        for event in state.finish() {
            yield Ok(event);
        }
    }
}

/// Find the end of one SSE event. Accepts both LF and CRLF framing.
fn find_event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    if let Some(position) = buffer.windows(2).position(|window| window == b"\n\n") {
        return Some((position, 2));
    }
    if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some((position, 4));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Value {
        json!({
            "model": "Provider/model",
            "max_tokens": 1024,
            "system": [{"type": "text", "text": "be terse"}],
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "t1", "name": "read", "input": {"path": "a"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
                    {"type": "text", "text": "continue"}
                ]}
            ],
            "tools": [{"name": "read", "description": "read a file", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto"}
        })
    }

    #[test]
    fn maps_system_tools_and_tool_results() {
        let chat = anthropic_to_chat(&request()).unwrap();
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(
            chat["messages"][2]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(chat["messages"][3]["role"], "tool");
        assert_eq!(chat["messages"][3]["tool_call_id"], "t1");
        assert_eq!(chat["messages"][4]["role"], "user");
        assert_eq!(chat["tools"][0]["function"]["parameters"]["type"], "object");
        assert_eq!(chat["tool_choice"], "auto");
    }

    #[test]
    fn maps_stop_reason_and_tool_calls_back() {
        let response = json!({
            "choices": [{
                "message": {
                    "content": "done",
                    "tool_calls": [{
                        "id": "t1",
                        "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        });
        let message = chat_to_anthropic(&response, "Provider/model");
        assert_eq!(message["stop_reason"], "tool_use");
        assert_eq!(message["usage"]["input_tokens"], 10);
        assert_eq!(message["content"][1]["input"]["path"], "a");
    }

    #[test]
    fn stream_state_emits_text_then_tool() {
        let mut state = SseState::new("Provider/model".into());
        let mut events = state.handle(&json!({
            "choices": [{"delta": {"content": "hi"}}]
        }));
        events.extend(state.handle(&json!({
            "choices": [{"delta": {"tool_calls": [{"index": 0, "id": "t1", "function": {"name": "read", "arguments": "{\"a\":1}"}}]}}]
        })));
        events.extend(state.finish());
        let body: String = events
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).to_string())
            .collect();
        assert!(body.contains("text_delta"));
        assert!(body.contains("input_json_delta"));
        assert!(body.contains("message_stop"));
        assert!(body.contains("stop_reason"));
    }

    #[test]
    fn stream_buffers_arguments_until_the_name_arrives() {
        let mut state = SseState::new("m".into());
        let mut events = state.handle(&json!({
            "choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"a\""}}]}}]
        }));
        let first = events
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).to_string())
            .collect::<String>();
        assert!(!first.contains("tool_use"));
        events.extend(state.handle(&json!({
            "choices": [{"delta": {"tool_calls": [{"index": 0, "id": "t1", "function": {"name": "read", "arguments": ":1}"}}]}}]
        })));
        let body: String = events
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).to_string())
            .collect();
        assert!(body.contains("tool_use"));
        assert!(body.contains("partial_json"));
    }
}
