//! Leader-only control MCP server.
//!
//! The core launches this binary as a stdio MCP server for the Leader ACP
//! session. It exposes exactly two tools and forwards them to the local
//! cli-layer HTTP control endpoint. It never talks to the user's MCP catalog.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Clone)]
struct ControlConfig {
    core_url: String,
    token: String,
    conversation_id: String,
    leader_role_id: String,
}

pub async fn run() -> Result<()> {
    let args = parse_args(std::env::args().skip(2));
    let core_url = args
        .get("core-url")
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:3081".to_string());
    let token = args
        .get("token")
        .cloned()
        .or_else(|| std::env::var("AGENTOS_CORE_TOKEN").ok())
        .ok_or_else(|| anyhow!("missing --token or AGENTOS_CORE_TOKEN"))?;
    let conversation_id = args
        .get("conversation")
        .cloned()
        .ok_or_else(|| anyhow!("missing --conversation"))?;
    let leader_role_id = args
        .get("leader")
        .cloned()
        .ok_or_else(|| anyhow!("missing --leader"))?;
    let config = ControlConfig {
        core_url: core_url.trim_end_matches('/').to_string(),
        token,
        conversation_id,
        leader_role_id,
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05"),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "agentos-team", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "This is the AgentOS team control channel. Call team_status to get the team members and their role_id values. Use delegate_to_agent for AgentOS members; Claude Code ListAgents is unrelated and will not show them.",
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => call_tool(&client, &config, &params).await,
            // Notifications (initialized, cancelled, ...) do not have an id.
            other => Err(anyhow!("method not found: {other}")),
        };
        let Some(id) = id else {
            continue;
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": error.to_string()},
            }),
        };
        stdout.write_all(response.to_string().as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

async fn call_tool(
    client: &reqwest::Client,
    config: &ControlConfig,
    params: &Value,
) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing tool name"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "delegate_to_agent" => {
            let member_role_id = args
                .get("roleId")
                .or_else(|| args.get("member_role_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("roleId is required"))?;
            let task = args
                .get("task")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("task is required"))?;
            let body = json!({
                "conversationId": config.conversation_id,
                "leaderRoleId": config.leader_role_id,
                "memberRoleId": member_role_id,
                "task": task,
                "context": args.get("context"),
                "expectedOutput": args.get("expectedOutput").or_else(|| args.get("expected_output")),
            });
            post(client, config, "/api/internal/team_delegate", body).await
        }
        "team_status" => {
            let body = json!({"conversationId": config.conversation_id});
            post(client, config, "/api/team_status", body).await
        }
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

async fn post(
    client: &reqwest::Client,
    config: &ControlConfig,
    path: &str,
    body: Value,
) -> Result<Value> {
    let response = client
        .post(format!("{}{path}", config.core_url))
        .bearer_auth(&config.token)
        .json(&body)
        .send()
        .await
        .map_err(|error| anyhow!("cli-layer request failed: {error}"))?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .await
        .unwrap_or_else(|_| json!({"error": "cli-layer returned a non-JSON response"}));
    if !status.is_success() {
        return Ok(tool_result(
            &format!("cli-layer {}: {}", status.as_u16(), value),
            true,
        ));
    }
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    Ok(tool_result(&text, false))
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    })
}

fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "delegate_to_agent",
            "description": "Delegate a bounded task to a AgentOS team member. Call team_status first to get roleId values. Only the Leader may call this. The member uses its own engine, model and skills; it does not see the full conversation. Do not use Claude Code ListAgents for AgentOS members.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "roleId": {"type": "string", "description": "Member role_id returned by team_status."},
                    "task": {"type": "string", "description": "What the member must do."},
                    "context": {"type": "string", "description": "Only the context the member needs."},
                    "expectedOutput": {"type": "string", "description": "What the member should return."}
                },
                "required": ["roleId", "task"]
            }
        }),
        json!({
            "name": "team_status",
            "description": "Read the AgentOS team roster (names + role_id), current member states, and delegation records. Use this before delegate_to_agent.",
            "inputSchema": {"type": "object", "properties": {}}
        }),
    ]
}

fn parse_args(args: impl Iterator<Item = String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if let Some(name) = arg.strip_prefix("--") {
            if let Some(value) = args.next() {
                out.insert(name.to_string(), value);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::tools;

    #[test]
    fn exposes_team_control_tools() {
        let tools = tools();
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert!(names.contains(&"delegate_to_agent"));
        assert!(names.contains(&"team_status"));
    }
}
