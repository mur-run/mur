# Quill skill bundles — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install a pack of one-or-more skills from a `.zip` or `.tar.gz`/`.tgz` URL — Hub + CLI — by safely extracting the archive, discovering every skill manifest, scanning each, and installing the clean ones (flagged ones need explicit acceptance).

**Architecture:** A new `mur-core` `skill_bundle.rs` adds archive detection, zip-slip/zip-bomb-safe extraction (`zip` crate + `tar`+`flate2`), multi-skill discovery, and a bundle preview/install built on quill P1's per-skill primitives. `install_skill_from_url` (P1) gains a front-door that routes archive URLs here. The Hub's preview/install commands unify to return a **list** of skills so one modal code path handles single skills and packs.

**Tech Stack:** Rust (`zip` v2, `tar`, `flate2` — all already mur-core deps), reusing quill P1 `skill_remote` + `cmd_skill_add` + `scan_skill`; Tauri 2; React/TS.

## Global Constraints

- Builds on quill P1 (branch `feat/quill-p1-skill-by-url`); this branch is `feat/quill-skill-bundles` stacked on it.
- No hardcoded values — caps are named consts. (CLAUDE.md rule 1)
- Single source file ≤ 800 lines. (rule 4)
- Rust edition 2024.
- Build/test mur-core: `ORT_STRATEGY=download`, plain `cargo test`; toolchain at `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin` on PATH + `RUSTC` if the rustup proxy is broken. **Run `cargo fmt --all` after Rust changes** (CI's stable rustfmt is strict — see the fmt-skew gotcha).
- Hub UI: `npx tsc --noEmit` + `npm run build` from `mur-hub-gui/ui` (symlink `node_modules` from the main checkout in a worktree). Hub `cargo check` needs `mur-hub-gui/ui/dist` (stub `index.html` if absent; gitignored). **Never commit the tracked 0-byte `mur-hub-gui/src-tauri/binaries/*` stubs as real binaries** — `git checkout --` them if a Hub build overwrote them.
- **Safe extraction is mandatory:** reject path-traversal (`..`/absolute/symlink), enforce entry-count + total-uncompressed caps; https-only; size-capped download.
- **Fail-closed:** a skill with blocking scan findings installs only on explicit acceptance.
- Changes apply on agent **restart** — surface in UI copy.
- All three archive deps already in `mur-core/Cargo.toml`: `tar = "0.4"`, `flate2 = "1"`, `zip = { version = "2", default-features = false, features = ["deflate"] }`. **No Cargo.toml change.**

## Reused existing API (verified, from quill P1 base)

- `skill_remote::SkillPreview { name, description, category, body, blocking, findings }` (Serialize).
- `skill_remote::preview_skill_text(text: &str, is_markdown: bool) -> Result<SkillPreview>`.
- `skill_remote::install_skill_from_url(agent, url, accept_findings) -> Result<String>` (returns `skills/<name>`).
- `skill_remote::{validate_skill_url, SKILL_MAX_BYTES, preview_skill_url}`.
- `cmd::agent::skill::cmd_skill_add(agent, source_path) -> Result<()>`.
- `mur_common::skill::{parse_canonical, parse_markdown}`.
- Hub: `agent_skill_preview_url(url) -> Result<SkillPreview, String>`, `agent_skill_install_url(name, url, accept_findings) -> Result<SkillInstallResult, String>`, `SkillInstallResult { detail, installed_id }`, `get_agent_detail`.
- `zip` API pattern (mur-core `update/release.rs`): `zip::ZipArchive::new(std::io::Cursor::new(bytes))` → `.by_index(i)` → `entry.enclosed_name()` (None ⇒ unsafe path).

---

## File Structure

- **Create** `mur-core/src/cmd/agent/skill_bundle.rs` — archive detection, safe extraction, discovery, bundle preview/install.
- **Modify** `mur-core/src/cmd/agent/mod.rs` — `pub mod skill_bundle;`.
- **Modify** `mur-core/src/cmd/agent/skill_remote.rs` — add `preview_any_url`/`install_any_url` routing helpers; route archives in `install_skill_from_url`.
- **Modify** `mur-hub-gui/src-tauri/src/mcp_skills.rs` — preview returns `Vec<SkillPreview>`; install returns a multi-id result.
- **Modify** `mur-hub-gui/ui/src/components/SkillAddUrlModal.tsx` — render a skill **list** + one bundle-level accept.
- **Modify** `mur-hub-gui/ui/src/i18n/{en,zh-TW}.ts` — pluralized strings.

---

## Task 1: Archive detection + caps (mur-core)

**Files:** Create `mur-core/src/cmd/agent/skill_bundle.rs`; Modify `mur-core/src/cmd/agent/mod.rs`.

**Interfaces:**
- Produces: `pub enum ArchiveKind { Zip, TarGz }`; `pub fn is_archive_url(url: &str) -> Option<ArchiveKind>`; consts `BUNDLE_MAX_BYTES`, `BUNDLE_MAX_ENTRIES`, `BUNDLE_MAX_TOTAL_UNCOMPRESSED`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_archive_extensions() {
        assert!(matches!(is_archive_url("https://x.com/p.zip"), Some(ArchiveKind::Zip)));
        assert!(matches!(is_archive_url("https://x.com/p.tar.gz"), Some(ArchiveKind::TarGz)));
        assert!(matches!(is_archive_url("https://x.com/p.tgz"), Some(ArchiveKind::TarGz)));
        assert!(is_archive_url("https://x.com/skill.yaml").is_none());
        assert!(is_archive_url("https://x.com/skill.md").is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle::tests::detects_archive`
Expected: FAIL — items not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Install a pack of one-or-more skills from an archive URL (.zip / .tar.gz).
//! Safely extracts (path-traversal + size caps), discovers every skill manifest,
//! and installs the clean ones via quill P1's per-skill path. Built on
//! `skill_remote`.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Supported skill-bundle archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
}

/// Max bytes downloaded for a bundle.
pub const BUNDLE_MAX_BYTES: usize = 5 * 1024 * 1024;
/// Max number of entries an archive may contain.
pub const BUNDLE_MAX_ENTRIES: usize = 256;
/// Max total uncompressed bytes written during extraction (zip-bomb guard).
pub const BUNDLE_MAX_TOTAL_UNCOMPRESSED: u64 = 32 * 1024 * 1024;

/// Classify a URL by archive extension. `None` = not an archive we handle.
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
```

Add to `mur-core/src/cmd/agent/mod.rs`: `pub mod skill_bundle;`

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle::tests::detects_archive`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_bundle.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(skill): archive detection + bundle caps"
```

---

## Task 2: Safe extraction (mur-core, network-free)

**Files:** Modify `mur-core/src/cmd/agent/skill_bundle.rs`.

**Interfaces:**
- Consumes: Task 1 `ArchiveKind`, caps.
- Produces: `pub fn extract_archive(bytes: &[u8], kind: ArchiveKind, dest: &Path) -> Result<()>` — extracts into `dest`, rejecting path-traversal/symlinks and over-cap archives.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn extract_zip_rejects_traversal_and_writes_safe_files() {
    use std::io::Write;
    // Build an in-memory zip with one safe file and one ../escape entry.
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        w.start_file("ok/skill.yaml", opts).unwrap();
        w.write_all(b"name: x").unwrap();
        w.start_file("../escape.txt", opts).unwrap();
        w.write_all(b"evil").unwrap();
        w.finish().unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    extract_archive(&buf, ArchiveKind::Zip, dir.path()).unwrap();
    assert!(dir.path().join("ok/skill.yaml").exists());
    // the traversal entry must NOT have been written outside dest
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle::tests::extract_`
Expected: FAIL — `extract_archive` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
use std::io::Read;

/// Extract `bytes` (a `kind` archive) into `dest`, safely. Rejects entries that
/// escape `dest` (`..`/absolute), symlinks, and archives exceeding the entry or
/// total-uncompressed caps. Directories are created; only regular files written.
pub fn extract_archive(bytes: &[u8], kind: ArchiveKind, dest: &Path) -> Result<()> {
    match kind {
        ArchiveKind::Zip => extract_zip(bytes, dest),
        ArchiveKind::TarGz => extract_targz(bytes, dest),
    }
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| anyhow::anyhow!("open zip: {e}"))?;
    if zip.len() > BUNDLE_MAX_ENTRIES {
        bail!("archive has too many entries ({} > {BUNDLE_MAX_ENTRIES})", zip.len());
    }
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| anyhow::anyhow!("zip entry {i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        // enclosed_name() returns None for traversal/absolute/unsafe paths.
        let rel = match entry.enclosed_name() {
            Some(p) => p,
            None => bail!("unsafe path in archive: {}", entry.name()),
        };
        total = total.saturating_add(entry.size());
        if total > BUNDLE_MAX_TOTAL_UNCOMPRESSED {
            bail!("archive uncompressed size exceeds {BUNDLE_MAX_TOTAL_UNCOMPRESSED} bytes");
        }
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("mkdir: {e}"))?;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| anyhow::anyhow!("read entry: {e}"))?;
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
        let unpacked = entry
            .unpack_in(dest)
            .map_err(|e| anyhow::anyhow!("unpack tar entry: {e}"))?;
        if !unpacked {
            bail!("unsafe path in archive (refused by unpack_in)");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle::tests::extract_`
Expected: PASS (both tests). (If `tempfile`/`zip`/`tar`/`flate2` aren't already dev-available, they are normal deps — confirm `use` paths compile.)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_bundle.rs
git commit -m "feat(skill): zip-slip + zip-bomb-safe archive extraction (zip + tar.gz)"
```

---

## Task 3: Skill discovery (mur-core, network-free)

**Files:** Modify `mur-core/src/cmd/agent/skill_bundle.rs`.

**Interfaces:**
- Produces: `pub fn discover_skills(dir: &Path) -> Vec<PathBuf>` — every file under `dir` that parses as a skill (`.yaml`/`.yml` via `parse_canonical`, `.md`/`.markdown` via `parse_markdown`), sorted for determinism.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn discovers_nested_skills_and_ignores_non_skills() {
    let dir = tempfile::tempdir().unwrap();
    let valid = "name: a\nversion: 1.0.0\npublisher: human:test\ndescription: d\ncategory: context\ncontent:\n  abstract: x\n  context: y\n";
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/skill.yaml"), valid).unwrap();
    std::fs::write(dir.path().join("README.md"), "# just docs, not a skill").unwrap();
    let found = discover_skills(dir.path());
    assert_eq!(found.len(), 1);
    assert!(found[0].ends_with("sub/skill.yaml"));
}
```

(Adjust the `valid` fixture to a minimal manifest that the real schema accepts — confirm required fields in `mur-common/src/skill/`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle::tests::discovers_nested`
Expected: FAIL — `discover_skills` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Walk `dir` recursively; return paths to every file that parses as a skill.
/// Non-skill files (READMEs, licenses, assets) are silently ignored.
pub fn discover_skills(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_skills(dir, &mut out);
    out.sort();
    out
}

fn walk_skills(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            walk_skills(&p, out);
        } else if file_parses_as_skill(&p) {
            out.push(p);
        }
    }
}

fn file_parses_as_skill(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    match ext.as_str() {
        "yaml" | "yml" => mur_common::skill::parse_canonical(&text).is_ok(),
        "md" | "markdown" => mur_common::skill::parse_markdown(&text).is_ok(),
        _ => false,
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle::tests::discovers_nested`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_bundle.rs
git commit -m "feat(skill): discover_skills walks an extracted bundle"
```

---

## Task 4: Bundle preview + install (mur-core)

**Files:** Modify `mur-core/src/cmd/agent/skill_bundle.rs`.

**Interfaces:**
- Consumes: Tasks 1-3; `skill_remote::{SkillPreview, preview_skill_text, SKILL_MAX_BYTES, validate_skill_url}`; `cmd::agent::skill::cmd_skill_add`.
- Produces:
  - `pub async fn fetch_bundle(url: &str, kind: ArchiveKind) -> Result<Vec<u8>>` (size-capped download).
  - `pub async fn preview_bundle_url(url: &str) -> Result<Vec<SkillPreview>>` — extract → discover → preview each. Installs nothing.
  - `pub async fn install_bundle_from_url(agent: &str, url: &str, accept_findings: bool) -> Result<Vec<String>>` — installs clean skills; blocked skills only with `accept_findings`. Returns installed ids.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn extract_then_discover_then_preview_is_consistent() {
    // Build a tar.gz with one clean skill, extract, discover, preview.
    use std::io::Write;
    let skill = "name: packed\nversion: 1.0.0\npublisher: human:test\ndescription: d\ncategory: context\ncontent:\n  abstract: x\n  context: y\n";
    let mut tarb = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tarb);
        let mut h = tar::Header::new_gnu();
        h.set_size(skill.len() as u64); h.set_mode(0o644); h.set_cksum();
        b.append_data(&mut h, "skill.yaml", skill.as_bytes()).unwrap();
        b.finish().unwrap();
    }
    let mut gz = Vec::new();
    { use flate2::{Compression, write::GzEncoder}; let mut e = GzEncoder::new(&mut gz, Compression::default()); e.write_all(&tarb).unwrap(); e.finish().unwrap(); }
    let dir = tempfile::tempdir().unwrap();
    extract_archive(&gz, ArchiveKind::TarGz, dir.path()).unwrap();
    let skills = discover_skills(dir.path());
    assert_eq!(skills.len(), 1);
    let text = std::fs::read_to_string(&skills[0]).unwrap();
    let p = crate::cmd::agent::skill_remote::preview_skill_text(&text, false).unwrap();
    assert_eq!(p.name, "packed");
    assert!(!p.blocking);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle::tests::extract_then_discover`
Expected: FAIL until the module compiles with the new fns (the test itself uses only Tasks 1-3 + preview_skill_text; it pins the pipeline).

- [ ] **Step 3: Write minimal implementation**

```rust
use crate::cmd::agent::skill_remote::{SkillPreview, preview_skill_text};

/// Download an archive, size-capped (Content-Length pre-check + post-download).
pub async fn fetch_bundle(url: &str, _kind: ArchiveKind) -> Result<Vec<u8>> {
    let url = crate::cmd::agent::skill_remote::validate_skill_url(url)?;
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("fetch failed: {e}"))?;
    if !resp.status().is_success() {
        bail!("server returned {}", resp.status());
    }
    if let Some(len) = resp.content_length()
        && len > BUNDLE_MAX_BYTES as u64
    {
        bail!("bundle too large ({len} bytes; max {BUNDLE_MAX_BYTES})");
    }
    let bytes = resp.bytes().await.map_err(|e| anyhow::anyhow!("read body: {e}"))?;
    if bytes.len() > BUNDLE_MAX_BYTES {
        bail!("bundle too large ({} bytes; max {BUNDLE_MAX_BYTES})", bytes.len());
    }
    Ok(bytes.to_vec())
}

fn is_md_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str(),
        "md" | "markdown"
    )
}

/// Fetch + extract + discover + preview every skill in a bundle. Installs nothing.
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

/// Fetch + extract + discover + install. Clean skills always install; skills with
/// blocking findings install only when `accept_findings` is true. Returns ids.
pub async fn install_bundle_from_url(
    agent: &str,
    url: &str,
    accept_findings: bool,
) -> Result<Vec<String>> {
    let kind = is_archive_url(url).ok_or_else(|| anyhow::anyhow!("not a supported archive URL"))?;
    let bytes = fetch_bundle(url, kind).await?;
    let tmp = tempdir_unique("mur-bundle")?;
    let run = (|| -> Result<Vec<String>> {
        extract_archive(&bytes, kind, &tmp)?;
        let paths = discover_skills(&tmp);
        if paths.is_empty() {
            bail!("no skills found in bundle");
        }
        let mut installed = Vec::new();
        for p in &paths {
            let text = std::fs::read_to_string(p)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", p.display()))?;
            let pv = preview_skill_text(&text, is_md_path(p))?;
            if pv.blocking && !accept_findings {
                continue; // fail-closed: skip flagged skills unless accepted
            }
            crate::cmd::agent::skill::cmd_skill_add(agent, &p.to_string_lossy())?;
            installed.push(format!("skills/{}", pv.name));
        }
        Ok(installed)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    run
}

/// Create a unique temp directory; caller removes it.
fn tempdir_unique(prefix: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("mkdir temp: {e}"))?;
    Ok(dir)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_bundle -- --nocapture`
Expected: PASS (all skill_bundle tests). Then `cargo clippy -p mur-core -- -D warnings` clean; `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_bundle.rs
git commit -m "feat(skill): bundle fetch + preview + fail-closed install"
```

---

## Task 5: Route archives + unified helpers (mur-core)

**Files:** Modify `mur-core/src/cmd/agent/skill_remote.rs`.

**Interfaces:**
- Produces:
  - `pub async fn preview_any_url(url: &str) -> Result<Vec<SkillPreview>>` — bundle → `preview_bundle_url`; else `vec![preview_skill_url]`.
  - `pub async fn install_any_url(agent: &str, url: &str, accept_findings: bool) -> Result<Vec<String>>` — bundle → `install_bundle_from_url`; else `vec![install_skill_from_url]`.
- Modifies: `install_skill_from_url` gains a front-door so the CLI transparently routes archives.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn archive_urls_route_to_bundle() {
    use crate::cmd::agent::skill_bundle::{ArchiveKind, is_archive_url};
    assert!(matches!(is_archive_url("https://x/p.zip"), Some(ArchiveKind::Zip)));
    assert!(is_archive_url("https://x/skill.yaml").is_none());
    // (preview_any_url/install_any_url routing is exercised by the bundle tests +
    // the gated network test; this asserts the discriminator the router uses.)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_remote::tests::archive_urls_route`
Expected: FAIL — import path not yet valid until the helpers + use exist.

- [ ] **Step 3: Write minimal implementation**

In `skill_remote.rs`, at the top of `install_skill_from_url` (before `fetch_skill`):

```rust
    if let Some(_kind) = crate::cmd::agent::skill_bundle::is_archive_url(url) {
        let ids = crate::cmd::agent::skill_bundle::install_bundle_from_url(agent, url, accept_findings).await?;
        return Ok(ids.join(", "));
    }
```

Add the unified helpers (used by the Hub):

```rust
/// Preview a URL whether it's a single skill or an archive of many. Returns one
/// preview per skill (length 1 for a single skill).
pub async fn preview_any_url(url: &str) -> Result<Vec<SkillPreview>> {
    if crate::cmd::agent::skill_bundle::is_archive_url(url).is_some() {
        crate::cmd::agent::skill_bundle::preview_bundle_url(url).await
    } else {
        Ok(vec![preview_skill_url(url).await?])
    }
}

/// Install a single skill or a bundle of many. Returns the installed skill ids.
pub async fn install_any_url(agent: &str, url: &str, accept_findings: bool) -> Result<Vec<String>> {
    if crate::cmd::agent::skill_bundle::is_archive_url(url).is_some() {
        crate::cmd::agent::skill_bundle::install_bundle_from_url(agent, url, accept_findings).await
    } else {
        Ok(vec![install_skill_from_url(agent, url, accept_findings).await?])
    }
}
```

- [ ] **Step 4: Run test + build to verify**

Run: `ORT_STRATEGY=download cargo test -p mur-core skill_remote:: -- --nocapture` then `cargo clippy -p mur-core -- -D warnings` and `cargo fmt --all`.
Expected: tests PASS; clippy clean. (CLI `mur agent skill add-url <agent> <archive-url>` now installs bundles via the front-door — the dispatch already prints the returned string; for a bundle it lists the joined ids.)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_remote.rs
git commit -m "feat(skill): route archive URLs to bundle install + preview_any/install_any"
```

---

## Task 6: Hub commands return a skill list (Hub backend)

**Files:** Modify `mur-hub-gui/src-tauri/src/mcp_skills.rs`, `lib.rs` (registration is unchanged — same command names).

**Interfaces:**
- Changes `agent_skill_preview_url` → `Result<Vec<SkillPreview>, String>`.
- Changes `agent_skill_install_url` → `Result<BundleInstallResult, String>` where `pub struct BundleInstallResult { detail: AgentDetail, installed_ids: Vec<String> }`.
- Consumes: Task 5 `preview_any_url`, `install_any_url`.

- [ ] **Step 1: Edit the commands**

```rust
use mur_core::cmd::agent::skill_remote::{SkillPreview, install_any_url, preview_any_url};

#[derive(Debug, Clone, serde::Serialize)]
pub struct BundleInstallResult {
    pub detail: AgentDetail,
    pub installed_ids: Vec<String>,
}

/// Fetch + parse + scan a skill URL (single skill or archive of many). Installs
/// nothing. Returns one preview per discovered skill.
#[tauri::command]
pub async fn agent_skill_preview_url(url: String) -> Result<Vec<SkillPreview>, String> {
    preview_any_url(&url).await.map_err(|e| format!("{e:#}"))
}

/// Install a skill or bundle from a URL. `accept_findings` gates skills with
/// blocking scan findings (clean skills install regardless).
#[tauri::command]
pub async fn agent_skill_install_url(
    name: String,
    url: String,
    accept_findings: bool,
) -> Result<BundleInstallResult, String> {
    let installed_ids = install_any_url(&name, &url, accept_findings)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let detail = get_agent_detail(name)?;
    Ok(BundleInstallResult { detail, installed_ids })
}
```

(Remove the now-unused single-`SkillPreview` import if the compiler flags it; keep `SkillPreview` since the return type uses it.)

- [ ] **Step 2: Verify it compiles**

Run: `ORT_STRATEGY=download CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/src-tauri/target cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: compiles (stub `mur-hub-gui/ui/dist/index.html` if needed; don't commit it).

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): skill preview/install URL return a list (single or bundle)"
```

---

## Task 7: Modal renders the skill list (Hub UI)

**Files:** Modify `mur-hub-gui/ui/src/components/SkillAddUrlModal.tsx`; i18n in `en.ts`/`zh-TW.ts`.

**Interfaces:** Consumes `agent_skill_preview_url` (now `SkillPreview[]`) and `agent_skill_install_url` (now `{ detail, installed_ids }`).

- [ ] **Step 1: Update the component**

Change the preview state to a list and render each skill. Replace the single-preview block with:

```tsx
  const [previews, setPreviews] = useState<SkillPreview[] | null>(null);
  const [accept, setAccept] = useState(false);
  // ...
  async function fetchPreview() {
    setError(null); setPreviews(null); setAccept(false);
    const trimmed = url.trim();
    if (!(trimmed.startsWith("https://") || trimmed.startsWith("http://localhost") || trimmed.startsWith("http://127.0.0.1") || trimmed.startsWith("http://[::1]"))) {
      setError(t("skillurl.invalidUrl")); return;
    }
    setBusy("fetch");
    try { setPreviews(await invoke<SkillPreview[]>("agent_skill_preview_url", { url: trimmed })); }
    catch (e) { setError(String(e)); } finally { setBusy(null); }
  }
  async function install() {
    setError(null); setBusy("install");
    try {
      const res = await invoke<{ detail: AgentDetail; installed_ids: string[] }>(
        "agent_skill_install_url",
        { name: agentName, url: url.trim(), acceptFindings: accept },
      );
      onSaved(res.detail);
      onClose();
    } catch (e) { setError(String(e)); } finally { setBusy(null); }
  }
  const anyBlocking = !!previews && previews.some((p) => p.blocking);
  const canInstall = !!previews && previews.length > 0 && busy === null && (!anyBlocking || accept);
```

Render the list (replacing the single-skill block):

```tsx
          {previews && previews.map((p, i) => (
            <div key={i} className="item-card" style={{ marginTop: 8 }}>
              <div className="item-card-name">{p.name} <span className="badge-sm">{p.category}</span></div>
              <code className="item-card-code">{p.description}</code>
              {p.findings.length > 0 && (
                <ul className="item-list">
                  {p.findings.map((f, j) => (<li key={j} className="save-error">{f}</li>))}
                </ul>
              )}
              <pre className="item-card-code" style={{ whiteSpace: "pre-wrap", maxHeight: 160, overflow: "auto" }}>{p.body}</pre>
            </div>
          ))}
          {anyBlocking && (
            <label style={{ display: "block", marginTop: 6 }}>
              <input type="checkbox" checked={accept} onChange={(e) => setAccept(e.target.checked)} />{" "}
              {t("skillurl.accept")}
            </label>
          )}
```

- [ ] **Step 2: i18n**

In both `en.ts` and `zh-TW.ts`, the existing `skillurl.*` keys still apply; add `skillurl.bundleHint` (en: "An archive may contain several skills — review each below."; zh-TW: "壓縮檔可能包含多個技能 — 請逐一檢視。") and show it when `previews && previews.length > 1`.

- [ ] **Step 3: Typecheck + build**

Run: `cd mur-hub-gui/ui && npx tsc --noEmit && npm run build`
Expected: tsc exit 0; vite build succeeds.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/SkillAddUrlModal.tsx mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): skill-add modal renders a list (single skill or bundle)"
```

- [ ] **Step 5: Live verify (manual)**

Rebuild + install the Hub (`gotcha_hub_local_app_build_recipe`). Serve a 2-skill `.tar.gz` + a `.zip` with one clean + one injection skill over `http://127.0.0.1`. In the Hub Skills tab → Install from URL → paste each archive URL → confirm the list renders all skills; the injection one shows findings + the install is gated until the accept checkbox is ticked; clean ones install.

---

## Self-Review

**Spec coverage:** archive formats (.zip + .tar.gz/.tgz) → Task 1 ✓; safe extraction (zip-slip + caps) → Task 2 ✓; discover 1+ skills any depth → Task 3 ✓; fetch size-cap + preview + fail-closed install → Task 4 ✓; route archives in install_skill_from_url + unified preview/install → Task 5 ✓; Hub list-returning commands → Task 6 ✓; multi-skill consent UI → Task 7 ✓. Resolves quill P1 §10a confusing-`.zip` (now installs). Non-goals (signing, assets, registry) excluded.

**Placeholder scan:** Two implementer-notes point at exact existing files to confirm against (the discover-test fixture schema; removing an unused import) — both name the file. No "TBD/handle errors/add validation" placeholders.

**Type consistency:** `ArchiveKind`/`is_archive_url`/caps (Task 1) consumed in Tasks 2,4,5. `extract_archive(bytes, kind, dest)` (Task 2) called in Task 4. `discover_skills(dir) -> Vec<PathBuf>` (Task 3) used in Task 4. `preview_bundle_url -> Vec<SkillPreview>` + `install_bundle_from_url -> Vec<String>` (Task 4) wrapped by `preview_any_url`/`install_any_url` (Task 5), surfaced by the Hub commands (Task 6, `Vec<SkillPreview>` / `BundleInstallResult { detail, installed_ids }`), consumed by the modal (Task 7, `SkillPreview[]` / `{ detail, installed_ids }`). `acceptFindings`→`accept_findings` camelCase mapping consistent with quill P1.
