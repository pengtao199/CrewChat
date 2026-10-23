//! Skill library and Agent skill-binding use cases.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::app::roster::RosterService;
use crate::infra::skill_layer::SkillLayerClient;

#[derive(Clone)]
pub struct SkillService {
    roster: RosterService,
    skill: Arc<SkillLayerClient>,
}

impl SkillService {
    pub fn new(roster: RosterService, skill: Arc<SkillLayerClient>) -> Self {
        Self { roster, skill }
    }

    pub async fn list(&self) -> Result<Value> {
        self.skill.list().await.map_err(anyhow::Error::msg)
    }

    pub async fn catalog(&self) -> Result<Value> {
        let skills = self.list().await?;
        let collections = self.skill.collections().await.map_err(anyhow::Error::msg)?;
        let links = self
            .skill
            .global_links()
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(json!({
            "skills": skills.get("skills").cloned().unwrap_or_else(|| json!([])),
            "collections": collections.get("collections").cloned().unwrap_or_else(|| json!([])),
            "globalLinks": links.get("links").cloned().unwrap_or_else(|| json!([])),
        }))
    }

    pub async fn read(&self, skill_id: &str) -> Result<Value> {
        self.skill
            .detail(skill_id)
            .await
            .map_err(anyhow::Error::msg)
    }

    pub async fn install(&self, body: &Value) -> Result<Value> {
        self.skill.install(body).await.map_err(anyhow::Error::msg)
    }

    pub async fn check(&self, skill_id: &str) -> Result<Value> {
        self.skill.check(skill_id).await.map_err(anyhow::Error::msg)
    }

    pub async fn update(&self, skill_id: &str) -> Result<Value> {
        self.skill
            .update(skill_id)
            .await
            .map_err(anyhow::Error::msg)
    }

    pub async fn remove(&self, skill_id: &str) -> Result<Value> {
        self.skill
            .remove(skill_id)
            .await
            .map_err(anyhow::Error::msg)
    }

    pub async fn link_status(&self, agent_type: Option<&str>) -> Result<Value> {
        let value = self
            .skill
            .global_links()
            .await
            .map_err(anyhow::Error::msg)?;
        let links = value
            .get("links")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|link| {
                agent_type.is_none_or(|agent| {
                    link.get("agentType")
                        .or_else(|| link.get("agent_type"))
                        .and_then(Value::as_str)
                        == Some(agent)
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "links": links }))
    }

    pub async fn set_agent_skills(
        &self,
        role_id: &str,
        mut skill_ids: Vec<String>,
        collection_ids: Vec<String>,
    ) -> Result<Value> {
        let mut role = self
            .roster
            .get(role_id)?
            .ok_or_else(|| anyhow!("role not found"))?;

        if !collection_ids.is_empty() {
            let value = self.skill.collections().await.map_err(anyhow::Error::msg)?;
            if let Some(collections) = value.get("collections").and_then(Value::as_array) {
                for collection in collections {
                    let id = collection
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !collection_ids.iter().any(|wanted| wanted == id) {
                        continue;
                    }
                    skill_ids.extend(string_array(
                        collection
                            .get("skillIds")
                            .or_else(|| collection.get("skill_ids")),
                    ));
                }
            }
        }

        skill_ids.sort();
        skill_ids.dedup();
        role.skills = skill_ids;
        let saved = self.roster.save(&role)?;
        Ok(json!({
            "roleId": saved.id,
            "skillIds": saved.skills,
        }))
    }

    pub async fn agent_status(&self, role_id: &str) -> Result<Value> {
        let role = self
            .roster
            .get(role_id)?
            .ok_or_else(|| anyhow!("role not found"))?;
        let available = self
            .skill
            .list()
            .await
            .ok()
            .and_then(|value| value.get("skills").and_then(Value::as_array).cloned())
            .unwrap_or_default();

        let mut rows = Vec::new();
        let mut missing = Vec::new();
        for id in role.skills {
            let found = available
                .iter()
                .find(|skill| skill.get("id").and_then(Value::as_str) == Some(id.as_str()));
            if found.is_none() {
                missing.push(id.clone());
            }
            rows.push(json!({
                "id": id,
                "name": found
                    .and_then(|skill| skill.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(&id),
                "available": found.is_some(),
            }));
        }
        Ok(json!({"skills": rows, "missing": missing}))
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(id) => Some(id.clone()),
                    Value::Object(object) => {
                        object.get("id").and_then(Value::as_str).map(str::to_string)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
