//! Task 3.2: install the pinned ast-grep at the resolver's path.
//!
//! Two sha256 checks, both against compile-time constants: the release zip
//! (same table as CI's install step) and the extracted binary, so a later
//! re-run can verify what is on disk without re-downloading.
//! [`verify_and_place`] is the network-free core; [`install`] adds the fetch.

use anyhow::{Context, Result, bail};
use mur_common::config::AST_GREP_PINNED_VERSION;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// One platform's pinned release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pinned {
    /// Rust target triple in the asset name (`app-<target>.zip`).
    pub target: &'static str,
    pub zip_sha256: String,
    /// Exact top-level entry name inside the zip.
    pub member: &'static str,
    pub bin_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Missing,
    Verified,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    AlreadyInstalled,
}

/// Release download base; the asset is `<base>/<version>/app-<target>.zip`.
const RELEASE_BASE: &str = "https://github.com/ast-grep/ast-grep/releases/download";
/// Whole-request ceiling for the zip fetch (~25 MB).
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

/// (os, arch, target, zip sha256, member, binary sha256) for
/// `AST_GREP_PINNED_VERSION`. Zip hashes must match `.github/workflows/ci.yml`
/// (a test enforces it); bump all of them together with the version.
const PINS: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "macos",
        "aarch64",
        "aarch64-apple-darwin",
        "6d2279dea5bea2ad79c66ea93f5fe54ba926e398a8a26de76c56db68fe59eac6",
        "ast-grep",
        "5651f0c6dcbbf2f7813297f9eb8f6ba00fb4ee2d81410a8138ac6151416f31ad",
    ),
    (
        "linux",
        "x86_64",
        "x86_64-unknown-linux-gnu",
        "f8ac830881339d1edee6b2652f54798c0f4da5a827f2db38a08ee31117783ce8",
        "ast-grep",
        "7a5ab30160186184c0bf8bffc87da4af25123c183964cd98c11b0b354137db0a",
    ),
    (
        "windows",
        "x86_64",
        "x86_64-pc-windows-msvc",
        "3751b7d6be7fd39a80df1180ffe7e053903dcf92d3190e5a8336ff1746af9059",
        "ast-grep.exe",
        "daff0f5963faab7617045833132a3538c85eee65f3afeedf347f829a7b8d83fb",
    ),
];

/// The pin for `os`/`arch`, or `None`: no unverified fallback.
pub fn pinned_for_platform(os: &str, arch: &str) -> Option<Pinned> {
    PINS.iter().find(|(o, a, ..)| *o == os && *a == arch).map(
        |&(_, _, target, zip, member, bin)| Pinned {
            target,
            zip_sha256: zip.to_owned(),
            member,
            bin_sha256: bin.to_owned(),
        },
    )
}

/// The pin for the running host.
pub fn pinned_for_host() -> Option<Pinned> {
    pinned_for_platform(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn download_url(p: &Pinned) -> String {
    format!(
        "{RELEASE_BASE}/{AST_GREP_PINNED_VERSION}/app-{}.zip",
        p.target
    )
}

/// What is at `dest` now, judged by the pinned binary hash.
pub fn installed_state(dest: &Path, p: &Pinned) -> Installed {
    match std::fs::read(dest) {
        Ok(b) if sha256_hex(&b).eq_ignore_ascii_case(&p.bin_sha256) => Installed::Verified,
        Ok(_) => Installed::Mismatch,
        Err(_) => Installed::Missing,
    }
}

/// Verify the zip, extract exactly `p.member`, verify it, then write it to
/// `dest` atomically (temp + rename, +x on unix). Fails closed: on any
/// mismatch nothing is created. A binary already matching the pin is left
/// untouched.
pub fn verify_and_place(zip: &[u8], p: &Pinned, dest: &Path) -> Result<Outcome> {
    if installed_state(dest, p) == Installed::Verified {
        return Ok(Outcome::AlreadyInstalled);
    }
    let got = sha256_hex(zip);
    if !got.eq_ignore_ascii_case(&p.zip_sha256) {
        bail!(
            "ast-grep zip sha256 mismatch: expected {}, got {got}",
            p.zip_sha256
        );
    }
    let bin = extract_member(zip, p.member)?;
    let got = sha256_hex(&bin);
    if !got.eq_ignore_ascii_case(&p.bin_sha256) {
        bail!(
            "ast-grep binary sha256 mismatch: expected {}, got {got}",
            p.bin_sha256
        );
    }
    place(dest, &bin)?;
    Ok(Outcome::Installed)
}

/// Download the host's pinned zip and install it at `dest`. Skips the fetch
/// when `dest` already holds the pinned binary.
pub fn install(dest: &Path) -> Result<Outcome> {
    let p = pinned_for_host().with_context(|| {
        format!(
            "no pinned ast-grep {AST_GREP_PINNED_VERSION} build for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    if installed_state(dest, &p) == Installed::Verified {
        return Ok(Outcome::AlreadyInstalled);
    }
    let url = download_url(&p);
    let zip = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .build()
        .context("http client")?
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .with_context(|| format!("GET {url}"))?;
    verify_and_place(&zip, &p, dest)
}

/// Exact top-level name match: `evil/ast-grep` is not `ast-grep`.
fn extract_member(zip: &[u8], member: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut ar = zip::ZipArchive::new(std::io::Cursor::new(zip)).context("read ast-grep zip")?;
    let mut f = ar
        .by_name(member)
        .with_context(|| format!("ast-grep zip has no `{member}` entry"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).context("extract ast-grep")?;
    Ok(buf)
}

fn place(dest: &Path, bytes: &[u8]) -> Result<()> {
    let parent = dest.parent().context("install path has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    let tmp: PathBuf = dest.with_extension("mur-tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).context("chmod +x");
        }
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", dest.display()));
    }
    Ok(())
}

fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

#[cfg(test)]
#[path = "ast_grep_install_tests.rs"]
mod tests;
