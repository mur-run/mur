//! Platform-aware COW (Copy-on-Write) build-cache copy for parallel tracks.
//!
//! APFS on macOS and Btrfs on Linux support block-level clones: a `cp -c` /
//! `cp --reflink` of a multi-GB `target/` directory completes in milliseconds
//! because no bytes are physically copied until a page is actually modified.
//!
//! Detection order:
//!   macOS  → `cp -cR`  (APFS clone; always available on macOS 10.13+)
//!   Linux  → `cp --reflink=auto -a`  (Btrfs/XFS if available, silent fallback otherwise)
//!   other  → skip (no COW; callers still work, just no warm cache)

use anyhow::Result;
use std::path::Path;
use std::process::Command;

/// Directories (relative to project root) that are worth COW-copying.
const CACHE_DIRS: &[&str] = &["target"];

/// COW copy method resolved at call time.
#[derive(Debug, PartialEq, Eq)]
pub enum CowMethod {
    /// macOS `cp -cR` — APFS block clone.
    ApfsClone,
    /// Linux `cp --reflink=auto -a` — Btrfs/XFS reflink, silent fallback to regular.
    Reflink,
    /// Platform has no known COW support; skip.
    Skip,
}

pub fn detect_cow_method() -> CowMethod {
    #[cfg(target_os = "macos")]
    return CowMethod::ApfsClone;

    #[cfg(target_os = "linux")]
    return CowMethod::Reflink;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    CowMethod::Skip
}

/// COW-copy each `CACHE_DIRS` entry from `src_project` into `dst_worktree`.
/// Non-fatal: logs a warning and returns `Ok(())` on any per-dir failure so
/// track creation is never blocked by a cache-copy error.
pub fn copy_build_cache(src_project: &Path, dst_worktree: &Path) -> Result<()> {
    let method = detect_cow_method();
    if method == CowMethod::Skip {
        return Ok(());
    }
    for dir in CACHE_DIRS {
        let src = src_project.join(dir);
        if !src.exists() {
            continue;
        }
        let dst = dst_worktree.join(dir);
        if dst.exists() {
            continue; // already present — created by cargo or another call
        }
        if let Err(e) = cow_copy_dir(&method, &src, &dst) {
            eprintln!("warn: COW copy of {dir}/ skipped ({e}); track will build from scratch");
        } else {
            eprintln!("COW-copied {dir}/ → track ({})", method_label(&method));
        }
    }
    Ok(())
}

fn cow_copy_dir(method: &CowMethod, src: &Path, dst: &Path) -> Result<()> {
    let status = match method {
        CowMethod::ApfsClone => Command::new("cp")
            .args(["-cR"])
            .arg(src)
            .arg(dst)
            .status()?,
        CowMethod::Reflink => Command::new("cp")
            .args(["--reflink=auto", "-a"])
            .arg(src)
            .arg(dst)
            .status()?,
        CowMethod::Skip => return Ok(()),
    };
    if !status.success() {
        anyhow::bail!("cp exited with {status}");
    }
    Ok(())
}

fn method_label(m: &CowMethod) -> &'static str {
    match m {
        CowMethod::ApfsClone => "APFS clone",
        CowMethod::Reflink => "reflink",
        CowMethod::Skip => "skip",
    }
}

/// Take a Time Machine local snapshot before parallel track creation so the
/// user can restore if something goes wrong.  No entitlement required.
/// Non-fatal: if `tmutil` is unavailable or Time Machine is off, logs and
/// continues.
#[cfg(target_os = "macos")]
pub fn take_local_snapshot() {
    let result = Command::new("tmutil").arg("localsnapshot").status();
    match result {
        Ok(s) if s.success() => eprintln!("Time Machine local snapshot created"),
        Ok(s) => eprintln!("warn: tmutil localsnapshot exited {s} — continuing"),
        Err(e) => eprintln!("warn: tmutil not available ({e}) — continuing"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn take_local_snapshot() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_known_variant() {
        // Must return one of the three variants — just confirm it doesn't panic.
        let m = detect_cow_method();
        assert!(matches!(
            m,
            CowMethod::ApfsClone | CowMethod::Reflink | CowMethod::Skip
        ));
    }

    #[test]
    fn copy_build_cache_noop_when_src_missing() {
        let tmp = tempdir();
        // src_project has no target/ dir → copy_build_cache should return Ok silently
        let result = copy_build_cache(tmp.path(), tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn copy_build_cache_skips_existing_dst() {
        let src = tempdir();
        let dst = tempdir();
        // Create src/target and dst/target — dst/target already present, should skip
        std::fs::create_dir(src.path().join("target")).unwrap();
        std::fs::create_dir(dst.path().join("target")).unwrap();
        let result = copy_build_cache(src.path(), dst.path());
        assert!(result.is_ok());
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("tempdir")
    }
}
