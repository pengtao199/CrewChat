use crate::config::Config;
use crate::registry::{Collection, GlobalLink, Registry, SkillRow, SkillSummary, VersionRow};
use crate::skill::{
    collect_files, copy_tree, has_script_files, hash_tree, list_files, now_rfc3339, parse_skill,
    slugify, FileInfo,
};
use crate::source;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRef {
    pub id: String,
    #[serde(default)]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializedLink {
    pub id: String,
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Materialized {
    pub home: String,
    pub skills_dir: String,
    pub links: Vec<MaterializedLink>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDetail {
    pub skill: SkillSummary,
    pub content: String,
    pub files: Vec<FileInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub id: String,
    pub current_hash: String,
    pub latest_hash: String,
    pub update_available: bool,
}

#[derive(Debug)]
pub enum MaterializeError {
    Missing(Vec<String>),
    Message(String),
}

impl fmt::Display for MaterializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(ids) => write!(f, "skills not installed: {}", ids.join(", ")),
            Self::Message(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for MaterializeError {}

pub struct Store {
    pub config: Config,
    registry: Registry,
}

impl Store {
    pub fn open(config: Config) -> Result<Self> {
        let registry = Registry::open(&config.registry_path)?;
        Ok(Self { config, registry })
    }

    pub fn list(&self) -> Result<Vec<SkillSummary>> {
        self.registry.list()
    }

    pub fn get(&self, id: &str) -> Result<Option<SkillDetail>> {
        let Some(skill) = self.registry.get(id)? else {
            return Ok(None);
        };
        let version = self
            .config
            .store_dir
            .join(&skill.id)
            .join(&skill.current_hash);
        let content = fs::read_to_string(version.join("SKILL.md"))
            .with_context(|| format!("read SKILL.md for {}", skill.id))?;
        let files = list_files(&version)?;
        Ok(Some(SkillDetail {
            skill,
            content,
            files,
        }))
    }

    pub fn install_local(&mut self, source: &Path) -> Result<SkillSummary> {
        let path = source
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("skill path is not valid UTF-8"))?;
        self.install_source(Some(path), None, None, None)
    }

    pub fn install_source(
        &mut self,
        path: Option<&str>,
        source: Option<&str>,
        git_ref: Option<&str>,
        subdir: Option<&str>,
    ) -> Result<SkillSummary> {
        let prepared = source::prepare(path, source, git_ref, subdir)?;
        self.install_prepared(&prepared)
    }

    fn install_prepared(&mut self, prepared: &source::PreparedSource) -> Result<SkillSummary> {
        let header = parse_skill(&prepared.path)?;
        let id = slugify(&header.name)?;
        let hash = hash_tree(&prepared.path)?;
        let version = self.config.store_dir.join(&id).join(&hash);

        if !version.exists() {
            let parent = self.config.store_dir.join(&id);
            fs::create_dir_all(&parent)?;
            let temp = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
            if let Err(error) = copy_tree(&prepared.path, &temp) {
                let _ = fs::remove_dir_all(&temp);
                return Err(error);
            }
            match fs::rename(&temp, &version) {
                Ok(()) => {}
                Err(error) => {
                    let _ = fs::remove_dir_all(&temp);
                    if !version.exists() {
                        return Err(error).with_context(|| {
                            format!("move skill {} into the version store", header.name)
                        });
                    }
                }
            }
        }

        let files = collect_files(&version)?;
        let size_bytes: u64 = files.iter().map(|(_, size)| *size).sum();
        let has_scripts = has_script_files(&version)?;
        let now = now_rfc3339();

        let row = SkillRow {
            id: id.clone(),
            name: header.name.clone(),
            description: header.description,
            current_hash: hash.clone(),
            license: header.license,
            source_kind: prepared.kind.to_string(),
            source_url: Some(prepared.location.clone()),
            source_ref: prepared.reference.clone(),
            source_subdir: prepared.subdir.clone(),
            has_scripts,
            size_bytes,
            installed_at: now.clone(),
            updated_at: now.clone(),
        };
        self.registry.upsert_skill(&row)?;
        self.registry.add_version(&VersionRow {
            skill_id: id.clone(),
            hash: hash.clone(),
            path: version.to_string_lossy().to_string(),
            source_kind: row.source_kind.clone(),
            source_url: row.source_url.clone(),
            source_ref: row.source_ref.clone(),
            source_subdir: row.source_subdir.clone(),
            license: row.license.clone(),
            created_at: now,
        })?;
        self.update_current_link(&id, &hash)?;

        self.registry
            .get(&id)?
            .ok_or_else(|| anyhow::anyhow!("installed skill {id} disappeared from the registry"))
    }

    pub fn check_update(&self, id: &str) -> Result<UpdateCheck> {
        let summary = self
            .registry
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("skill not found: {id}"))?;
        let prepared = prepare_from_summary(&summary)?;
        let latest_hash = hash_tree(&prepared.path)?;
        Ok(UpdateCheck {
            id: id.to_string(),
            current_hash: summary.current_hash.clone(),
            latest_hash: latest_hash.clone(),
            update_available: latest_hash != summary.current_hash,
        })
    }

    pub fn update(&mut self, id: &str) -> Result<SkillSummary> {
        let summary = self
            .registry
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("skill not found: {id}"))?;
        let prepared = prepare_from_summary(&summary)?;
        let header = parse_skill(&prepared.path)?;
        let new_id = slugify(&header.name)?;
        if new_id != id {
            bail!("source now declares skill {new_id}; expected {id}");
        }
        self.install_prepared(&prepared)
    }

    pub fn remove(&mut self, id: &str) -> Result<bool> {
        let Some(summary) = self.registry.get(id)? else {
            return Ok(false);
        };

        for link in self
            .registry
            .list_global_links()?
            .into_iter()
            .filter(|link| link.skill_id == id)
        {
            let path = PathBuf::from(&link.path);
            if let Ok(metadata) = fs::symlink_metadata(&path) {
                if metadata.file_type().is_symlink() {
                    let _ = fs::remove_file(&path);
                }
            }
        }

        let current = self.config.skills_dir.join(id);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                fs::remove_file(&current)?;
            } else {
                bail!("refusing to remove non-symlink at {}", current.display());
            }
        }

        let version_dir = self.config.store_dir.join(id);
        if version_dir.exists() {
            let trash = self
                .config
                .root
                .join("skill-layer")
                .join("trash")
                .join(now_rfc3339().replace(':', "-"))
                .join(id);
            if let Some(parent) = trash.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&version_dir, &trash)
                .with_context(|| format!("move {} to trash", version_dir.display()))?;
        }

        self.registry.delete_skill(id)?;
        let _ = summary;
        Ok(true)
    }

    pub fn sync_global_links(&mut self, agents: Option<&[String]>) -> Result<Vec<GlobalLink>> {
        let skills = self.registry.list()?;
        let now = now_rfc3339();

        for (agent_type, dir) in global_dirs() {
            if let Some(agents) = agents {
                if !agents.iter().any(|agent| agent == agent_type) {
                    continue;
                }
            }
            fs::create_dir_all(&dir)?;

            for skill in &skills {
                let target = self.config.skills_dir.join(&skill.id);
                if !target.exists() {
                    continue;
                }
                let link_path = dir.join(&skill.id);
                let mut state = "linked";

                if let Ok(metadata) = fs::symlink_metadata(&link_path) {
                    if metadata.file_type().is_symlink() {
                        let existing = fs::read_link(&link_path)?;
                        if existing != target {
                            fs::remove_file(&link_path)?;
                            symlink(&target, &link_path)?;
                        }
                    } else {
                        state = "foreign";
                    }
                } else {
                    symlink(&target, &link_path)?;
                }

                self.registry.upsert_global_link(&GlobalLink {
                    skill_id: skill.id.clone(),
                    agent_type: agent_type.to_string(),
                    path: link_path.to_string_lossy().to_string(),
                    state: state.to_string(),
                    linked_at: now.clone(),
                })?;
            }
        }

        self.registry.list_global_links()
    }

    pub fn list_global_links(&self) -> Result<Vec<GlobalLink>> {
        self.registry.list_global_links()
    }

    pub fn materialize(
        &self,
        home: &Path,
        skills_dir: Option<&Path>,
        refs: &[SkillRef],
    ) -> Result<Materialized, MaterializeError> {
        let home = home
            .canonicalize()
            .map_err(|error| MaterializeError::Message(format!("invalid session home: {error}")))?;
        if !home.is_dir() {
            return Err(MaterializeError::Message(format!(
                "session home is not a directory: {}",
                home.display()
            )));
        }

        if let Some(root) = &self.config.session_root {
            let root = root.canonicalize().map_err(|error| {
                MaterializeError::Message(format!("invalid session root: {error}"))
            })?;
            if !home.starts_with(&root) {
                return Err(MaterializeError::Message(format!(
                    "session home {} is outside the allowed session root {}",
                    home.display(),
                    root.display()
                )));
            }
        }

        let skills_dir = skills_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("skills"));
        fs::create_dir_all(&skills_dir).map_err(|error| {
            MaterializeError::Message(format!("create skills directory: {error}"))
        })?;
        let skills_dir = skills_dir.canonicalize().map_err(|error| {
            MaterializeError::Message(format!("resolve skills directory: {error}"))
        })?;
        if !skills_dir.starts_with(&home) {
            return Err(MaterializeError::Message(format!(
                "skills directory {} is outside session home {}",
                skills_dir.display(),
                home.display()
            )));
        }

        let mut targets = Vec::with_capacity(refs.len());
        let mut missing = Vec::new();

        for skill_ref in refs {
            if skill_ref.id.is_empty()
                || skill_ref.id.contains('/')
                || skill_ref.id.contains('\\')
                || skill_ref.id.contains("..")
            {
                return Err(MaterializeError::Message(format!(
                    "invalid skill id: {}",
                    skill_ref.id
                )));
            }

            let hash = match &skill_ref.hash {
                Some(hash) if !hash.is_empty() => Some(hash.clone()),
                _ => self
                    .registry
                    .current_hash(&skill_ref.id)
                    .map_err(|error| MaterializeError::Message(error.to_string()))?,
            };

            let Some(hash) = hash else {
                missing.push(skill_ref.id.clone());
                continue;
            };

            let target = self.config.store_dir.join(&skill_ref.id).join(&hash);
            if !target.is_dir() {
                missing.push(skill_ref.id.clone());
                continue;
            }
            targets.push((skill_ref.id.clone(), target));
        }

        if !missing.is_empty() {
            return Err(MaterializeError::Missing(missing));
        }

        let mut links = Vec::with_capacity(targets.len());
        for (id, target) in targets {
            let link = skills_dir.join(&id);
            if let Ok(metadata) = fs::symlink_metadata(&link) {
                if metadata.file_type().is_symlink() {
                    let current = fs::read_link(&link).map_err(|error| {
                        MaterializeError::Message(format!(
                            "read existing link {}: {error}",
                            link.display()
                        ))
                    })?;
                    if current == target {
                        links.push(MaterializedLink {
                            id,
                            path: link.to_string_lossy().to_string(),
                            target: target.to_string_lossy().to_string(),
                        });
                        continue;
                    }
                    fs::remove_file(&link).map_err(|error| {
                        MaterializeError::Message(format!(
                            "replace link {}: {error}",
                            link.display()
                        ))
                    })?;
                } else {
                    return Err(MaterializeError::Message(format!(
                        "refusing to replace non-symlink at {}",
                        link.display()
                    )));
                }
            }

            symlink(&target, &link).map_err(|error| {
                MaterializeError::Message(format!("create link {}: {error}", link.display()))
            })?;
            links.push(MaterializedLink {
                id,
                path: link.to_string_lossy().to_string(),
                target: target.to_string_lossy().to_string(),
            });
        }

        Ok(Materialized {
            home: home.to_string_lossy().to_string(),
            skills_dir: skills_dir.to_string_lossy().to_string(),
            links,
        })
    }

    pub fn create_collection(
        &mut self,
        name: &str,
        description: &str,
        skill_ids: &[String],
    ) -> Result<Collection> {
        self.registry
            .create_collection(name, description, skill_ids)
    }

    pub fn list_collections(&self) -> Result<Vec<Collection>> {
        self.registry.list_collections()
    }

    pub fn delete_collection(&mut self, id: &str) -> Result<bool> {
        self.registry.delete_collection(id)
    }

    fn update_current_link(&self, id: &str, hash: &str) -> Result<()> {
        let link = self.config.skills_dir.join(id);
        if let Ok(metadata) = fs::symlink_metadata(&link) {
            if metadata.file_type().is_symlink() {
                fs::remove_file(&link)?;
            } else {
                bail!(
                    "refusing to replace non-symlink at {}; move it before installing {id}",
                    link.display()
                );
            }
        }

        let target = PathBuf::from("../skill-store").join(id).join(hash);
        symlink(target, &link)
            .with_context(|| format!("link {} to the current version", link.display()))?;
        Ok(())
    }
}

fn prepare_from_summary(summary: &SkillSummary) -> Result<source::PreparedSource> {
    let location = summary
        .source_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("skill {} has no source URL", summary.id))?;
    let path = if summary.source_kind == "local" {
        Some(location)
    } else {
        None
    };
    let source = if summary.source_kind == "local" {
        None
    } else {
        Some(location)
    };
    source::prepare(
        path,
        source,
        summary.source_ref.as_deref(),
        summary.source_subdir.as_deref(),
    )
}

fn global_dirs() -> Vec<(&'static str, PathBuf)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));

    vec![
        ("claude_code", home.join(".claude").join("skills")),
        ("codex", home.join(".codex").join("skills")),
        ("open_code", config_home.join("opencode").join("skills")),
        ("gemini", home.join(".gemini").join("skills")),
    ]
}
