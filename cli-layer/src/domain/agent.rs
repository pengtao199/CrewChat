//! Typed Agent definition used by the app layer.
//!
//! The SQLite store still persists the legacy JSON shape; this type is the
//! boundary that will gain readiness, team snapshots and versioning without
//! leaking JSON keys into handlers.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Agent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub soul: String,
    #[serde(default)]
    pub rules: String,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub model_provider: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp: Vec<String>,
    #[serde(default)]
    pub permission_mode: String,
    #[serde(default)]
    pub auto_approve: bool,
    /// Roster revision, bumped on every save. Team snapshots store this so a
    /// running team can tell that its frozen member config has drifted.
    #[serde(default)]
    pub version: u64,
}

impl Agent {
    pub fn from_value(value: Value) -> Result<Self> {
        serde_json::from_value(value).context("invalid agent role")
    }

    pub fn to_value(&self) -> Result<Value> {
        serde_json::to_value(self).context("serialize agent role")
    }
}
