use crate::config::Config;
use crate::registry::{Collection, Registry, SkillRow, SkillSummary, VersionRow};
use crate::skill::{
    collect_files, copy_tree, has_script_files, hash_tree, list_files, now_rfc3339, parse_skill,
    slugify, FileInfo,
};
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

    /// Install or update one skill from a local directory. The directory must
    /// contain `SKILL.md`; a new content hash becomes the current version.
    pub fn install_local(&mut self, source: &Path) -> Result<SkillSummary> {
        let source = source
            .canonicalize()
            .with_context(|| format!("resolve skill source {}", source.display()))?;
        if !source.is_dir() {
            bail!("skill source is not a directory: {}", source.display());
        }

        let header = parse_skill(&source)?;
        let id = slugify(&header.name)?;
        let hash = hash_tree(&source)?;
        let version = self.config.store_dir.join(&id).join(&hash);

        if !version.exists() {
            let parent = self.config.store_dir.join(&id);
            fs::create_dir_all(&parent)?;
            let temp = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
            if let Err(error) = copy_tree(&source, &temp) {
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
            source_kind: "local".to_string(),
            source_url: Some(source.to_string_lossy().to_string()),
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
            license: row.license.clone(),
            created_at: now,
        })?;
        self.update_current_link(&id, &hash)?;

        self.registry
            .get(&id)?
            .ok_or_else(|| anyhow::anyhow!("installed skill {id} disappeared from the registry"))
    }

    /// Create the per-session skill directory and symlink only the selected
    /// versions into it. The caller creates the CLI config home.
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
