use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Safety limits for one skill package. These are intentionally small: a skill
/// is instructions plus a few assets, not a general-purpose archive.
pub const MAX_FILES: usize = 2000;
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct SkillHeader {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
}

pub fn parse_frontmatter(content: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return fields;
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        fields.insert(key, value.to_string());
    }

    fields
}

pub fn parse_skill(root: &Path) -> Result<SkillHeader> {
    let content_path = root.join("SKILL.md");
    let content = fs::read_to_string(&content_path)
        .with_context(|| format!("read {}", content_path.display()))?;
    let fields = parse_frontmatter(&content);

    let name = fields
        .get("name")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("SKILL.md frontmatter is missing `name`"))?;
    let description = fields
        .get("description")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("SKILL.md frontmatter is missing `description`"))?;
    let license = fields
        .get("license")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    Ok(SkillHeader {
        name,
        description,
        license,
    })
}

pub fn slugify(name: &str) -> Result<String> {
    let mut slug = String::new();
    let mut last_dash = false;

    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        bail!("skill name does not contain any usable characters");
    }
    Ok(slug)
}

pub fn hash_tree(root: &Path) -> Result<String> {
    let files = collect_files(root)?;
    let mut hasher = Sha256::new();

    for (relative, _) in &files {
        hasher.update(relative.as_bytes());
        hasher.update([0u8]);
        let bytes = fs::read(root.join(relative))
            .with_context(|| format!("read {} for hashing", relative))?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn copy_tree(src: &Path, dst: &Path) -> Result<(u64, bool)> {
    let files = collect_files(src)?;
    let mut total = 0u64;
    let mut has_scripts = false;

    fs::create_dir_all(dst)?;

    for (relative, size) in &files {
        let target = dst.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src.join(relative), &target).with_context(|| format!("copy {}", relative))?;
        total += size;
        if is_script(relative) {
            has_scripts = true;
        }
    }

    Ok((total, has_scripts))
}

pub fn list_files(root: &Path) -> Result<Vec<FileInfo>> {
    Ok(collect_files(root)?
        .into_iter()
        .map(|(path, size)| FileInfo { path, size })
        .collect())
}

pub fn collect_files(root: &Path) -> Result<Vec<(String, u64)>> {
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }

    let mut files = Vec::new();
    let mut total = 0u64;

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".git")
    {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            bail!(
                "skill package contains a symlink ({}); symlinks are not allowed",
                entry.path().display()
            );
        }
        if !entry.file_type().is_file() {
            continue;
        }

        let relative = entry
            .path()
            .strip_prefix(root)
            .with_context(|| format!("relative path for {}", entry.path().display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() || relative.starts_with("../") {
            bail!("invalid skill file path: {relative}");
        }

        let size = entry.metadata()?.len();
        total += size;
        if files.len() >= MAX_FILES {
            bail!("skill package has more than {MAX_FILES} files");
        }
        if total > MAX_BYTES {
            bail!("skill package is larger than {MAX_BYTES} bytes");
        }

        files.push((relative, size));
    }

    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

pub fn has_script_files(root: &Path) -> Result<bool> {
    Ok(collect_files(root)?.iter().any(|(path, _)| is_script(path)))
}

pub fn is_script(path: &str) -> bool {
    if path.starts_with("scripts/") || path.contains("/scripts/") {
        return true;
    }
    matches!(
        PathBuf::from(path)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("sh" | "bash" | "zsh" | "py" | "rb" | "pl" | "js" | "ts" | "mjs" | "cjs")
    )
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_frontmatter() {
        let fields = parse_frontmatter(
            "---\nname: backend-api\ndescription: \"Backend API work\"\nlicense: MIT\n---\n\n# Body\n",
        );
        assert_eq!(fields.get("name").map(String::as_str), Some("backend-api"));
        assert_eq!(
            fields.get("description").map(String::as_str),
            Some("Backend API work")
        );
        assert_eq!(fields.get("license").map(String::as_str), Some("MIT"));
    }

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("Backend API").unwrap(), "backend-api");
        assert_eq!(slugify("iOS Expert!").unwrap(), "ios-expert");
        assert!(slugify("***").is_err());
    }
}
