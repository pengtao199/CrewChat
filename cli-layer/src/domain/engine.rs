//! Engine settings owned by the app layer.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct EnginePreferences {
    pub enabled: bool,
    pub env: BTreeMap<String, String>,
}

impl Default for EnginePreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            env: BTreeMap::new(),
        }
    }
}
