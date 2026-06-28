//! Skill bundle installation — archive detection, safe extraction, discovery,
//! and bundle preview/install built on quill P1's per-skill primitives. Safely
//! extracts .zip and .tar.gz bundles, discovers every skill manifest, scans
//! each, and installs clean ones via quill P1's per-skill path. Built on
//! `skill_remote`.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

use super::skill_remote::{SkillPreview, preview_skill_text};

/// Supported skill-bundle archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
}

/// Max bytes downloaded for a bundle archive.
pub const BUNDLE_MAX_BYTES: usize = 5 * 1024 * 1024;
/// Max number of entries an archive may contain.
pub const BUNDLE_MAX_ENTRIES: usize = 256;
/// Max total uncompressed bytes written during extraction (zip-bomb guard).
pub const BUNDLE_MAX_TOTAL_UNCOMPRESSED: u64 = 32 * 1024 * 1024;

/// HTTP request timeout for fetching a remote bundle.
const BUNDLE_FETCH_TIMEOUT_SECS: u64 = 30;

/// Classify a URL by archive extension. Returns `None` if not an archive we handle.
pub fn is_archive_url(url: &str) -> Option<ArchiveKind> {
    let path = reqwest::Url::parse(url).ok()?.path().to_ascii_lowercase();
    if path.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else {
        None
    }
}

/// Extract `bytes` (`kind` archive) into `dest`, safely. Rejects entries that
/// would escape `dest` (`..`/absolute), symlinks, and archives exceeding the
/// entry or total-uncompressed caps.
pub fn extract_archive(bytes: &[u8], kind: ArchiveKind, dest: &Path) -> Result<()> {
    match kind {
        ArchiveKind::Zip => extract_zip(bytes, dest),
        ArchiveKind::TarGz => extract_targz(bytes, dest),
    }
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    use std::io::Read;
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| anyhow::anyhow!("open zip: {e}"))?;
    let count = zip.len();
    if count > BUNDLE_MAX_ENTRIES {
        bail!("archive has too many entries (> {BUNDLE_MAX_ENTRIES})");
    }
    let mut total: u64 = 0;
    for i in 0..count {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("zip entry {i}: {e}"))?;
        // Skip directories.
        if entry.is_dir() {
            continue;
        }
        // Zip-slip guard: enclosed_name() returns None for unsafe paths.
        let rel = match entry.enclosed_name() {
            Some(p) => p,
            None => continue,
        };
        // Bomb guard.
        total = total.saturating_add(entry.size());
        if total > BUNDLE_MAX_TOTAL_UNCOMPRESSED {
            bail!("archive uncompressed size exceeds {BUNDLE_MAX_TOTAL_UNCOMPRESSED} bytes");
        }
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("mkdir: {e}"))?;
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| anyhow::anyhow!("read entry: {e}"))?;
        std::fs::write(&out, &data).map_err(|e| anyhow::anyhow!("write {}: {e}", out.display()))?;
    }
    Ok(())
}

fn extract_targz(bytes: &[u8], dest: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut ar = tar::Archive::new(gz);
    let mut count = 0usize;
    let mut total: u64 = 0;
    for entry in ar.entries().map_err(|e| anyhow::anyhow!("read tar: {e}"))? {
        let mut entry = entry.map_err(|e| anyhow::anyhow!("tar entry: {e}"))?;
        // Skip non-regular files (symlinks, hardlinks, devices) — defense.
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        count += 1;
        if count > BUNDLE_MAX_ENTRIES {
            bail!("archive has too many entries (> {BUNDLE_MAX_ENTRIES})");
        }
        total = total.saturating_add(entry.size());
        if total > BUNDLE_MAX_TOTAL_UNCOMPRESSED {
            bail!("archive uncompressed size exceeds {BUNDLE_MAX_TOTAL_UNCOMPRESSED} bytes");
        }
        // unpack_in is documented to refuse paths that escape `dest` (returns
        // Ok(false) and skips), giving zip-slip protection for tar.
        let _unpacked = entry
            .unpack_in(dest)
            .map_err(|e| anyhow::anyhow!("unpack: {e}"))?;
    }
    Ok(())
}

/// Walk `dir` recursively; return paths that parse as a valid skill manifest.
/// Non-skill files (READMEs, licenses, assets) are silently ignored.
pub fn discover_skills(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_skills(dir, &mut out);
    out.sort();
    out
}

fn walk_skills(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            walk_skills(&p, out);
        } else if is_candidate_skill_file(&p)
            && let Ok(text) = std::fs::read_to_string(&p)
        {
            let parses = if is_md_path(&p) {
                mur_common::skill::parse_markdown(&text).is_ok()
            } else {
                mur_common::skill::parse_canonical(&text).is_ok()
            };
            if parses {
                out.push(p);
            }
        }
    }
}

fn is_candidate_skill_file(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "yaml" | "yml" | "md" | "markdown"
    )
}

fn is_md_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "md" | "markdown"
    )
}

/// Download a bundle archive, size-capped at [`BUNDLE_MAX_BYTES`].
pub async fn fetch_bundle(url: &str, _kind: ArchiveKind) -> Result<Vec<u8>> {
    let url = super::skill_remote::validate_skill_url(url)?;
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(BUNDLE_FETCH_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?;
    if !resp.status().is_success() {
        bail!("fetch {url}: HTTP {}", resp.status());
    }
    if let Some(len) = resp.content_length()
        && len > BUNDLE_MAX_BYTES as u64
    {
        bail!("bundle too large ({len} bytes; max {BUNDLE_MAX_BYTES})");
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("read body: {e}"))?;
    if bytes.len() > BUNDLE_MAX_BYTES {
        bail!(
            "bundle too large ({} bytes; max {BUNDLE_MAX_BYTES})",
            bytes.len()
        );
    }
    Ok(bytes.to_vec())
}

/// Fetch + extract + discover + preview each skill in a bundle. Installs nothing.
pub async fn preview_bundle_url(url: &str) -> Result<Vec<SkillPreview>> {
    let kind = is_archive_url(url).ok_or_else(|| anyhow::anyhow!("not a supported archive URL"))?;
    let bytes = fetch_bundle(url, kind).await?;
    let tmp = tempdir_unique("mur-bundle")?;
    extract_archive(&bytes, kind, &tmp)?;
    let paths = discover_skills(&tmp);
    if paths.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("no skills found in bundle");
    }
    let mut previews = Vec::new();
    for p in &paths {
        if let Ok(text) = std::fs::read_to_string(p) {
            match preview_skill_text(&text, is_md_path(p)) {
                Ok(pv) => previews.push(pv),
                Err(_) => { /* skip non-conforming file already filtered by discover */ }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(previews)
}

/// Fetch + extract + discover + install. Clean skills always install; skills
/// with blocking findings install only if `accept_findings` is true.
/// Returns installed skill ids (`skills/<name>`).
pub async fn install_bundle_from_url(
    agent: &str,
    url: &str,
    accept_findings: bool,
) -> Result<Vec<String>> {
    let kind = is_archive_url(url).ok_or_else(|| anyhow::anyhow!("not a supported archive URL"))?;
    let bytes = fetch_bundle(url, kind).await?;
    let tmp = tempdir_unique("mur-bundle")?;
    extract_archive(&bytes, kind, &tmp)?;
    let paths = discover_skills(&tmp);
    if paths.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("no skills found in bundle");
    }
    let mut installed = Vec::new();
    for p in &paths {
        if let Ok(text) = std::fs::read_to_string(p) {
            let preview = match preview_skill_text(&text, is_md_path(p)) {
                Ok(pv) => pv,
                Err(_) => continue,
            };
            // Fail-closed: skip skills with blocking findings unless accepted.
            if preview.blocking && !accept_findings {
                continue;
            }
            let ext = if is_md_path(p) { "md" } else { "yaml" };
            let safe: String = preview
                .name
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            let safe = if safe.is_empty() {
                "skill".to_string()
            } else {
                safe
            };
            let tmp_skill = std::env::temp_dir().join(format!(
                "mur-skill-bundle-{}-{safe}.{ext}",
                std::process::id()
            ));
            if std::fs::write(&tmp_skill, &text).is_ok() {
                let add_result = super::skill::cmd_skill_add(agent, &tmp_skill.to_string_lossy());
                let _ = std::fs::remove_file(&tmp_skill);
                if add_result.is_ok() {
                    installed.push(format!("skills/{}", preview.name));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(installed)
}

/// Create a unique temporary directory for bundle extraction. Removes any
/// pre-existing directory with the same name first.
pub fn tempdir_unique(prefix: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("mkdir temp: {e}"))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task 1 ────────────────────────────────────────────────────────────────

    #[test]
    fn detects_archive_urls() {
        assert_eq!(
            is_archive_url("https://example.com/skills.zip"),
            Some(ArchiveKind::Zip)
        );
        assert_eq!(
            is_archive_url("https://example.com/pack.tar.gz"),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(
            is_archive_url("https://example.com/pack.tgz"),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(is_archive_url("https://example.com/skill.yaml"), None);
        assert_eq!(is_archive_url("https://example.com/skill.md"), None);
    }

    // ── Task 2 ────────────────────────────────────────────────────────────────

    #[test]
    fn extract_zip_roundtrips_a_skill() {
        use std::io::Write;
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("ok/skill.yaml", opts).unwrap();
            zw.write_all(b"name: z").unwrap();
            // Traversal entry — should be skipped.
            zw.start_file("../escape.txt", opts).unwrap();
            zw.write_all(b"bad").unwrap();
            zw.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        extract_archive(&buf, ArchiveKind::Zip, dir.path()).unwrap();
        assert!(dir.path().join("ok/skill.yaml").exists());
        // traversal entry must NOT be written outside dest
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn extract_targz_roundtrips_a_skill() {
        use std::io::Write;
        let mut tar_buf = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_buf);
            let data = b"name: y";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "pack/skill.yaml", &data[..]).unwrap();
            b.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            use flate2::{Compression, write::GzEncoder};
            let mut e = GzEncoder::new(&mut gz, Compression::default());
            e.write_all(&tar_buf).unwrap();
            e.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        extract_archive(&gz, ArchiveKind::TarGz, dir.path()).unwrap();
        assert!(dir.path().join("pack/skill.yaml").exists());
    }

    // ── Task 3 ────────────────────────────────────────────────────────────────

    const VALID_SKILL: &str = "name: a\nversion: 1.0.0\npublisher: human:test\ndescription: d\ncategory: context\ncontent:\n  abstract: x\n  context: y\n";

    #[test]
    fn discovers_nested_skills_and_ignores_non_skills() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/skill.yaml"), VALID_SKILL).unwrap();
        std::fs::write(dir.path().join("README.md"), "# just docs, not a skill").unwrap();
        let found = discover_skills(dir.path());
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("sub/skill.yaml"));
    }

    // ── Task 4 ────────────────────────────────────────────────────────────────

    #[test]
    fn extract_then_discover_then_preview_is_consistent() {
        use std::io::Write;
        let skill = "name: packed\nversion: 1.0.0\npublisher: human:test\ndescription: d\ncategory: context\ncontent:\n  abstract: x\n  context: y\n";
        let mut tarb = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tarb);
            let mut h = tar::Header::new_gnu();
            h.set_size(skill.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "skill.yaml", skill.as_bytes())
                .unwrap();
            b.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            use flate2::{Compression, write::GzEncoder};
            let mut e = GzEncoder::new(&mut gz, Compression::default());
            e.write_all(&tarb).unwrap();
            e.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        extract_archive(&gz, ArchiveKind::TarGz, dir.path()).unwrap();
        let skills = discover_skills(dir.path());
        assert_eq!(skills.len(), 1);
        let text = std::fs::read_to_string(&skills[0]).unwrap();
        let preview = preview_skill_text(&text, is_md_path(&skills[0])).unwrap();
        assert_eq!(preview.name, "packed");
        assert!(!preview.blocking);
    }
}
