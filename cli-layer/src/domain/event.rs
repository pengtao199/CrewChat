//! Event envelope shared by runtime, app and API layers.

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize)]
pub struct EventEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub connection_id: String,
    pub seq: u64,
    #[serde(flatten)]
    pub payload: Value,
}
