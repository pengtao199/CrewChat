//! One-time agent preset installation.
//!
//! Existing rosters are matched by display name so a migration does not break
//! conversation history through legacy ids. Once a row owns a `preset_key`,
//! user edits are never overwritten.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::domain::agent::Agent;
use crate::domain::ports::AgentRepository;

#[derive(Debug, Deserialize)]
struct Preset {
    preset_key: String,
    id: String,
    name: String,
    soul: String,
    rules: String,
}

pub fn ensure(repo: &dyn AgentRepository) -> Result<usize> {
    let presets: Vec<Preset> = serde_json::from_str(include_str!("../../presets/agents.json"))
        .context("parse embedded agent presets")?;
    let existing = repo.list()?;
    let mut changed = 0usize;

    if existing.is_empty() {
        for preset in presets {
            repo.save(&Agent {
                id: preset.id,
                name: preset.name,
                soul: preset.soul,
                rules: preset.rules,
                preset_key: Some(preset.preset_key),
                ..Agent::default()
            })?;
            changed += 1;
        }
        return Ok(changed);
    }

    for preset in presets {
        if existing
            .iter()
            .any(|agent| agent.preset_key.as_deref() == Some(preset.preset_key.as_str()))
        {
            continue;
        }
        let Some(mut agent) = existing
            .iter()
            .find(|agent| agent.name == preset.name)
            .cloned()
        else {
            continue;
        };
        agent.name = preset.name;
        agent.soul = preset.soul;
        agent.rules = preset.rules;
        agent.preset_key = Some(preset.preset_key.clone());
        if matches!(
            preset.preset_key.as_str(),
            "agentos_helper" | "agentos_manager"
        ) {
            agent.skills.clear();
            agent.mcp.clear();
            agent.auto_approve = false;
        }
        repo.save(&agent)?;
        changed += 1;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ensure;
    use crate::infra::store::Store;

    #[test]
    fn migrates_existing_names_once_and_preserves_engine() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("presets.db")).unwrap();
        store.init_schema().unwrap();
        store
            .save_role(&json!({
                "id": "legacy-help",
                "name": "Agent 助手",
                "agent_type": "codex",
                "model": "chosen-by-user",
                "soul": "old product",
                "auto_approve": true,
                "skills": ["old-skill"]
            }))
            .unwrap();
        store
            .save_role(&json!({
                "id": "legacy-manager",
                "name": "管理员",
                "agent_type": "claude_code",
                "soul": "old product"
            }))
            .unwrap();

        assert_eq!(ensure(&store).unwrap(), 2);
        let agents = store.list_roles().unwrap();
        let helper = agents.iter().find(|r| r["name"] == "Agent 助手").unwrap();
        assert_eq!(helper["preset_key"], "agentos_helper");
        assert_eq!(helper["agent_type"], "codex");
        assert_eq!(helper["model"], "chosen-by-user");
        assert_eq!(helper["skills"], json!([]));
        assert_eq!(helper["auto_approve"], false);
        assert_eq!(ensure(&store).unwrap(), 0);
    }

    #[test]
    fn seeds_all_presets_for_a_fresh_roster() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("fresh.db")).unwrap();
        store.init_schema().unwrap();
        assert_eq!(ensure(&store).unwrap(), 8);
        let agents = store.list_roles().unwrap();
        assert!(agents.iter().any(|r| r["preset_key"] == "agentos_manager"));
        assert!(agents.iter().any(|r| r["name"] == "文档维护专家"));
        assert!(agents
            .iter()
            .all(|r| !r["soul"].as_str().unwrap().contains("old product")));
    }
}
