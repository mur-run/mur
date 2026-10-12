//! Warm a turn's Cargo target dir from the project's main `target/`.
//!
//! Runs once per turn, on its first build command: sweep orphaned turn dirs,
//! check the disk floor, then COW-clone the reusable parts of the main target
//! (see [`copy_build_cache_filtered`]). Every failure is a skip — the build
//! then runs cold, never wrong.

use super::target::{PROJECT_TARGET_DIR, sweep_orphan_turn_targets, turn_target_dir};
use crate::backend::cow::{CachePlan, CopyOutcome, copy_build_cache_filtered};
use mur_common::config::WarmBuildCacheConfig;
use std::io;
use std::path::Path;
use std::time::Duration;

/// Bytes per GiB, for comparing free space against `min_free_gib`.
const BYTES_PER_GIB: u64 = 1 << 30;

/// What [`warm_build_cache`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum WarmOutcome {
    Warmed { elapsed: Duration },
    AlreadyWarm,
    Skipped(SkipReason),
}

/// Why a warm clone did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum SkipReason {
    Disabled,
    NoSourceTarget,
    NoCow,
    LowDisk { free_gib: u64, floor_gib: u64 },
    Failed(String),
}

/// Clone `<project>/target` into this turn's dir. Never fails: problems come
/// back as [`WarmOutcome::Skipped`].
pub fn warm_build_cache(
    project: &Path,
    track_name: &str,
    cfg: &WarmBuildCacheConfig,
) -> WarmOutcome {
    warm_with(
        project,
        track_name,
        cfg,
        available_space,
        copy_build_cache_filtered,
    )
}

type FreeSpace = fn(&Path) -> io::Result<u64>;
type Clone = fn(&Path, &Path, &CachePlan) -> anyhow::Result<CopyOutcome>;

fn available_space(p: &Path) -> io::Result<u64> {
    fs2::available_space(p)
}

fn warm_with(
    project: &Path,
    track_name: &str,
    cfg: &WarmBuildCacheConfig,
    free: FreeSpace,
    clone: Clone,
) -> WarmOutcome {
    if !cfg.enabled {
        return WarmOutcome::Skipped(SkipReason::Disabled);
    }
    let subdir = cfg.validated_subdir();
    let dst = turn_target_dir(project, subdir, track_name);
    if dst.exists() {
        return WarmOutcome::AlreadyWarm;
    }
    let src = project.join(PROJECT_TARGET_DIR);
    if !src.is_dir() {
        return WarmOutcome::Skipped(SkipReason::NoSourceTarget);
    }
    // Before the floor check: an orphan sweep is what frees the space.
    sweep_orphan_turn_targets(project, subdir);
    match free(project) {
        Ok(bytes) if bytes / BYTES_PER_GIB < cfg.min_free_gib => {
            return WarmOutcome::Skipped(SkipReason::LowDisk {
                free_gib: bytes / BYTES_PER_GIB,
                floor_gib: cfg.min_free_gib,
            });
        }
        Ok(_) => {}
        Err(e) => return WarmOutcome::Skipped(SkipReason::Failed(format!("free space: {e}"))),
    }
    let skip_top = [subdir.to_string()];
    let plan = CachePlan {
        skip_subdirs: &cfg.skip_subdirs,
        skip_top: &skip_top,
    };
    match clone(&src, &dst, &plan) {
        Ok(CopyOutcome::Copied { elapsed }) => WarmOutcome::Warmed { elapsed },
        Ok(CopyOutcome::AlreadyPresent) => WarmOutcome::AlreadyWarm,
        Ok(CopyOutcome::SourceMissing) => WarmOutcome::Skipped(SkipReason::NoSourceTarget),
        Ok(CopyOutcome::Unsupported) => WarmOutcome::Skipped(SkipReason::NoCow),
        Err(e) => WarmOutcome::Skipped(SkipReason::Failed(e.to_string())),
    }
}

#[cfg(test)]
#[path = "warm_tests.rs"]
mod tests;
