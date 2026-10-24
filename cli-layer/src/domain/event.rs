//! Event envelope shared by runtime, app and API layers.

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize)]
pub struct EventEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub connection_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_role_id: Option<String>,
    pub seq: u64,
    #[serde(flatten)]
    pub payload: Value,
}
