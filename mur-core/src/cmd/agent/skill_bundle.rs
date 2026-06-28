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
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("mkdir: {e}"))?;
        }
        // Bomb guard — bound the ACTUAL read, not the header-declared size
        // (entry.size() is attacker-controlled and can lie as 0). We read
        // actual bytes via Read::take so the property holds even if the stored
        // uncompressed-size header is falsified.
        let budget = BUNDLE_MAX_TOTAL_UNCOMPRESSED.saturating_sub(total);
        let mut data = Vec::new();
        // read budget+1 so we can detect overflow
        std::io::Read::take(&mut entry, budget + 1)
            .read_to_end(&mut data)
            .map_err(|e| anyhow::anyhow!("read entry: {e}"))?;
        total = total.saturating_add(data.len() as u64);
        if total > BUNDLE_MAX_TOTAL_UNCOMPRESSED {
            bail!("archive uncompressed size exceeds {BUNDLE_MAX_TOTAL_UNCOMPRESSED} bytes");
        }
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
        // unpack_in refuses paths that escape `dest` by returning Ok(false).
        if !entry
            .unpack_in(dest)
            .map_err(|e| anyhow::anyhow!("unpack: {e}"))?
        {
            bail!("unsafe path in archive (refused by unpack_in)");
        }
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
/// Uses a streaming accumulate loop so the cap applies to actual bytes
/// received, not just the Content-Length header (which may be absent or
/// spoofed). Aborts the download as soon as the running total would exceed
/// the cap — no DoS via a header-lying server that omits Content-Length.
pub async fn fetch_bundle(url: &str) -> Result<Vec<u8>> {
    let url = super::skill_remote::validate_skill_url(url)?;
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(BUNDLE_FETCH_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("network error: {e}"))?;
    if !resp.status().is_success() {
        bail!("server returned {}", resp.status());
    }
    // Early rejection when Content-Length is present and already over cap.
    if let Some(len) = resp.content_length()
        && len > BUNDLE_MAX_BYTES as u64
    {
        bail!("bundle too large ({len} bytes; max {BUNDLE_MAX_BYTES})");
    }
    // Stream body and enforce cap on actual bytes received.
    let mut buf: Vec<u8> = Vec::new();
    let mut body = resp;
    while let Some(chunk) = body
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("read body: {e}"))?
    {
        if buf.len() + chunk.len() > BUNDLE_MAX_BYTES {
            bail!("bundle too large (exceeds {BUNDLE_MAX_BYTES} bytes)");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Fetch + extract + discover + preview each skill in a bundle. Installs nothing.
pub async fn preview_bundle_url(url: &str) -> Result<Vec<SkillPreview>> {
    let kind = is_archive_url(url).ok_or_else(|| anyhow::anyhow!("not a supported archive URL"))?;
    let bytes = fetch_bundle(url).await?;
    let tmp = tempfile::TempDir::new().map_err(|e| anyhow::anyhow!("temp dir: {e}"))?;
    extract_archive(&bytes, kind, tmp.path())?;
    let paths = discover_skills(tmp.path());
    if paths.is_empty() {
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
    Ok(previews)
}

/// Fetch + extract + discover + install. Clean skills always install; skills
/// with blocking findings install only if `accept_findings` is true.
/// Returns installed skill ids (`skills/<name>`).
///
/// Errors:
/// - If ALL skills were skipped for blocking findings (and none errored),
///   bails with an actionable message directing the caller to pass `--yes`.
/// - If ALL skills failed with errors (no blocking-skip), bails with the
///   joined error list.
/// - On PARTIAL success: returns `Ok(installed)` and writes a one-line
///   warning to stderr naming skipped-for-findings and errored counts.
pub async fn install_bundle_from_url(
    agent: &str,
    url: &str,
    accept_findings: bool,
) -> Result<Vec<String>> {
    let kind = is_archive_url(url).ok_or_else(|| anyhow::anyhow!("not a supported archive URL"))?;
    let bytes = fetch_bundle(url).await?;
    let tmp = tempfile::TempDir::new().map_err(|e| anyhow::anyhow!("temp dir: {e}"))?;
    extract_archive(&bytes, kind, tmp.path())?;
    let paths = discover_skills(tmp.path());
    if paths.is_empty() {
        bail!("no skills found in bundle");
    }
    let mut installed = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut skipped_for_findings: Vec<String> = Vec::new();
    for p in &paths {
        let text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{}: read failed: {e}", p.display()));
                continue;
            }
        };
        let preview = match preview_skill_text(&text, is_md_path(p)) {
            Ok(pv) => pv,
            Err(e) => {
                errors.push(format!("{}: preview failed: {e}", p.display()));
                continue;
            }
        };
        // Fail-closed: skip skills with blocking findings unless accepted.
        if preview.blocking && !accept_findings {
            skipped_for_findings.push(preview.name.clone());
            continue;
        }
        // Install directly from the already-extracted TempDir path — no
        // second temp-file copy needed. cmd_skill_add derives the install id
        // from the manifest `name` field, not the source filename.
        match super::skill::cmd_skill_add(agent, &p.to_string_lossy()) {
            Ok(_) => installed.push(format!("skills/{}", preview.name)),
            Err(e) => errors.push(format!("{}: install failed: {e}", preview.name)),
        }
    }

    if installed.is_empty() {
        // Distinguish the two all-empty cases for actionable messaging.
        if !skipped_for_findings.is_empty() && errors.is_empty() {
            bail!(
                "{} skill(s) skipped due to blocking security findings; \
                 re-run with --yes (CLI) or tick accept (Hub) to install them",
                skipped_for_findings.len()
            );
        } else if !skipped_for_findings.is_empty() {
            bail!(
                "{} skill(s) skipped due to blocking security findings \
                 (re-run with --yes to install); {} additional error(s):\n{}",
                skipped_for_findings.len(),
                errors.len(),
                errors.join("\n")
            );
        } else {
            bail!("no skills installed:\n{}", errors.join("\n"));
        }
    }

    // Partial success: some installed, some skipped/errored — warn on stderr.
    if !skipped_for_findings.is_empty() || !errors.is_empty() {
        eprintln!(
            "warning: bundle partially installed ({} ok, {} skipped for findings, {} error(s))",
            installed.len(),
            skipped_for_findings.len(),
            errors.len()
        );
    }

    Ok(installed)
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

    // ── Fix #4 ──────────────────────────────────────────────────────────────
    // tar.gz with a path-traversal entry → extract_archive must return Err.
    // Both `set_path` and `append_data` validate `..`, so we build the raw
    // 512-byte tar header block by hand, writing "../escape.txt" directly
    // into the name field, then append the data block manually.
    #[test]
    fn extract_targz_refuses_path_traversal() {
        use std::io::Write;
        // Build a minimal POSIX tar manually with a `..` path.
        // Tar header: 512 bytes. Name at [0..100], mode [100..108],
        // size [124..136] (octal), typeflag [156], checksum [148..156].
        let data = b"bad";
        let mut header = [0u8; 512];
        // name field
        header[..13].copy_from_slice(b"../escape.txt");
        // mode: "0000644\0"
        header[100..108].copy_from_slice(b"0000644\0");
        // uid, gid: "0000000\0"
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        // size: "00000000003\0" (3 bytes, octal)
        header[124..136].copy_from_slice(b"00000000003\0");
        // mtime: "00000000000\0"
        header[136..148].copy_from_slice(b"00000000000\0");
        // checksum placeholder (8 spaces for calculation)
        header[148..156].copy_from_slice(b"        ");
        // typeflag: '0' = regular file
        header[156] = b'0';
        // compute checksum
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        // write checksum in octal, null-terminated with space per POSIX
        let ck_str = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(ck_str.as_bytes());
        // data block (padded to 512)
        let mut data_block = [0u8; 512];
        data_block[..data.len()].copy_from_slice(data);
        // two end-of-archive zero blocks
        let end_blocks = [0u8; 1024];
        // assemble the raw tar
        let mut tar_buf = Vec::new();
        tar_buf.extend_from_slice(&header);
        tar_buf.extend_from_slice(&data_block);
        tar_buf.extend_from_slice(&end_blocks);
        // gzip-wrap it
        let mut gz = Vec::new();
        {
            use flate2::{Compression, write::GzEncoder};
            let mut e = GzEncoder::new(&mut gz, Compression::default());
            e.write_all(&tar_buf).unwrap();
            e.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let result = extract_archive(&gz, ArchiveKind::TarGz, dir.path());
        assert!(result.is_err(), "path traversal should be rejected");
    }

    // ── Fix #1 ──────────────────────────────────────────────────────────────
    // zip whose real uncompressed content exceeds BUNDLE_MAX_TOTAL_UNCOMPRESSED
    // → extract_archive must return Err (zip-bomb guard on actual bytes).
    //
    // Note: the `zip` crate's ZipWriter always writes honest stored-size
    // headers, so this test uses an honest zip where declared == actual.
    // The guard in extract_zip reads actual bytes via Read::take (not
    // entry.size()), so it correctly bounds real inflated data regardless
    // of what the header claims — a lying header would also be caught.
    #[test]
    fn extract_zip_rejects_zip_bomb() {
        use std::io::Write;
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            // 5 entries × 7 MB zeros = 35 MB > BUNDLE_MAX_TOTAL_UNCOMPRESSED (32 MB).
            // Zeros deflate to ~a few bytes, so the .zip is tiny and the test is fast.
            let chunk = vec![0u8; 7 * 1024 * 1024];
            for i in 0..5u32 {
                zw.start_file(format!("big{i}.bin"), opts).unwrap();
                zw.write_all(&chunk).unwrap();
            }
            zw.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let result = extract_archive(&buf, ArchiveKind::Zip, dir.path());
        assert!(result.is_err(), "zip bomb should be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("exceeds"),
            "error should mention size limit: {msg}"
        );
    }
}
