# MUR Install Simplification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce install friction for MUR CLI by adding a `mur update` self-updater, a curl-pipe installer (`install.sh` / `install.ps1`), signed macOS DMG+PKG installers, and crates.io publishing — all driven from the same GitHub Releases assets.

**Architecture:** All install methods fetch from GitHub Releases (single source of truth). `mur update` lives inside the existing `mur-core` binary as a new subcommand backed by a small `update` module. The shell installers are static scripts deployed to GitHub Pages (`mur.run` CNAME). The DMG/PKG and crates.io publishing are added as new jobs in the existing `release.yml` workflow.

**Tech Stack:** Rust (edition 2024) using `reqwest` (blocking + rustls), `sha2`, `serde_json`, `semver`; POSIX `sh` for `install.sh`; PowerShell 5+ for `install.ps1`; GitHub Actions (`apple-actions/import-codesign-certs`, `pkgbuild`, `productsign`, `hdiutil`, `codesign`, `xcrun notarytool`, `xcrun stapler`); crates.io tokens.

---

## File Structure

**New files:**
- `mur-core/src/update/mod.rs` — public entry point, orchestrates the update flow (~80 lines)
- `mur-core/src/update/release.rs` — GitHub Releases API client + asset selection + 5-min cache (~120 lines)
- `mur-core/src/update/source.rs` — install-source detection (brew / cargo / other) (~60 lines)
- `mur-core/src/update/swap.rs` — atomic binary swap: Unix `rename` + Windows PowerShell helper (~80 lines)
- `mur-core/src/cmd/update.rs` — CLI entry point wired to `crate::update` (~40 lines)
- `scripts/install.sh` — POSIX shell installer (~150 lines)
- `scripts/install.ps1` — PowerShell installer (~120 lines)
- `tests/update_integration.rs` (in `mur-core`) — integration test using the real `/releases/latest` endpoint (gated by `MUR_UPDATE_NETWORK_TESTS=1`)

**Modified files:**
- `mur-core/src/lib.rs` — add `pub mod update;`
- `mur-core/src/cmd/mod.rs` — register `pub(crate) mod update;`
- `mur-core/src/cli/mod.rs` — add `Update { check: bool }` variant to `Commands`
- `mur-core/src/dispatch.rs` — handle `Commands::Update`
- `mur-core/Cargo.toml` — add `semver`, `keywords`, `categories`, switch `mur-common` to versioned dep
- `mur-common/Cargo.toml` — add `keywords`, `categories`
- `.github/workflows/release.yml` — add `package-macos`, `deploy-installer`, `publish-crates` jobs
- `README.md` — replace install section with new options

**Boundaries:** Network I/O lives in `update/release.rs` and `update/swap.rs` (Windows helper). Pure logic (version compare, asset matching, source detection, OS/arch mapping) is isolated and unit-tested without network. CLI parsing stays in `cli/mod.rs`; the `cmd/update.rs` shim only translates clap args into `update::run(...)` calls so the library is reusable.

---

## Task 1: Scaffold the `update` module

**Files:**
- Create: `mur-core/src/update/mod.rs`
- Create: `mur-core/src/update/release.rs`
- Create: `mur-core/src/update/source.rs`
- Create: `mur-core/src/update/swap.rs`
- Modify: `mur-core/src/lib.rs`
- Modify: `mur-core/Cargo.toml`

- [ ] **Step 1: Add `semver` dependency to `mur-core/Cargo.toml`**

Locate the `[dependencies]` block in `mur-core/Cargo.toml` (around line 16) and add:

```toml
semver = "1"
```

after the existing `regex = "1"` line.

- [ ] **Step 2: Create the module skeleton files**

Create `mur-core/src/update/mod.rs`:

```rust
//! `mur update` self-update implementation.
//!
//! The update flow is driven by `run()`. Network and platform-specific code is
//! split into submodules to keep each file under the 800-line rule and to make
//! unit testing possible.

pub mod release;
pub mod source;
pub mod swap;

use anyhow::Result;

#[derive(Debug, Clone, Copy)]
pub struct UpdateOptions {
    pub check_only: bool,
}

pub fn run(_opts: UpdateOptions) -> Result<()> {
    anyhow::bail!("not yet implemented")
}
```

Create `mur-core/src/update/release.rs`:

```rust
//! GitHub Releases API client used by `mur update`.
```

Create `mur-core/src/update/source.rs`:

```rust
//! Detect how `mur` was installed (Homebrew, cargo install, other).
```

Create `mur-core/src/update/swap.rs`:

```rust
//! Atomically replace the running `mur` binary with a newly downloaded one.
```

- [ ] **Step 3: Register `update` in the lib root**

In `mur-core/src/lib.rs`, find the existing `pub mod ...` declarations and add:

```rust
pub mod update;
```

(alphabetical placement is fine; just add it as a sibling of the other top-level modules).

- [ ] **Step 4: Build to make sure it compiles**

Run: `cargo build -p mur-core`
Expected: clean build, no warnings about unused `update` module (it is `pub mod`).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/update/ mur-core/src/lib.rs mur-core/Cargo.toml
git commit -m "feat(update): scaffold update module"
```

---

## Task 2: Version comparison logic

**Files:**
- Modify: `mur-core/src/update/release.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/update/release.rs`:

```rust
/// Strip a leading `v` (or `V`) from a git-tag-style version string.
pub fn strip_v_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').or_else(|| tag.strip_prefix('V')).unwrap_or(tag)
}

/// Returns `true` when `latest` is strictly newer than `current` per semver.
pub fn is_newer(current: &str, latest: &str) -> anyhow::Result<bool> {
    let cur = semver::Version::parse(strip_v_prefix(current))?;
    let lat = semver::Version::parse(strip_v_prefix(latest))?;
    Ok(lat > cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_v_prefix_handles_both_cases() {
        assert_eq!(strip_v_prefix("v1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("V1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("1.2.3"), "1.2.3");
    }

    #[test]
    fn is_newer_detects_patch_bump() {
        assert!(is_newer("2.16.0", "v2.16.1").unwrap());
    }

    #[test]
    fn is_newer_rejects_same_version() {
        assert!(!is_newer("2.16.1", "v2.16.1").unwrap());
    }

    #[test]
    fn is_newer_rejects_older() {
        assert!(!is_newer("2.16.1", "v2.16.0").unwrap());
    }

    #[test]
    fn is_newer_errors_on_garbage() {
        assert!(is_newer("nope", "v1.0.0").is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p mur-core --lib update::release::tests`
Expected: compile error (semver not yet in scope) or test failures.

- [ ] **Step 3: Make tests pass**

The code in Step 1 *is* the implementation. If tests fail because `semver` isn't picked up, re-run `cargo build -p mur-core` to force dep resolution.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib update::release::tests`
Expected: all five tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/update/release.rs
git commit -m "feat(update): version comparison helpers"
```

---

## Task 3: Asset selection by OS/arch

**Files:**
- Modify: `mur-core/src/update/release.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/update/release.rs`:

```rust
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

/// Returns the expected asset filename for the current host platform, or `None`
/// if no prebuilt binary is published for it.
pub fn asset_name_for_host() -> Option<&'static str> {
    asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn asset_name_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("mur-aarch64-apple-darwin.tar.gz"),
        ("linux", "x86_64") => Some("mur-x86_64-unknown-linux-gnu.tar.gz"),
        ("windows", "x86_64") => Some("mur-x86_64-pc-windows-msvc.zip"),
        _ => None,
    }
}

/// Find the asset matching `name` in a release; returns the asset or an error.
pub fn select_asset<'a>(release: &'a Release, name: &str) -> anyhow::Result<&'a ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| anyhow::anyhow!("no asset named {name} in release {}", release.tag_name))
}

#[cfg(test)]
mod asset_tests {
    use super::*;

    fn fake_release() -> Release {
        Release {
            tag_name: "v2.17.0".into(),
            assets: vec![
                ReleaseAsset {
                    name: "mur-aarch64-apple-darwin.tar.gz".into(),
                    browser_download_url: "https://example/mac.tgz".into(),
                },
                ReleaseAsset {
                    name: "mur-x86_64-unknown-linux-gnu.tar.gz".into(),
                    browser_download_url: "https://example/lnx.tgz".into(),
                },
                ReleaseAsset {
                    name: "checksums.txt".into(),
                    browser_download_url: "https://example/c.txt".into(),
                },
            ],
        }
    }

    #[test]
    fn maps_known_targets() {
        assert_eq!(
            asset_name_for("macos", "aarch64"),
            Some("mur-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(
            asset_name_for("linux", "x86_64"),
            Some("mur-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            asset_name_for("windows", "x86_64"),
            Some("mur-x86_64-pc-windows-msvc.zip")
        );
    }

    #[test]
    fn returns_none_for_unsupported() {
        assert_eq!(asset_name_for("linux", "aarch64"), None);
        assert_eq!(asset_name_for("macos", "x86_64"), None);
    }

    #[test]
    fn select_asset_finds_match() {
        let r = fake_release();
        let a = select_asset(&r, "mur-aarch64-apple-darwin.tar.gz").unwrap();
        assert_eq!(a.browser_download_url, "https://example/mac.tgz");
    }

    #[test]
    fn select_asset_errors_when_missing() {
        let r = fake_release();
        assert!(select_asset(&r, "mur-aarch64-unknown-linux-gnu.tar.gz").is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail then pass**

Run: `cargo test -p mur-core --lib update::release::asset_tests`
Expected: all four tests pass (the implementation is included).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/update/release.rs
git commit -m "feat(update): asset selection by OS/arch"
```

---

## Task 4: Install source detection

**Files:**
- Modify: `mur-core/src/update/source.rs`

- [ ] **Step 1: Write the failing test**

Replace the contents of `mur-core/src/update/source.rs` with:

```rust
//! Detect how `mur` was installed.

use std::process::Command;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InstallSource {
    Homebrew,
    Cargo,
    Other,
}

impl InstallSource {
    /// Human-readable upgrade instruction, or `None` if self-update should run.
    pub fn upgrade_hint(self) -> Option<&'static str> {
        match self {
            InstallSource::Homebrew => Some("Installed via Homebrew. Run: brew upgrade mur"),
            InstallSource::Cargo => {
                Some("Installed via cargo. Run: cargo install mur --force")
            }
            InstallSource::Other => None,
        }
    }
}

/// Detect by querying system package managers. Lives behind a small layer so
/// tests can inject fake command outputs via [`detect_from_outputs`].
pub fn detect() -> InstallSource {
    let brew = Command::new("brew").args(["list", "mur"]).output().ok();
    let cargo = Command::new("cargo").args(["install", "--list"]).output().ok();

    detect_from_outputs(
        brew.as_ref().map(|o| (o.status.success(), o.stdout.as_slice())),
        cargo.as_ref().map(|o| (o.status.success(), o.stdout.as_slice())),
    )
}

pub fn detect_from_outputs(
    brew: Option<(bool, &[u8])>,
    cargo: Option<(bool, &[u8])>,
) -> InstallSource {
    if let Some((true, _)) = brew {
        return InstallSource::Homebrew;
    }
    if let Some((true, stdout)) = cargo {
        let s = std::str::from_utf8(stdout).unwrap_or("");
        if s.lines().any(|l| l.starts_with("mur ") || l.starts_with("mur-core ")) {
            return InstallSource::Cargo;
        }
    }
    InstallSource::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brew_success_wins() {
        let s = detect_from_outputs(Some((true, b"mur")), Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Homebrew);
    }

    #[test]
    fn cargo_when_brew_absent_or_failed() {
        let s = detect_from_outputs(Some((false, b"")), Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Cargo);
        let s = detect_from_outputs(None, Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Cargo);
    }

    #[test]
    fn cargo_list_must_mention_mur() {
        let s = detect_from_outputs(None, Some((true, b"ripgrep v14.0.0:\n")));
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn other_when_both_missing() {
        let s = detect_from_outputs(None, None);
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn hints_are_shaped() {
        assert!(InstallSource::Homebrew.upgrade_hint().unwrap().contains("brew upgrade"));
        assert!(InstallSource::Cargo.upgrade_hint().unwrap().contains("cargo install"));
        assert!(InstallSource::Other.upgrade_hint().is_none());
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p mur-core --lib update::source::tests`
Expected: all five tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/update/source.rs
git commit -m "feat(update): install source detection"
```

---

## Task 5: GitHub Releases API client with 5-minute cache

**Files:**
- Modify: `mur-core/src/update/release.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/update/release.rs`:

```rust
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const LATEST_URL: &str = "https://api.github.com/repos/mur-run/mur/releases/latest";
const CACHE_TTL: Duration = Duration::from_secs(300);
const USER_AGENT: &str = concat!("mur-update/", env!("CARGO_PKG_VERSION"));

fn cache_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".mur").join("update-cache.json"))
}

/// Read the cached release if it is still fresh.
pub fn read_cached_release(path: &std::path::Path, now: SystemTime) -> Option<Release> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    if now.duration_since(modified).ok()? > CACHE_TTL {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Release>(&bytes).ok()
}

pub fn write_cached_release(path: &std::path::Path, release: &Release) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec(release)?)?;
    Ok(())
}

/// Fetch the latest release, using a 5-minute cache to avoid rate limits.
pub fn fetch_latest() -> anyhow::Result<Release> {
    let cache = cache_path();
    if let Some(p) = cache.as_ref() {
        if let Some(r) = read_cached_release(p, SystemTime::now()) {
            return Ok(r);
        }
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client.get(LATEST_URL).send()?;
    if resp.status() == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!(
            "GitHub API rate limit reached. Try again in a few minutes."
        );
    }
    let release: Release = resp.error_for_status()?.json()?;
    if let Some(p) = cache.as_ref() {
        let _ = write_cached_release(p, &release);
    }
    Ok(release)
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn release_fixture() -> Release {
        Release {
            tag_name: "v9.9.9".into(),
            assets: vec![],
        }
    }

    #[test]
    fn round_trips_cache_within_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        write_cached_release(&path, &release_fixture()).unwrap();
        let got = read_cached_release(&path, SystemTime::now()).unwrap();
        assert_eq!(got.tag_name, "v9.9.9");
    }

    #[test]
    fn expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        write_cached_release(&path, &release_fixture()).unwrap();
        let future = SystemTime::now() + Duration::from_secs(301);
        assert!(read_cached_release(&path, future).is_none());
    }
}
```

- [ ] **Step 2: Add the test-only `tempfile` dependency**

In `mur-core/Cargo.toml`, find the existing `[dev-dependencies]` block (or create one above `[features]` if absent). Add:

```toml
[dev-dependencies]
tempfile = "3"
```

(skip if `tempfile` is already listed under `dev-dependencies`).

- [ ] **Step 3: Run the tests**

Run: `cargo test -p mur-core --lib update::release::cache_tests`
Expected: both cache tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/update/release.rs mur-core/Cargo.toml
git commit -m "feat(update): releases API client with 5min cache"
```

---

## Task 6: SHA256 verification helpers

**Files:**
- Modify: `mur-core/src/update/release.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/update/release.rs`:

```rust
use sha2::{Digest, Sha256};

/// Parse a `checksums.txt` line ("<hex> *<filename>" or "<hex>  <filename>")
/// and return the SHA256 hex for `filename`, or `None`.
pub fn checksum_for(checksums_txt: &str, filename: &str) -> Option<String> {
    for line in checksums_txt.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let hex = parts.next()?.trim();
        let rest = parts.next()?.trim_start_matches('*').trim();
        if rest == filename {
            return Some(hex.to_string());
        }
    }
    None
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    #[test]
    fn parses_bsd_style_with_asterisk() {
        let txt = "deadbeef *mur-x86_64.tar.gz\nfeedface *mur.zip\n";
        assert_eq!(
            checksum_for(txt, "mur-x86_64.tar.gz").as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn parses_gnu_style_with_double_space() {
        let txt = "deadbeef  mur-x86_64.tar.gz\n";
        assert_eq!(
            checksum_for(txt, "mur-x86_64.tar.gz").as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn returns_none_when_missing() {
        let txt = "deadbeef *mur.zip\n";
        assert!(checksum_for(txt, "other.tar.gz").is_none());
    }

    #[test]
    fn sha256_of_known_string() {
        // sha256("abc") = ba7816bf...
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p mur-core --lib update::release::checksum_tests`
Expected: all four tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/update/release.rs
git commit -m "feat(update): checksums.txt parser + sha256"
```

---

## Task 7: Tarball/zip extraction helper

**Files:**
- Modify: `mur-core/src/update/release.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/update/release.rs`:

```rust
use std::io::{Read, Write};
use std::path::Path;

/// Extract the `mur` (or `mur.exe`) binary from a release archive (tar.gz or zip)
/// and write it to `dest`. Returns an error if the archive contains no `mur` entry.
pub fn extract_binary(archive_name: &str, bytes: &[u8], dest: &Path) -> anyhow::Result<()> {
    if archive_name.ends_with(".tar.gz") || archive_name.ends_with(".tgz") {
        let gz = flate2::read::GzDecoder::new(bytes);
        let mut tar = tar::Archive::new(gz);
        for entry in tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "mur" {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                std::fs::File::create(dest)?.write_all(&buf)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
                }
                return Ok(());
            }
        }
        anyhow::bail!("archive {archive_name} did not contain a `mur` binary");
    }
    if archive_name.ends_with(".zip") {
        // zip support uses the `zip` crate; see Step 2 for adding it.
        let cursor = std::io::Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(cursor)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name().rsplit('/').next().unwrap_or("");
            if name == "mur.exe" {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                std::fs::File::create(dest)?.write_all(&buf)?;
                return Ok(());
            }
        }
        anyhow::bail!("archive {archive_name} did not contain mur.exe");
    }
    anyhow::bail!("unknown archive type: {archive_name}")
}

#[cfg(test)]
mod extract_tests {
    use super::*;

    fn build_tgz(contents: &[(&str, &[u8])]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            for (name, data) in contents {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tar.append_data(&mut header, name, *data).unwrap();
            }
            tar.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_mur_from_tgz() {
        let bytes = build_tgz(&[("mur", b"FAKEBIN")]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("mur");
        extract_binary("foo.tar.gz", &bytes, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"FAKEBIN");
    }

    #[test]
    fn errors_when_mur_missing_from_tgz() {
        let bytes = build_tgz(&[("README", b"hi")]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("mur");
        assert!(extract_binary("foo.tar.gz", &bytes, &dest).is_err());
    }
}
```

- [ ] **Step 2: Add the `zip` dependency**

In `mur-core/Cargo.toml`, under `[dependencies]`, add:

```toml
zip = { version = "2", default-features = false, features = ["deflate"] }
```

`tar` and `flate2` are already present per the existing manifest.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p mur-core --lib update::release::extract_tests`
Expected: both extraction tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/update/release.rs mur-core/Cargo.toml
git commit -m "feat(update): extract mur binary from release archive"
```

---

## Task 8: Unix atomic binary swap

**Files:**
- Modify: `mur-core/src/update/swap.rs`

- [ ] **Step 1: Write the failing test**

Replace the contents of `mur-core/src/update/swap.rs` with:

```rust
//! Atomically replace the running `mur` binary with a newly downloaded one.

use std::path::{Path, PathBuf};

/// Locate the running executable on disk.
pub fn current_exe() -> anyhow::Result<PathBuf> {
    std::env::current_exe().map_err(|e| anyhow::anyhow!("cannot resolve current exe: {e}"))
}

/// Atomically replace `target` with `new_binary`. On Unix this is `rename(2)`,
/// which is atomic when both paths live on the same filesystem. On Windows the
/// caller must use [`spawn_windows_swap_helper`] instead because the running
/// `.exe` is locked.
#[cfg(unix)]
pub fn swap(new_binary: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(new_binary, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(new_binary, target)?;
    Ok(())
}

#[cfg(windows)]
pub fn swap(_new_binary: &Path, _target: &Path) -> anyhow::Result<()> {
    anyhow::bail!("Windows must use spawn_windows_swap_helper instead of swap")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn swap_replaces_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mur");
        let newbin = dir.path().join("mur.new");
        std::fs::write(&target, b"OLD").unwrap();
        std::fs::write(&newbin, b"NEWBIN").unwrap();
        swap(&newbin, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"NEWBIN");
        assert!(!newbin.exists());
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core --lib update::swap::tests`
Expected: `swap_replaces_target` passes on Unix.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/update/swap.rs
git commit -m "feat(update): atomic binary swap on Unix"
```

---

## Task 9: Windows binary swap via PowerShell helper

**Files:**
- Modify: `mur-core/src/update/swap.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/update/swap.rs`:

```rust
/// Generate the PowerShell helper script content used on Windows to replace a
/// locked .exe after this process exits. The script sleeps 2s, moves the new
/// binary into place, then deletes itself. Caller is responsible for spawning
/// it detached via `Start-Process` or equivalent.
pub fn windows_helper_script(new_exe: &Path, target_exe: &Path, self_path: &Path) -> String {
    fn escape(p: &Path) -> String {
        p.display().to_string().replace('\'', "''")
    }
    format!(
        "Start-Sleep -Seconds 2\n\
         Move-Item -Force -LiteralPath '{new}' -Destination '{target}'\n\
         Remove-Item -LiteralPath '{self_}'\n",
        new = escape(new_exe),
        target = escape(target_exe),
        self_ = escape(self_path),
    )
}

#[cfg(windows)]
pub fn spawn_windows_swap_helper(new_exe: &Path, target_exe: &Path) -> anyhow::Result<()> {
    use std::process::Command;
    let helper = std::env::temp_dir().join(format!("mur-update-{}.ps1", std::process::id()));
    let script = windows_helper_script(new_exe, target_exe, &helper);
    std::fs::write(&helper, script)?;
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&helper)
        .spawn()
        .map_err(|e| anyhow::anyhow!(
            "On Windows, mur update requires PowerShell to complete the update: {e}"
        ))?;
    Ok(())
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn script_quotes_paths_and_self_deletes() {
        let s = windows_helper_script(
            Path::new("C:\\Temp\\mur.new.exe"),
            Path::new("C:\\Program Files\\mur\\mur.exe"),
            Path::new("C:\\Temp\\helper.ps1"),
        );
        assert!(s.contains("Start-Sleep -Seconds 2"));
        assert!(s.contains("'C:\\Temp\\mur.new.exe'"));
        assert!(s.contains("'C:\\Program Files\\mur\\mur.exe'"));
        assert!(s.contains("Remove-Item -LiteralPath 'C:\\Temp\\helper.ps1'"));
    }

    #[test]
    fn script_escapes_single_quotes_in_paths() {
        let s = windows_helper_script(
            Path::new("C:\\Temp\\bob's\\mur.new.exe"),
            Path::new("C:\\Apps\\mur.exe"),
            Path::new("C:\\Temp\\h.ps1"),
        );
        assert!(s.contains("'C:\\Temp\\bob''s\\mur.new.exe'"));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p mur-core --lib update::swap::helper_tests`
Expected: both helper tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/update/swap.rs
git commit -m "feat(update): Windows PowerShell swap helper"
```

---

## Task 10: Orchestrate the full update flow

**Files:**
- Modify: `mur-core/src/update/mod.rs`

- [ ] **Step 1: Replace the stub with the real flow**

Replace the body of `mur-core/src/update/mod.rs` (keeping the three `pub mod` lines) with:

```rust
pub mod release;
pub mod source;
pub mod swap;

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy)]
pub struct UpdateOptions {
    pub check_only: bool,
}

pub fn run(opts: UpdateOptions) -> Result<()> {
    let src = source::detect();
    if let Some(hint) = src.upgrade_hint() {
        println!("{hint}");
        return Ok(());
    }

    let release = release::fetch_latest().context("Could not check for updates. Are you online?")?;
    let current = env!("CARGO_PKG_VERSION");
    let latest = release::strip_v_prefix(&release.tag_name);

    if !release::is_newer(current, latest)? {
        println!("Already up to date (v{current})");
        return Ok(());
    }

    println!("New version available: v{current} → v{latest}");
    if opts.check_only {
        return Ok(());
    }

    let asset_name = release::asset_name_for_host().ok_or_else(|| {
        anyhow::anyhow!(
            "No prebuilt binary for {os}/{arch}. Install from source: cargo install mur",
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
        )
    })?;
    let asset = release::select_asset(&release, asset_name)?;

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("mur-update/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    println!("Downloading {asset_name}…");
    let bin_bytes = client
        .get(&asset.browser_download_url)
        .send()?
        .error_for_status()?
        .bytes()?
        .to_vec();

    let checksums_asset = release::select_asset(&release, "checksums.txt")?;
    let checksums_txt = client
        .get(&checksums_asset.browser_download_url)
        .send()?
        .error_for_status()?
        .text()?;
    let expected = release::checksum_for(&checksums_txt, asset_name)
        .ok_or_else(|| anyhow::anyhow!("no checksum entry for {asset_name}"))?;
    let actual = release::sha256_hex(&bin_bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        anyhow::bail!("Checksum verification FAILED. Aborting.");
    }

    let target = swap::current_exe().context(
        "Cannot determine install location. Please reinstall via: \
         curl -fsSL https://mur.run/install.sh | sh",
    )?;
    let tmp_dir = tempfile::tempdir()?;
    let tmp_bin = tmp_dir.path().join(if cfg!(windows) { "mur.new.exe" } else { "mur.new" });
    release::extract_binary(asset_name, &bin_bytes, &tmp_bin)?;

    #[cfg(unix)]
    {
        swap::swap(&tmp_bin, &target)?;
        println!("Updated to v{latest}");
    }
    #[cfg(windows)]
    {
        swap::spawn_windows_swap_helper(&tmp_bin, &target)?;
        println!("Update staged; closing now so it can take effect…");
    }

    // Make sure the temp dir survives the move on Unix (rename moved tmp_bin out of it)
    drop(tmp_dir);
    Ok(())
}
```

- [ ] **Step 2: Add `tempfile` to runtime dependencies if not already present**

`tempfile` is used at runtime now, not just in dev-dependencies. In `mur-core/Cargo.toml` `[dependencies]`, add:

```toml
tempfile = "3"
```

(if it is currently only under `[dev-dependencies]`, move it; otherwise leave alone). Remove the dev-dependencies entry to avoid duplication.

- [ ] **Step 3: Build to make sure it compiles on the current host**

Run: `cargo build -p mur-core`
Expected: clean build.

- [ ] **Step 4: Run all `update` module tests to confirm nothing regressed**

Run: `cargo test -p mur-core --lib update::`
Expected: every test from tasks 2–9 still passes.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/update/mod.rs mur-core/Cargo.toml
git commit -m "feat(update): orchestrate full update flow"
```

---

## Task 11: Wire `mur update` into the CLI

**Files:**
- Create: `mur-core/src/cmd/update.rs`
- Modify: `mur-core/src/cmd/mod.rs`
- Modify: `mur-core/src/cli/mod.rs`
- Modify: `mur-core/src/dispatch.rs`

- [ ] **Step 1: Create the cmd shim**

Create `mur-core/src/cmd/update.rs`:

```rust
//! `mur update` CLI verb — translates clap args into `crate::update::run`.

use anyhow::Result;

use crate::update::{self, UpdateOptions};

pub(crate) fn cmd_update(check_only: bool) -> Result<()> {
    update::run(UpdateOptions { check_only })
}
```

- [ ] **Step 2: Register the module in cmd/mod.rs**

In `mur-core/src/cmd/mod.rs`, add (placed alphabetically near the other `update`/`var` lines):

```rust
pub(crate) mod update;
```

- [ ] **Step 3: Add the `Update` variant to the CLI**

In `mur-core/src/cli/mod.rs`, inside the `pub enum Commands { ... }` block, add:

```rust
    /// Check for and install the latest mur release
    Update {
        /// Only check whether a newer version exists; don't install
        #[arg(long)]
        check: bool,
    },
```

Place it next to other top-level utility commands (e.g., near `Reindex` or `Doctor`).

- [ ] **Step 4: Handle it in dispatch.rs**

In `mur-core/src/dispatch.rs`, find the `match cli.command { ... }` block and add:

```rust
        Commands::Update { check } => cmd::update::cmd_update(check)?,
```

near the other simple verbs like `Commands::Doctor` and `Commands::Reindex { .. }`.

- [ ] **Step 5: Verify the CLI surface**

Run: `cargo run -p mur-core -- update --help`
Expected: clap prints a help block for `mur update` including the `--check` flag.

Run: `cargo run -p mur-core -- update --check`
Expected (offline-safe): prints either an upgrade hint (`brew upgrade mur` if you are on a brew install), `Already up to date (vX.Y.Z)`, or `New version available: ...`. No panic.

- [ ] **Step 6: Lint**

Run: `cargo clippy -p mur-core -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/update.rs mur-core/src/cmd/mod.rs mur-core/src/cli/mod.rs mur-core/src/dispatch.rs
git commit -m "feat(update): expose 'mur update [--check]' CLI verb"
```

---

## Task 12: Write `install.sh`

**Files:**
- Create: `scripts/install.sh`

- [ ] **Step 1: Author the installer**

Create `scripts/install.sh` with the following content (POSIX `sh`, no bash-isms; tested by hand against `dash` semantics):

```sh
#!/bin/sh
# install.sh — one-liner installer for mur CLI.
# Usage: curl -fsSL https://mur.run/install.sh | sh
#        curl -fsSL https://mur.run/install.sh | sh -s -- --version 2.16.1
# Env:   MUR_INSTALL_DIR (default: $HOME/.local/bin then /usr/local/bin)

set -eu

GH_OWNER="mur-run"
GH_REPO="mur"
SILENT=0
VERSION=""

log() { [ "$SILENT" = "1" ] || printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }

# --- Parse args ---
while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:-}"; shift 2 ;;
        --version=*) VERSION="${1#--version=}"; shift ;;
        -s|--silent) SILENT=1; shift ;;
        -h|--help)
            echo "Usage: install.sh [--version X.Y.Z] [-s]"
            exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

# --- Detect OS / arch ---
uname_s=$(uname -s 2>/dev/null || echo unknown)
uname_m=$(uname -m 2>/dev/null || echo unknown)

case "$uname_s" in
    Darwin)
        case "$uname_m" in
            arm64|aarch64) ASSET="mur-aarch64-apple-darwin.tar.gz" ;;
            *) die "macOS on $uname_m is not supported. Install from source: cargo install mur" ;;
        esac ;;
    Linux)
        case "$uname_m" in
            x86_64|amd64) ASSET="mur-x86_64-unknown-linux-gnu.tar.gz" ;;
            aarch64|arm64) die "No prebuilt Linux arm64 binary yet. Install from source: cargo install mur" ;;
            *) die "Linux on $uname_m is not supported." ;;
        esac ;;
    MINGW*|MSYS*|CYGWIN*)
        die "On Windows, run instead: irm https://mur.run/install.ps1 | iex" ;;
    *) die "Unsupported OS: $uname_s" ;;
esac

# --- Resolve version ---
if [ -z "$VERSION" ]; then
    log "Resolving latest mur release…"
    API_URL="https://api.github.com/repos/$GH_OWNER/$GH_REPO/releases/latest"
    if command -v curl >/dev/null 2>&1; then
        LATEST_JSON=$(curl -fsSL "$API_URL" 2>/dev/null) || die "Could not reach GitHub. Check your connection."
    elif command -v wget >/dev/null 2>&1; then
        LATEST_JSON=$(wget -qO- "$API_URL") || die "Could not reach GitHub. Check your connection."
    else
        die "Neither curl nor wget is installed."
    fi
    TAG=$(printf '%s' "$LATEST_JSON" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
    [ -n "$TAG" ] || die "Could not parse latest tag from GitHub API."
    VERSION="${TAG#v}"
fi

REL_URL="https://github.com/$GH_OWNER/$GH_REPO/releases/download/v${VERSION}"
ASSET_URL="$REL_URL/$ASSET"
CHECKSUMS_URL="$REL_URL/checksums.txt"

# --- Pick install dir ---
DEFAULT_DIR="${HOME:-/root}/.local/bin"
INSTALL_DIR="${MUR_INSTALL_DIR:-$DEFAULT_DIR}"
mkdir -p "$INSTALL_DIR" 2>/dev/null || true
if ! [ -w "$INSTALL_DIR" ]; then
    if [ -w /usr/local/bin ]; then
        INSTALL_DIR=/usr/local/bin
    else
        die "$INSTALL_DIR is not writable. Re-run with: MUR_INSTALL_DIR=/usr/local/bin sudo sh install.sh"
    fi
fi

# --- Download + verify ---
TMP=$(mktemp -d 2>/dev/null || mktemp -d -t mur-install)
trap 'rm -rf "$TMP"' EXIT

log "Downloading $ASSET (v$VERSION)…"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$ASSET_URL"     -o "$TMP/$ASSET" || die "Download failed."
    curl -fsSL "$CHECKSUMS_URL" -o "$TMP/checksums.txt" || die "Download failed."
else
    wget -q "$ASSET_URL"     -O "$TMP/$ASSET" || die "Download failed."
    wget -q "$CHECKSUMS_URL" -O "$TMP/checksums.txt" || die "Download failed."
fi

EXPECTED=$(grep " [*]\\{0,1\\}$ASSET\$" "$TMP/checksums.txt" | awk '{print $1}' | head -n1)
[ -n "$EXPECTED" ] || die "No checksum entry for $ASSET in checksums.txt."

if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL=$(sha256sum "$TMP/$ASSET" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL=$(shasum -a 256 "$TMP/$ASSET" | awk '{print $1}')
else
    die "Need sha256sum or shasum to verify download."
fi

[ "$EXPECTED" = "$ACTUAL" ] || die "Checksum verification FAILED. Aborting."

# --- Extract + install ---
tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/mur" ] || die "Archive did not contain a mur binary."
chmod +x "$TMP/mur"
mv "$TMP/mur" "$INSTALL_DIR/mur"

# --- PATH check ---
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        log ""
        log "Installed to $INSTALL_DIR but it is not on your PATH."
        log "Add this to your shell profile:"
        log "    export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

log ""
log "mur v$VERSION installed."
log "Run 'mur init' to get started."
```

- [ ] **Step 2: Mark it executable**

```bash
chmod +x scripts/install.sh
```

- [ ] **Step 3: Lint with `shellcheck` if available**

Run: `command -v shellcheck && shellcheck -s sh scripts/install.sh`
Expected: no errors. If `shellcheck` is not installed, skip; the CI test image will install it.

- [ ] **Step 4: Smoke-test in a Docker container**

Run:

```bash
docker run --rm -v "$PWD/scripts/install.sh:/install.sh:ro" ubuntu:latest sh -c \
  "apt-get update -qq && apt-get install -y curl ca-certificates tar -qq && sh /install.sh --version 2.16.1 && /root/.local/bin/mur --version"
```

Expected: prints `mur 2.16.1` (or whatever version the release publishes).

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(install): POSIX shell installer for Linux + macOS"
```

---

## Task 13: Write `install.ps1`

**Files:**
- Create: `scripts/install.ps1`

- [ ] **Step 1: Author the installer**

Create `scripts/install.ps1`:

```powershell
# install.ps1 — one-liner Windows installer for mur CLI.
# Usage:
#   irm https://mur.run/install.ps1 | iex
#   $env:MUR_VERSION = "2.16.1"; irm https://mur.run/install.ps1 | iex
# Optional env: MUR_INSTALL_DIR (defaults to $HOME\.local\bin)

$ErrorActionPreference = 'Stop'

$GhOwner = 'mur-run'
$GhRepo  = 'mur'

# --- Detect arch ---
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64') {
    throw "Windows on $arch is not yet supported. Install from source: cargo install mur"
}
$asset = 'mur-x86_64-pc-windows-msvc.zip'

# --- Resolve version ---
$version = $env:MUR_VERSION
if (-not $version) {
    Write-Host 'Resolving latest mur release…'
    $latest = Invoke-RestMethod -Headers @{ 'User-Agent' = 'mur-install' } `
        -Uri "https://api.github.com/repos/$GhOwner/$GhRepo/releases/latest"
    $version = $latest.tag_name -replace '^v',''
}

$relUrl       = "https://github.com/$GhOwner/$GhRepo/releases/download/v$version"
$assetUrl     = "$relUrl/$asset"
$checksumsUrl = "$relUrl/checksums.txt"

# --- Pick install dir ---
$installDir = $env:MUR_INSTALL_DIR
if (-not $installDir) {
    $installDir = Join-Path $HOME '.local\bin'
}
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

# --- Download + verify ---
$tmp = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) ("mur-install-" + [Guid]::NewGuid())) -Force
try {
    Write-Host "Downloading $asset (v$version)…"
    Invoke-WebRequest -Uri $assetUrl     -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest -Uri $checksumsUrl -OutFile (Join-Path $tmp 'checksums.txt')

    $expectedLine = (Get-Content (Join-Path $tmp 'checksums.txt')) | Where-Object { $_ -match "[ *]$([regex]::Escape($asset))$" }
    if (-not $expectedLine) { throw "No checksum entry for $asset" }
    $expected = ($expectedLine -split '\s+')[0].ToLower()

    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
    if ($expected -ne $actual) { throw 'Checksum verification FAILED. Aborting.' }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    $exe = Get-ChildItem -Path $tmp -Recurse -Filter 'mur.exe' | Select-Object -First 1
    if (-not $exe) { throw 'Archive did not contain mur.exe' }
    Move-Item -Force -Path $exe.FullName -Destination (Join-Path $installDir 'mur.exe')
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# --- PATH check ---
$userPath = [Environment]::GetEnvironmentVariable('Path','User')
if (-not ($userPath -split ';' | Where-Object { $_ -eq $installDir })) {
    Write-Host "Adding $installDir to your User PATH…"
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
    Write-Host 'Open a new terminal for PATH changes to take effect.'
}

Write-Host ''
Write-Host "mur v$version installed to $installDir\mur.exe"
Write-Host "Run 'mur init' to get started."
```

- [ ] **Step 2: Static-check by running in `pwsh -NoProfile -Command "$ErrorActionPreference='Stop'; . scripts/install.ps1 -WhatIf"`**

(Skip if no PowerShell is available locally; CI will exercise it on the Windows runner.)

- [ ] **Step 3: Commit**

```bash
git add scripts/install.ps1
git commit -m "feat(install): PowerShell installer for Windows"
```

---

## Task 14: Add crates.io metadata to `mur-common`

**Files:**
- Modify: `mur-common/Cargo.toml`

- [ ] **Step 1: Add `keywords`, `categories`, `homepage`, and `readme`**

In `mur-common/Cargo.toml`, update the `[package]` block to look like:

```toml
[package]
name = "mur-common"
description = "Shared types and traits for the MUR ecosystem"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
homepage = "https://mur.run"
readme = "../README.md"
keywords = ["ai", "coding", "cli", "learning", "patterns"]
categories = ["command-line-utilities", "development-tools"]
```

- [ ] **Step 2: Verify package metadata is publishable**

Run: `cargo publish -p mur-common --dry-run`
Expected: dry-run completes with no errors about missing `description`, `license`, or `repository`.

- [ ] **Step 3: Commit**

```bash
git add mur-common/Cargo.toml
git commit -m "chore(mur-common): add crates.io metadata"
```

---

## Task 15: Add crates.io metadata to `mur-core` + versioned `mur-common` dep

**Files:**
- Modify: `mur-core/Cargo.toml`

- [ ] **Step 1: Add `keywords`, `categories`, `homepage`, `readme`**

In `mur-core/Cargo.toml`, update the `[package]` block:

```toml
[package]
name = "mur-core"
description = "Continuous learning for AI assistants — CLI and library"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
homepage = "https://mur.run"
readme = "../README.md"
keywords = ["ai", "coding", "cli", "learning", "patterns"]
categories = ["command-line-utilities", "development-tools"]
```

- [ ] **Step 2: Convert `mur-common` from path-only to dual path+version**

Find this line in `mur-core/Cargo.toml`:

```toml
mur-common = { path = "../mur-common" }
```

Replace with:

```toml
mur-common = { path = "../mur-common", version = "2.16" }
```

(Pin the **minor** version of the *current* workspace version. Update whenever the workspace bumps a minor.)

- [ ] **Step 3: Do the same for `mur-agent-runtime` if it is path-only**

If `mur-agent-runtime` is also published to crates.io, repeat Step 2 for that dep. If it stays workspace-only, leave it alone — but `mur-core` will then fail `cargo publish` because it references an unpublished path dep. **Decision (per spec §6):** publish `mur-common` first, then `mur-core`; `mur-agent-runtime` stays path-only for now. To make `mur-core` publishable without `mur-agent-runtime` on crates.io, gate the `cmd::agent::cmd_export` use behind a default-on feature `agent-runtime` that pulls in `mur-agent-runtime`, and disable it in the published manifest via `[package.metadata.docs.rs]` is **not** sufficient — we instead use:

```toml
[features]
default = ["agent-runtime"]
agent-runtime = ["dep:mur-agent-runtime"]
```

and convert the existing dep to:

```toml
mur-agent-runtime = { path = "../mur-agent-runtime", optional = true }
```

Then guard every usage of `mur_agent_runtime::` with `#[cfg(feature = "agent-runtime")]`. Run `cargo check -p mur-core --no-default-features` to confirm the no-feature build compiles. If guarding is too invasive (the agent runtime is deeply embedded), DEFER publication of `mur-core` to a follow-up and publish only `mur-common` in Task 19 — note this in the PR description.

- [ ] **Step 4: Verify dry-run publish**

Run: `cargo publish -p mur-core --dry-run --allow-dirty`
Expected: succeeds, OR prints a clear blocker that maps to the deferral noted in Step 3.

- [ ] **Step 5: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/  # only if cfg-guarding was added
git commit -m "chore(mur-core): add crates.io metadata + versioned mur-common dep"
```

---

## Task 16: release.yml — `package-macos` job (DMG + PKG)

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Insert the job between `build` and `release`**

In `.github/workflows/release.yml`, after the `build` job (ends around line 106) and before the `release` job, insert:

```yaml
  package-macos:
    name: Package macOS (DMG + PKG)
    needs: build
    runs-on: macos-latest
    if: always() && needs.build.result == 'success'
    steps:
      - uses: actions/checkout@v6

      - name: Get version from tag
        id: version
        run: echo "version=${GITHUB_REF_NAME#v}" >> "$GITHUB_OUTPUT"

      - uses: actions/download-artifact@v4
        with:
          name: mur-aarch64-apple-darwin

      - name: Extract binary from tar.gz
        run: tar xzf mur-aarch64-apple-darwin.tar.gz

      - name: Import Apple signing certs
        uses: apple-actions/import-codesign-certs@v2
        with:
          p12-file-base64: ${{ secrets.APPLE_SIGNING_CERT }}
          p12-password: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}

      - name: Stage pkg-root
        run: |
          mkdir -p pkg-root/usr/local/bin
          mkdir -p pkg-root/usr/local/share/doc/mur
          cp mur pkg-root/usr/local/bin/mur
          chmod +x pkg-root/usr/local/bin/mur
          cp LICENSE pkg-root/usr/local/share/doc/mur/LICENSE
          mkdir -p scripts-pkg
          cat > scripts-pkg/postinstall <<'EOF'
          #!/bin/sh
          echo "MUR installed to /usr/local/bin/mur"
          echo "Run 'mur init' to get started."
          exit 0
          EOF
          chmod +x scripts-pkg/postinstall

      - name: Build .pkg
        run: |
          pkgbuild --root pkg-root \
            --identifier run.mur.cli \
            --version "${{ steps.version.outputs.version }}" \
            --scripts scripts-pkg/ \
            --install-location / \
            mur.pkg

      - name: Sign .pkg
        run: |
          productsign --sign "Developer ID Installer: $APPLE_TEAM_NAME ($APPLE_TEAM_ID)" \
            mur.pkg mur-aarch64-apple-darwin.pkg
        env:
          APPLE_TEAM_NAME: ${{ secrets.APPLE_TEAM_NAME }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}

      - name: Create DMG
        run: |
          mkdir dmg-root
          cp mur-aarch64-apple-darwin.pkg dmg-root/
          hdiutil create -volname "MUR" -srcfolder dmg-root -format UDZO mur-aarch64-apple-darwin.dmg

      - name: Sign DMG
        run: |
          codesign --sign "Developer ID Application: $APPLE_TEAM_NAME ($APPLE_TEAM_ID)" \
            mur-aarch64-apple-darwin.dmg
        env:
          APPLE_TEAM_NAME: ${{ secrets.APPLE_TEAM_NAME }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}

      - name: Notarize DMG
        run: |
          xcrun notarytool submit mur-aarch64-apple-darwin.dmg \
            --apple-id "$APPLE_ID" \
            --team-id "$TEAM_ID" \
            --password "$NOTARY_PWD" \
            --wait
          xcrun stapler staple mur-aarch64-apple-darwin.dmg
        env:
          APPLE_ID: ${{ secrets.APPLE_NOTARY_USER }}
          TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
          NOTARY_PWD: ${{ secrets.APPLE_NOTARY_PASSWORD }}

      - name: Verify Gatekeeper acceptance
        run: |
          spctl -a -t open --context context:primary-signature mur-aarch64-apple-darwin.dmg

      - name: Upload DMG + PKG artifacts
        uses: actions/upload-artifact@v4
        with:
          name: mur-macos-installer
          path: |
            mur-aarch64-apple-darwin.pkg
            mur-aarch64-apple-darwin.dmg
```

- [ ] **Step 2: Update the `release` job to wait for `package-macos`**

Find:

```yaml
  release:
    name: Publish Release
    needs: build
```

Change to:

```yaml
  release:
    name: Publish Release
    needs: [build, package-macos]
```

And widen the artifact glob — leave `merge-multiple: true`, so the DMG/PKG artifact is bundled. The existing `softprops/action-gh-release` step uploads `*.tar.gz`, `*.zip`, `checksums.txt` — add `*.pkg` and `*.dmg` to the `files:` list:

```yaml
          files: |
            *.tar.gz
            *.zip
            *.pkg
            *.dmg
            checksums.txt
```

- [ ] **Step 3: Add a doc note about required secrets**

At the top of `.github/workflows/release.yml`, just under the existing `permissions:` block, add a comment listing required secrets (no functional change):

```yaml
# Required secrets:
#   APPLE_SIGNING_CERT       - base64-encoded Developer ID p12 bundle
#   APPLE_KEYCHAIN_PASSWORD  - password for the p12 above
#   APPLE_NOTARY_USER        - Apple ID email used for notarization
#   APPLE_NOTARY_PASSWORD    - app-specific password for that Apple ID
#   APPLE_TEAM_ID            - Apple Developer Team ID
#   APPLE_TEAM_NAME          - "common name" piece used in the codesign identity
#   CRATES_IO_TOKEN          - crates.io API token (publish-crates job)
#   HOMEBREW_TAP_TOKEN       - existing, used by update-homebrew job
```

- [ ] **Step 4: Lint the YAML**

Run: `yamllint .github/workflows/release.yml` (skip if not installed).
Expected: no errors. At minimum, confirm `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml'))"` exits 0.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): sign+notarize macOS DMG and PKG"
```

---

## Task 17: release.yml — `deploy-installer` job (gh-pages)

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Append the job at the end of the file**

After the `update-homebrew` job, append:

```yaml
  deploy-installer:
    name: Deploy install.sh / install.ps1 to mur.run
    needs: release
    if: always() && needs.release.result == 'success'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0

      - name: Stage installer scripts
        run: |
          mkdir -p /tmp/installer
          cp scripts/install.sh  /tmp/installer/install.sh
          cp scripts/install.ps1 /tmp/installer/install.ps1

      - name: Push to gh-pages
        run: |
          git fetch origin gh-pages || git checkout --orphan gh-pages
          git checkout gh-pages
          cp /tmp/installer/install.sh  install.sh
          cp /tmp/installer/install.ps1 install.ps1
          # CNAME for mur.run
          if [ ! -f CNAME ]; then echo "mur.run" > CNAME; fi
          git config user.name  "github-actions[bot]"
          git config user.email "github-actions[bot]@users.noreply.github.com"
          git add install.sh install.ps1 CNAME
          if git diff --cached --quiet; then
            echo "No installer changes."
            exit 0
          fi
          git commit -m "installer scripts for ${GITHUB_REF_NAME}"
          git push origin gh-pages
```

- [ ] **Step 2: Document the DNS prerequisite**

Append to the comment block at the top of the file (created in Task 16 Step 3):

```yaml
# DNS prerequisite:
#   - mur.run DNS must CNAME to mur-run.github.io
#   - GitHub Pages must be configured to publish from gh-pages branch with
#     "mur.run" as the custom domain.
#   - This job will silently no-op if the prerequisite is not in place.
```

- [ ] **Step 3: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"`
Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): deploy install.sh + install.ps1 to gh-pages"
```

---

## Task 18: release.yml — `publish-crates` job

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Append the job**

After `deploy-installer`, append:

```yaml
  publish-crates:
    name: Publish to crates.io
    needs: release
    if: always() && needs.release.result == 'success'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable

      - name: Install protobuf
        run: sudo apt-get update && sudo apt-get install -y protobuf-compiler libdbus-1-dev libasound2-dev

      - name: Publish mur-common
        run: cargo publish -p mur-common --token "${CRATES_IO_TOKEN}"
        env:
          CRATES_IO_TOKEN: ${{ secrets.CRATES_IO_TOKEN }}

      - name: Wait for crates.io index propagation
        run: sleep 60

      - name: Publish mur-core
        # Allowed to fail until mur-core feature-gating is complete (see Task 15 Step 3).
        continue-on-error: true
        run: cargo publish -p mur-core --token "${CRATES_IO_TOKEN}"
        env:
          CRATES_IO_TOKEN: ${{ secrets.CRATES_IO_TOKEN }}
```

The `continue-on-error: true` on `mur-core` keeps the rest of the pipeline green during the rollout window described in Task 15 Step 3. Remove it once `mur-core` reliably publishes.

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"`
Expected: exit 0.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): publish mur-common (and best-effort mur-core) to crates.io"
```

---

## Task 19: Update README install section

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Read the existing install section**

Run: `grep -n -i "install" README.md | head -20` to find the section heading.

- [ ] **Step 2: Replace the "Install" section with**

```markdown
## Install

### macOS / Linux — one-liner

```sh
curl -fsSL https://mur.run/install.sh | sh
```

### Windows — one-liner (PowerShell)

```powershell
irm https://mur.run/install.ps1 | iex
```

### macOS — signed installer

Download the latest `mur-aarch64-apple-darwin.dmg` from the
[Releases page](https://github.com/mur-run/mur/releases/latest) and double-click.

### Homebrew (macOS arm64)

```sh
brew install mur-run/tap/mur
```

### crates.io

```sh
cargo install mur
```

### Update

```sh
mur update          # check + install
mur update --check  # check only, don't install
```

`mur update` is a no-op when installed via Homebrew or `cargo install` — those
package managers should be used to upgrade instead.
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): refresh install section with new options"
```

---

## Task 20: End-to-end smoke test for `mur update`

**Files:**
- Create: `mur-core/tests/update_integration.rs`

- [ ] **Step 1: Write the gated integration test**

Create `mur-core/tests/update_integration.rs`:

```rust
//! Integration smoke test for `mur update`. Network-bound; gated on
//! MUR_UPDATE_NETWORK_TESTS=1 so CI doesn't fail when offline or rate-limited.

#[test]
fn fetch_latest_release_returns_a_tag() {
    if std::env::var("MUR_UPDATE_NETWORK_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping: set MUR_UPDATE_NETWORK_TESTS=1 to run");
        return;
    }
    let r = mur_core::update::release::fetch_latest().expect("fetch_latest");
    assert!(r.tag_name.starts_with('v'));
    assert!(!r.assets.is_empty());
}
```

- [ ] **Step 2: Run with the env flag set**

Run: `MUR_UPDATE_NETWORK_TESTS=1 cargo test -p mur-core --test update_integration -- --nocapture`
Expected: passes against the real GitHub API.

- [ ] **Step 3: Confirm the test is skipped by default**

Run: `cargo test -p mur-core --test update_integration`
Expected: test runs but exits 0 with a "skipping" message in stderr.

- [ ] **Step 4: Commit**

```bash
git add mur-core/tests/update_integration.rs
git commit -m "test(update): gated integration test against live releases API"
```

---

## Self-Review Notes

- **Spec coverage:** §1 architecture covered by Tasks 11/12/13/16–18; §2 install.sh by Task 12; §3 DMG+PKG by Task 16; §4 self-update by Tasks 1–11 and 20; §5 CI/CD by Tasks 16–18; §6 crates.io metadata by Tasks 14–15; §7 error messages mapped into Tasks 4, 9, 10, 12; §8 testing strategy implemented as unit tests in each module plus the gated integration test in Task 20.
- **Rollout sequence:** matches spec §9 — self-update first (1–11), shell installers next (12–13), Cargo metadata (14–15), CI updates (16–18), README (19), integration smoke (20).
- **Names used consistently:** `update::run`, `UpdateOptions { check_only }`, `release::fetch_latest`, `release::is_newer`, `release::strip_v_prefix`, `release::checksum_for`, `release::sha256_hex`, `release::asset_name_for_host`, `release::select_asset`, `release::extract_binary`, `source::detect`, `source::InstallSource`, `swap::current_exe`, `swap::swap`, `swap::spawn_windows_swap_helper`, `cmd::update::cmd_update`, `Commands::Update { check }`.
- **Apple Developer membership** (spec §9.6) is a human prerequisite — Task 16 will fail in CI until secrets are populated, but the rest of the rollout (Tasks 1–15, 17–20) is independent and can land first.
