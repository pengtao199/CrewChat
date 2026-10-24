//! Pending ACP permission requests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

#[derive(Clone, Debug)]
pub enum PermissionDecision {
    Selected(String),
    Cancelled,
}

#[derive(Clone, Default)]
pub struct PermissionBroker {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>>,
}

impl PermissionBroker {
    pub(crate) fn open(&self) -> (String, oneshot::Receiver<PermissionDecision>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id.clone(), tx);
        (request_id, rx)
    }

    pub fn resolve(&self, request_id: &str, decision: PermissionDecision) -> bool {
        let sender = self.pending.lock().unwrap().remove(request_id);
        match sender {
            Some(sender) => sender.send(decision).is_ok(),
            None => false,
        }
    }
}

use agent_client_protocol::schema::v1::{PermissionOptionKind, RequestPermissionRequest};
use serde_json::{json, Value};

use crate::domain::permission::{match_tool_rule, PermissionAction, ToolRule};

pub(crate) struct AutoDecision {
    pub kind: &'static str,
    pub option_id: Option<String>,
    pub audit: Value,
}

pub(crate) fn auto_decision(
    request: &RequestPermissionRequest,
    rules: &[ToolRule],
    auto_approve: bool,
) -> Option<AutoDecision> {
    let tool = serde_json::to_value(&request.tool_call).ok()?;
    let name = tool
        .get("name")
        .or_else(|| tool.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let kind = tool.get("kind").and_then(Value::as_str).unwrap_or("");
    let paths = tool
        .get("locations")
        .and_then(Value::as_array)
        .map(|locations| {
            locations
                .iter()
                .filter_map(|location| location.get("path").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rule = match_tool_rule(rules, name, kind, &paths);
    let action = match rule {
        Some(rule) => rule.action().unwrap_or(PermissionAction::Deny),
        None if auto_approve => PermissionAction::Allow,
        None => return None,
    };
    let option_id = match action {
        PermissionAction::Allow => request
            .options
            .iter()
            .find(|option| matches!(option.kind, PermissionOptionKind::AllowOnce))
            .map(|option| option.option_id.0.to_string()),
        PermissionAction::Deny => request
            .options
            .iter()
            .find(|option| matches!(option.kind, PermissionOptionKind::RejectOnce))
            .map(|option| option.option_id.0.to_string()),
        PermissionAction::Ask => return None,
    };
    Some(AutoDecision {
        kind: if action == PermissionAction::Allow {
            "permission_auto_approved"
        } else {
            "permission_denied"
        },
        option_id,
        audit: json!({
            "tool": name,
            "kind": kind,
            "paths": paths,
            "action": match action {
                PermissionAction::Allow => "allow",
                PermissionAction::Ask => "ask",
                PermissionAction::Deny => "deny",
            },
            "rule": rule.map(|rule| serde_json::to_value(rule).unwrap_or(Value::Null)),
        }),
    })
}
