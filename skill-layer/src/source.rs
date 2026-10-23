use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Archive;
use tempfile::TempDir;
use zip::ZipArchive;

const MAX_DOWNLOAD_BYTES: u64 = 10 * 1024 * 1024;
const MAX_EXTRACT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_EXTRACT_FILES: usize = 1000;

#[derive(Debug)]
pub struct PreparedSource {
    pub _temp: Option<TempDir>,
    pub path: PathBuf,
    pub kind: &'static str,
    pub location: String,
    pub reference: Option<String>,
    pub subdir: Option<String>,
}

pub fn prepare(
    path: Option<&str>,
    source: Option<&str>,
    git_ref: Option<&str>,
    subdir: Option<&str>,
) -> Result<PreparedSource> {
    if let Some(path) = path.map(str::trim).filter(|value| !value.is_empty()) {
        return local_source(Path::new(path), git_ref, subdir);
    }

    let source = source
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("either `path` or `source` is required"))?;

    if Path::new(source).is_dir() {
        return local_source(Path::new(source), git_ref, subdir);
    }

    let mut parsed = parse_source(source);
    if let Some(explicit_ref) = git_ref.map(str::trim).filter(|value| !value.is_empty()) {
        parsed.reference = Some(explicit_ref.to_string());
    }
    if let Some(explicit_subdir) = subdir.map(str::trim).filter(|value| !value.is_empty()) {
        parsed.subdir = Some(explicit_subdir.to_string());
    }

    match parsed.kind {
        ParsedKind::Git => clone_git(&parsed),
        ParsedKind::Download => download(&parsed),
    }
}

fn local_source(
    path: &Path,
    reference: Option<&str>,
    subdir: Option<&str>,
) -> Result<PreparedSource> {
    let root = path
        .canonicalize()
        .with_context(|| format!("resolve local skill source {}", path.display()))?;
    if !root.is_dir() {
        bail!("local skill source is not a directory: {}", root.display());
    }
    let location = root.to_string_lossy().to_string();
    let path = apply_subdir(&root, subdir)?;
    Ok(PreparedSource {
        _temp: None,
        path,
        kind: "local",
        location,
        reference: reference.map(str::to_string),
        subdir: subdir.map(str::to_string),
    })
}

struct ParsedSource {
    kind: ParsedKind,
    url: String,
    reference: Option<String>,
    subdir: Option<String>,
}

enum ParsedKind {
    Git,
    Download,
}

fn parse_source(source: &str) -> ParsedSource {
    if let Some((repo, rest)) = source.split_once("/tree/") {
        let (reference, subdir) = match rest.split_once('/') {
            Some((reference, subdir)) => (reference.to_string(), Some(subdir.to_string())),
            None => (rest.to_string(), None),
        };
        let url = if repo.ends_with(".git") {
            repo.to_string()
        } else {
            format!("{repo}.git")
        };
        return ParsedSource {
            kind: ParsedKind::Git,
            url,
            reference: Some(reference),
            subdir,
        };
    }

    if is_github_shorthand(source) {
        return ParsedSource {
            kind: ParsedKind::Git,
            url: format!("https://github.com/{source}.git"),
            reference: None,
            subdir: None,
        };
    }

    if is_git_url(source) {
        return ParsedSource {
            kind: ParsedKind::Git,
            url: source.to_string(),
            reference: None,
            subdir: None,
        };
    }

    ParsedSource {
        kind: ParsedKind::Download,
        url: source.to_string(),
        reference: None,
        subdir: None,
    }
}

fn is_github_shorthand(source: &str) -> bool {
    if source.contains(':') || source.starts_with('.') || source.contains(' ') {
        return false;
    }
    let mut parts = source.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    !owner.is_empty() && !repo.is_empty() && parts.next().is_none()
}

fn is_git_url(source: &str) -> bool {
    if source.starts_with("git@") || source.starts_with("ssh://") || source.ends_with(".git") {
        return true;
    }
    if !source.starts_with("http://") && !source.starts_with("https://") {
        return false;
    }
    let lower = source.to_lowercase();
    if lower.contains(".zip") || lower.contains(".tar") || lower.contains(".tgz") {
        return false;
    }
    ["github.com", "gitlab.com", "dev.azure.com", "bitbucket.org"]
        .iter()
        .any(|host| lower.contains(host))
}

fn clone_git(source: &ParsedSource) -> Result<PreparedSource> {
    let temp = TempDir::new().context("create temporary git directory")?;
    let repo = temp.path().join("repo");

    let mut command = Command::new("git");
    command
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(reference) = &source.reference {
        command.arg("--branch").arg(reference);
    }
    command.arg(&source.url).arg(&repo);

    let output = command.output().context("run git clone")?;
    if !output.status.success() {
        // A commit SHA is not a branch; retry with a full clone and checkout.
        if let Some(reference) = &source.reference {
            let _ = fs::remove_dir_all(&repo);
            let retry = Command::new("git")
                .arg("clone")
                .env("GIT_TERMINAL_PROMPT", "0")
                .arg(&source.url)
                .arg(&repo)
                .output()
                .context("retry git clone")?;
            if retry.status.success() {
                let checkout = Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .arg("checkout")
                    .arg(reference)
                    .output()
                    .context("git checkout")?;
                if checkout.status.success() {
                    let path = apply_subdir(&repo, source.subdir.as_deref())?;
                    return Ok(PreparedSource {
                        _temp: Some(temp),
                        path,
                        kind: "git",
                        location: source.url.clone(),
                        reference: Some(reference.clone()),
                        subdir: source.subdir.clone(),
                    });
                }
            }
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git clone failed: {}", stderr.trim());
    }

    let path = apply_subdir(&repo, source.subdir.as_deref())?;
    Ok(PreparedSource {
        _temp: Some(temp),
        path,
        kind: "git",
        location: source.url.clone(),
        reference: source.reference.clone(),
        subdir: source.subdir.clone(),
    })
}

fn download(source: &ParsedSource) -> Result<PreparedSource> {
    let response = ureq::get(&source.url)
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .map_err(|error| anyhow::anyhow!("download {} failed: {error}", source.url))?;
    if let Some(length) = response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
    {
        if length > MAX_DOWNLOAD_BYTES {
            bail!("download is larger than {MAX_DOWNLOAD_BYTES} bytes");
        }
    }

    let mut bytes = Vec::new();
    let mut reader = response.into_reader();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).context("read download body")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
            bail!("download is larger than {MAX_DOWNLOAD_BYTES} bytes");
        }
    }

    let temp = TempDir::new().context("create temporary download directory")?;
    let root = temp.path().join("skill");
    fs::create_dir_all(&root)?;

    if looks_like_zip(&bytes) {
        extract_zip(&bytes, &root)?;
    } else if looks_like_gzip(&bytes) {
        extract_tar_gz(&bytes, &root)?;
    } else if looks_like_tar(&bytes) {
        extract_tar(&bytes, &root)?;
    } else if looks_like_skill(&bytes) {
        fs::write(root.join("SKILL.md"), &bytes)?;
    } else {
        bail!("download is neither a SKILL.md nor a supported archive");
    }

    let root = normalize_single_root(&root)?;
    let path = apply_subdir(&root, source.subdir.as_deref())?;
    Ok(PreparedSource {
        _temp: Some(temp),
        path,
        kind: "url",
        location: source.url.clone(),
        reference: source.reference.clone(),
        subdir: source.subdir.clone(),
    })
}

fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06")
}

fn looks_like_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

fn looks_like_tar(bytes: &[u8]) -> bool {
    bytes.len() > 262 && &bytes[257..262] == b"ustar"
}

fn looks_like_skill(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
    text.trim_start().starts_with("---")
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).context("open zip archive")?;
    let mut total = 0u64;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else {
            bail!("zip entry has an unsafe path");
        };
        let target = dest.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                bail!("zip contains a symlink: {}", relative.display());
            }
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut output)?;
        total += entry.size();
        check_extract_limits(total, index + 1)?;
    }
    Ok(())
}

fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<()> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    extract_tar_reader(decoder, dest)
}

fn extract_tar(bytes: &[u8], dest: &Path) -> Result<()> {
    extract_tar_reader(Cursor::new(bytes), dest)
}

fn extract_tar_reader<R: Read>(reader: R, dest: &Path) -> Result<()> {
    let mut archive = Archive::new(reader);
    let mut total = 0u64;
    let mut count = 0usize;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path.is_absolute() || path.components().any(|part| part.as_os_str() == "..") {
            bail!("tar entry has an unsafe path: {}", path.display());
        }
        let target = dest.join(&path);
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            bail!("tar contains a link: {}", path.display());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut output)?;
        total += entry.size();
        count += 1;
        check_extract_limits(total, count)?;
    }
    Ok(())
}

fn check_extract_limits(total: u64, count: usize) -> Result<()> {
    if total > MAX_EXTRACT_BYTES {
        bail!("archive expands beyond {MAX_EXTRACT_BYTES} bytes");
    }
    if count > MAX_EXTRACT_FILES {
        bail!("archive contains more than {MAX_EXTRACT_FILES} files");
    }
    Ok(())
}

fn normalize_single_root(root: &Path) -> Result<PathBuf> {
    if root.join("SKILL.md").is_file() {
        return Ok(root.to_path_buf());
    }
    let entries = fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    if entries.len() == 1 && entries[0].path().is_dir() {
        return Ok(entries[0].path());
    }
    Ok(root.to_path_buf())
}

fn apply_subdir(root: &Path, subdir: Option<&str>) -> Result<PathBuf> {
    let Some(subdir) = subdir.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(root.to_path_buf());
    };
    let subdir = Path::new(subdir);
    if subdir.is_absolute() || subdir.components().any(|part| part.as_os_str() == "..") {
        bail!("invalid subdir: {}", subdir.display());
    }
    let root = root.canonicalize()?;
    let path = root.join(subdir).canonicalize()?;
    if !path.starts_with(&root) {
        bail!("subdir escapes the source root");
    }
    if !path.is_dir() {
        bail!("subdir is not a directory: {}", path.display());
    }
    Ok(path)
}
