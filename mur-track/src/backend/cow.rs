//! Platform-aware COW (Copy-on-Write) build-cache copy for parallel tracks.
//!
//! APFS on macOS and Btrfs on Linux support block-level clones: a `cp -c` /
//! `cp --reflink` of a multi-GB `target/` directory completes in milliseconds
//! because no bytes are physically copied until a page is actually modified.
//!
//! Detection order:
//!   macOS  → `cp -cR`  (APFS clone; always available on macOS 10.13+)
//!   Linux  → `cp --reflink=always -a`  (Btrfs/XFS only; fails fast on ext4 so no slow fallback copy)
//!   other  → skip (no COW; callers still work, just no warm cache)

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Directories (relative to project root) that are worth COW-copying.
const CACHE_DIRS: &[&str] = &["target"];

/// Upper bound on the pre-track Time Machine snapshot. It is a convenience,
/// never a correctness requirement, so it is abandoned rather than waited on.
#[cfg(target_os = "macos")]
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

/// COW copy method resolved at call time.
#[derive(Debug, PartialEq, Eq)]
pub enum CowMethod {
    /// macOS `cp -cR` — APFS block clone.
    ApfsClone,
    /// Linux `cp --reflink=always -a` — Btrfs/XFS only; fails fast on ext4 (no slow fallback).
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

/// Per-profile subdirs the fleet clone leaves out. `incremental/` is the bulk
/// of a target and is cheap for rustc to regenerate.
const FLEET_SKIP_SUBDIRS: &[&str] = &["incremental"];

/// A profile dir (`debug/`, `release/`, …) is recognised by this child.
const PROFILE_MARKER: &str = ".fingerprint";

/// Suffix of the scratch dir a clone is built in before it is renamed into place.
const TMP_SUFFIX: &str = ".tmp-";

/// What a filtered build-cache clone leaves out.
pub struct CachePlan<'a> {
    /// Child dirs of each profile dir to skip, e.g. `["incremental"]`.
    pub skip_subdirs: &'a [String],
    /// Dirs directly under the target to skip, e.g. `["turns"]`. Must name
    /// the turn subdir when cloning for a turn, or a clone would recurse into
    /// other turns' clones.
    pub skip_top: &'a [String],
}

/// Result of [`copy_build_cache_filtered`].
#[derive(Debug, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied { elapsed: Duration },
    AlreadyPresent,
    SourceMissing,
    Unsupported,
}

/// COW-copy each `CACHE_DIRS` entry from `src_project` into `dst_worktree`,
/// leaving out `incremental/` and final binaries.
/// Non-fatal: logs a warning and returns `Ok(())` on any per-dir failure so
/// track creation is never blocked by a cache-copy error.
pub fn copy_build_cache(src_project: &Path, dst_worktree: &Path) -> Result<()> {
    let skip_subdirs: Vec<String> = FLEET_SKIP_SUBDIRS.iter().map(|s| s.to_string()).collect();
    let plan = CachePlan {
        skip_subdirs: &skip_subdirs,
        skip_top: &[],
    };
    for dir in CACHE_DIRS {
        match copy_build_cache_filtered(&src_project.join(dir), &dst_worktree.join(dir), &plan) {
            Ok(CopyOutcome::Copied { .. }) => eprintln!("COW-copied {dir}/ → track"),
            Ok(_) => {}
            Err(e) => {
                eprintln!("warn: COW copy of {dir}/ skipped ({e}); track will build from scratch")
            }
        }
    }
    Ok(())
}

/// COW-clone the reusable parts of `src_target` into `dst_target`: every
/// profile dir's children except `plan.skip_subdirs` (profile-root files are
/// final binaries, which Cargo relinks), plus regular files directly under the
/// target. Non-profile dirs and `plan.skip_top` are left out.
///
/// The clone is built in a sibling scratch dir and renamed into place, so a
/// half-finished clone is never mistaken for a warm one.
pub fn copy_build_cache_filtered(
    src_target: &Path,
    dst_target: &Path,
    plan: &CachePlan,
) -> Result<CopyOutcome> {
    let method = detect_cow_method();
    if method == CowMethod::Skip {
        return Ok(CopyOutcome::Unsupported);
    }
    filtered_with(src_target, dst_target, plan, &|s, d| {
        cow_copy_dir(&method, s, d)
    })
}

type Copier<'a> = &'a dyn Fn(&Path, &Path) -> Result<()>;

fn filtered_with(
    src_target: &Path,
    dst_target: &Path,
    plan: &CachePlan,
    copy: Copier,
) -> Result<CopyOutcome> {
    if !src_target.is_dir() {
        return Ok(CopyOutcome::SourceMissing);
    }
    if dst_target.exists() {
        return Ok(CopyOutcome::AlreadyPresent);
    }
    let started = Instant::now();
    let tmp = scratch_path(dst_target);
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    if let Some(parent) = tmp.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir(&tmp)?;
    if let Err(e) = clone_into(src_target, &tmp, plan, copy) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, dst_target) {
        let _ = std::fs::remove_dir_all(&tmp);
        if dst_target.exists() {
            return Ok(CopyOutcome::AlreadyPresent); // lost a race to another clone
        }
        return Err(e.into());
    }
    Ok(CopyOutcome::Copied {
        elapsed: started.elapsed(),
    })
}

fn scratch_path(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(format!("{TMP_SUFFIX}{}", std::process::id()));
    dst.with_file_name(name)
}

fn clone_into(src: &Path, tmp: &Path, plan: &CachePlan, copy: Copier) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        let from = entry.path();
        if ft.is_file() {
            copy(&from, &tmp.join(&name))?;
        } else if ft.is_dir()
            && !plan.skip_top.iter().any(|s| name == s.as_str())
            && from.join(PROFILE_MARKER).is_dir()
        {
            let profile = tmp.join(&name);
            std::fs::create_dir(&profile)?;
            for child in std::fs::read_dir(&from)? {
                let child = child?;
                let cname = child.file_name();
                if child.file_type()?.is_dir()
                    && !plan.skip_subdirs.iter().any(|s| cname == s.as_str())
                {
                    copy(&child.path(), &profile.join(&cname))?;
                }
            }
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
            .args(["--reflink=always", "-a"])
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

/// Take a Time Machine local snapshot before parallel track creation so the
/// user can restore if something goes wrong.  No entitlement required.
/// Non-fatal: if `tmutil` is unavailable or Time Machine is off, logs and
/// continues.
///
/// Bounded by [`SNAPSHOT_TIMEOUT`]: `tmutil localsnapshot` can block for a very
/// long time when the volume is nearly full, and this runs *before* any track
/// is created — an unbounded wait here stalls the whole `fleet run` with no
/// output. The snapshot is a best-effort convenience, so a slow one is
/// abandoned rather than waited on.
#[cfg(target_os = "macos")]
pub fn take_local_snapshot() {
    use std::time::Instant;

    let mut child = match Command::new("tmutil").arg("localsnapshot").spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warn: tmutil not available ({e}) — continuing");
            return;
        }
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => {
                eprintln!("Time Machine local snapshot created");
                return;
            }
            Ok(Some(s)) => {
                eprintln!("warn: tmutil localsnapshot exited {s} — continuing");
                return;
            }
            Ok(None) => {
                if start.elapsed() >= SNAPSHOT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!(
                        "warn: tmutil localsnapshot exceeded {}s (disk nearly full?) — continuing without a snapshot",
                        SNAPSHOT_TIMEOUT.as_secs()
                    );
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("warn: waiting on tmutil failed ({e}) — continuing");
                return;
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn take_local_snapshot() {}

#[cfg(test)]
#[path = "cow_tests.rs"]
mod filtered_tests;

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
