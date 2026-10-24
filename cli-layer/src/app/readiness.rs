//! Agent readiness checks used before starting a session or creating a team.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::app::engines::EngineService;
use crate::app::roster::RosterService;
use crate::domain::agent::Agent;
use crate::domain::permission::resolve_mode;
use crate::domain::ports::{ModelCatalog, SkillCatalog};

#[derive(Clone)]
pub struct ReadinessService {
    roster: RosterService,
    engines: EngineService,
    model: Arc<dyn ModelCatalog>,
    skills: Arc<dyn SkillCatalog>,
}

impl ReadinessService {
    pub fn new(
        roster: RosterService,
        engines: EngineService,
        model: Arc<dyn ModelCatalog>,
        skills: Arc<dyn SkillCatalog>,
    ) -> Self {
        Self {
            roster,
            engines,
            model,
            skills,
        }
    }

    pub async fn check(&self, role_id: &str) -> Result<Value> {
        let agent = self
            .roster
            .get(role_id)?
            .ok_or_else(|| anyhow!("role not found"))?;
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();

        self.check_engine(&agent, &mut blockers, &mut warnings);
        self.check_model(&agent, &mut blockers, &mut warnings).await;
        self.check_skills(&agent, &mut blockers).await;
        self.check_permissions(&agent, &mut blockers, &mut warnings);

        if !agent.mcp.is_empty() {
            warnings.push(issue(
                "mcp_deferred",
                "MCP servers are not wired into sessions yet.",
                "Keep the selection; MCP support lands after the readiness phase.",
            ));
        }

        Ok(json!({
            "ready": blockers.is_empty(),
            "blockers": blockers,
            "warnings": warnings,
            "resolved": {
                "engine": agent.agent_type,
                "model": model_label(&agent),
                "skills": agent.skills,
                "mcp": agent.mcp,
                "permissionMode": agent.permission_mode,
                "effectivePermissionMode": resolve_mode(&agent.agent_type, &agent.permission_mode)
                    .ok()
                    .flatten(),
                "autoApprove": agent.auto_approve,
            },
        }))
    }

    fn check_engine(&self, agent: &Agent, blockers: &mut Vec<Value>, warnings: &mut Vec<Value>) {
        let Some(engine) = self.engines.list().into_iter().find(|engine| {
            engine.get("agent_type").and_then(Value::as_str) == Some(&agent.agent_type)
        }) else {
            blockers.push(issue(
                "engine_unknown",
                &format!("Unknown engine '{}'.", agent.agent_type),
                "Pick an engine from the Agent editor.",
            ));
            return;
        };

        if engine.get("enabled").and_then(Value::as_bool) == Some(false) {
            blockers.push(issue(
                "engine_disabled",
                &format!(
                    "{} is disabled.",
                    engine
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&agent.agent_type)
                ),
                "Enable the engine in Settings → Engines.",
            ));
        }

        if engine.get("available").and_then(Value::as_bool) != Some(true) {
            blockers.push(issue(
                "engine_unavailable",
                &format!(
                    "{} is not installed or not on PATH.",
                    engine
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&agent.agent_type)
                ),
                "Install the adapter from Settings → Engines, or install its CLI.",
            ));
        }

        if engine.get("installed_version").is_none_or(Value::is_null) {
            warnings.push(issue(
                "engine_version_unverified",
                "The adapter version could not be read.",
                "Run engine diagnostics if the session fails to start.",
            ));
        }
    }

    async fn check_model(
        &self,
        agent: &Agent,
        blockers: &mut Vec<Value>,
        warnings: &mut Vec<Value>,
    ) {
        if agent.model.trim().is_empty() {
            warnings.push(issue(
                "model_not_configured",
                "No model is configured; the CLI default model will be used.",
                "Choose a model in the Agent editor if you need per-role routing.",
            ));
            return;
        }

        if !self.engines.supports_model_injection(&agent.agent_type) {
            warnings.push(issue(
                "model_fallback",
                "This engine does not support per-role model injection; it will use its own default model.",
                "Choose an engine with model injection if the model must be enforced.",
            ));
            return;
        }

        match self.model.model_face(&agent.agent_type).await {
            Ok(face) => {
                let model = agent.model.trim();
                let provider = agent.model_provider.trim();
                let found = face
                    .get("models")
                    .and_then(Value::as_array)
                    .is_some_and(|models| {
                        models.iter().any(|entry| {
                            let model_matches = entry
                                .get("model")
                                .and_then(Value::as_str)
                                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(model));
                            let provider_matches = provider.is_empty()
                                || entry.get("provider").and_then(Value::as_str).is_some_and(
                                    |candidate| candidate.eq_ignore_ascii_case(provider),
                                )
                                || entry
                                    .get("provider_id")
                                    .and_then(Value::as_str)
                                    .is_some_and(|candidate| candidate == provider);
                            model_matches && provider_matches
                        })
                    });
                if !found {
                    warnings.push(issue(
                        "model_not_listed",
                        &format!("Model '{model}' is not in the current model catalog."),
                        "Check the provider configuration, or keep it if this is a custom model.",
                    ));
                }
            }
            Err(error) => blockers.push(issue(
                "model_catalog_unavailable",
                &format!("The model layer could not be reached: {error}"),
                "Start the model layer and retry readiness.",
            )),
        }
    }

    async fn check_skills(&self, agent: &Agent, blockers: &mut Vec<Value>) {
        if agent.skills.is_empty() {
            return;
        }
        let available = match self.skills.list().await {
            Ok(value) => value
                .get("skills")
                .and_then(Value::as_array)
                .map(|skills| {
                    skills
                        .iter()
                        .filter_map(|skill| skill.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default(),
            Err(error) => {
                blockers.push(issue(
                    "skill_catalog_unavailable",
                    &format!("The skill layer could not be reached: {error}"),
                    "Start the skill layer and retry readiness.",
                ));
                return;
            }
        };

        for id in &agent.skills {
            if !available.contains(id) {
                blockers.push(issue(
                    "skill_missing",
                    &format!("Skill '{id}' is not installed."),
                    "Install the skill or remove it from this Agent.",
                ));
            }
        }
    }

    fn check_permissions(
        &self,
        agent: &Agent,
        blockers: &mut Vec<Value>,
        warnings: &mut Vec<Value>,
    ) {
        if let Err(message) = resolve_mode(&agent.agent_type, &agent.permission_mode) {
            blockers.push(issue(
                "permission_mode_unsupported",
                &message,
                "Choose a mode this engine supports in the Agent editor.",
            ));
        }
        if let Some(timeout) = agent.limits.timeout_seconds {
            if !(1..=3600).contains(&timeout) {
                blockers.push(issue(
                    "limit_timeout_invalid",
                    "Agent timeout must be between 1 and 3600 seconds.",
                    "Update the Agent limits.",
                ));
            }
        }
        if let Some(max_turns) = agent.limits.max_turns {
            if max_turns == 0 {
                blockers.push(issue(
                    "limit_max_turns_invalid",
                    "Agent max_turns must be greater than zero.",
                    "Update the Agent limits.",
                ));
            }
        }
        for (index, rule) in agent.tool_rules.iter().enumerate() {
            if rule.action().is_none() {
                blockers.push(issue(
                    "tool_rule_invalid",
                    &format!(
                        "Tool rule #{} has an invalid action '{}'.",
                        index + 1,
                        rule.action
                    ),
                    "Use allow, ask, or deny.",
                ));
            }
        }
        if agent.auto_approve && agent.permission_mode.trim().is_empty() {
            warnings.push(issue(
                "auto_approve_overrides_mode",
                "Auto-approve is on while the engine uses its default permission mode.",
                "Pick an explicit mode or turn auto-approve off if you need a clear policy.",
            ));
        }
    }
}

fn model_label(agent: &Agent) -> Option<String> {
    if agent.model.trim().is_empty() {
        return None;
    }
    if agent.model_provider.trim().is_empty() {
        return Some(agent.model.clone());
    }
    Some(format!("{} / {}", agent.model_provider, agent.model))
}

fn issue(code: &str, message: &str, fix: &str) -> Value {
    json!({"code": code, "message": message, "fix": fix})
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{json, Value};

    use super::ReadinessService;
    use crate::app::engines::EngineService;
    use crate::app::roster::RosterService;
    use crate::config::Config;
    use crate::domain::agent::Agent;
    use crate::domain::engine::EnginePreferences;
    use crate::domain::ports::{
        AgentRepository, EngineSettingsRepository, ModelCatalog, SkillCatalog,
    };
    use crate::runtime::engine::registry::Engines;

    struct FakeAgents(Agent);

    impl AgentRepository for FakeAgents {
        fn list(&self) -> anyhow::Result<Vec<Agent>> {
            Ok(vec![self.0.clone()])
        }

        fn get(&self, id: &str) -> anyhow::Result<Option<Agent>> {
            Ok((self.0.id == id).then(|| self.0.clone()))
        }

        fn save(&self, agent: &Agent) -> anyhow::Result<Agent> {
            Ok(agent.clone())
        }

        fn delete(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FakeModel;

    #[async_trait]
    impl ModelCatalog for FakeModel {
        fn health(&self) -> Value {
            json!({})
        }

        async fn rpc(&self, _method: &str, _args: Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn provider_save(&self, _body: &Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn provider_delete(&self, _body: &Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn provider_probe(&self, _body: &Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn disabled_models(&self) -> Result<Value, String> {
            Ok(json!([]))
        }

        async fn model_face(&self, _agent_type: &str) -> Result<Value, String> {
            Ok(json!({
                "supported": true,
                "models": [{"provider": "OpenAI", "provider_id": "openai", "model": "gpt-5"}],
            }))
        }

        async fn normalize_provider(&self, _provider: &str, _model: &str) -> Option<String> {
            Some("OpenAI".to_string())
        }

        async fn local_gateway_key(&self) -> Option<String> {
            None
        }
    }

    struct FakeSkills;

    #[async_trait]
    impl SkillCatalog for FakeSkills {
        async fn list(&self) -> Result<Value, String> {
            Ok(json!({"skills": [{"id": "s1", "name": "Swift"}]}))
        }

        async fn detail(&self, _id: &str) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn install(&self, _body: &Value) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn check(&self, _id: &str) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn update(&self, _id: &str) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn remove(&self, _id: &str) -> Result<Value, String> {
            Ok(json!({}))
        }

        async fn collections(&self) -> Result<Value, String> {
            Ok(json!({"collections": []}))
        }

        async fn global_links(&self) -> Result<Value, String> {
            Ok(json!({"links": []}))
        }

        async fn materialize(
            &self,
            _home: &Path,
            _agent_type: &str,
            _skill_ids: &[String],
        ) -> Result<Value, String> {
            Ok(json!({}))
        }
    }

    struct FakeSettings;

    impl EngineSettingsRepository for FakeSettings {
        fn engine_preferences(&self, _agent_type: &str) -> anyhow::Result<EnginePreferences> {
            Ok(EnginePreferences::default())
        }

        fn save_engine_preferences(
            &self,
            _agent_type: &str,
            _enabled: bool,
            _env: &BTreeMap<String, String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn config() -> Arc<Config> {
        Arc::new(Config {
            data_dir: PathBuf::from("/tmp"),
            host: "127.0.0.1".to_string(),
            port: 0,
            token: "test".to_string(),
            model_layer_url: "http://127.0.0.1:3458".to_string(),
            model_layer_gateway: "http://127.0.0.1:3456".to_string(),
            model_layer_token: None,
            claude_command: "python3".to_string(),
            codex_command: "python3".to_string(),
            opencode_command: "python3".to_string(),
        })
    }

    #[tokio::test]
    async fn reports_missing_skill_and_unknown_permission() {
        let config = config();
        let engines = Engines::new(config.clone());
        let model = Arc::new(FakeModel);
        let roster = RosterService::new(
            Arc::new(FakeAgents(Agent {
                id: "role-1".to_string(),
                name: "Reviewer".to_string(),
                agent_type: "claude_code".to_string(),
                model: "gpt-5".to_string(),
                model_provider: "openai".to_string(),
                skills: vec!["s1".to_string(), "missing".to_string()],
                permission_mode: "turbo".to_string(),
                ..Agent::default()
            })),
            model.clone(),
            engines.clone(),
        );
        let engines_service = EngineService::new(Arc::new(FakeSettings), engines, config);
        let readiness = ReadinessService::new(roster, engines_service, model, Arc::new(FakeSkills));

        let value = readiness.check("role-1").await.unwrap();

        assert_eq!(value["ready"], false);
        assert!(value["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "skill_missing"));
        assert!(value["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "permission_mode_unsupported"));
    }
}
