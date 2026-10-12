//! Per-turn Cargo target dirs: `<project>/target/<subdir>/<track-name>/`.
//!
//! The dir lives under the project's own `target/` because that is the path
//! the sandbox lets build scripts execute from; the track's private `target/`
//! is not. Its name is the track's name, so a turn dir is live exactly while
//! `<project>/.worktrees/<name>` exists — no lock, no PID, and it also covers
//! turns owned by other agents in the same project.

use super::WORKTREES_DIR;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The project's Cargo target dir, relative to the project root.
pub const PROJECT_TARGET_DIR: &str = "target";

/// Prefix of a turn dir that has been renamed aside for deletion.
const TRASH_PREFIX: &str = ".trash-";

/// `<project>/target/<subdir>/<track_name>`.
pub fn turn_target_dir(project: &Path, subdir: &str, track_name: &str) -> PathBuf {
    project
        .join(PROJECT_TARGET_DIR)
        .join(subdir)
        .join(track_name)
}

/// Delete a turn dir. Idempotent, and safe to race with itself and with
/// [`sweep_orphan_turn_targets`]: the dir is first renamed to a unique
/// `.trash-*` sibling, so exactly one caller wins the rename and the rest see
/// `NotFound`, which counts as success.
pub fn remove_turn_target(dir: &Path) -> io::Result<()> {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Ok(());
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let trash = parent.join(format!(
        "{TRASH_PREFIX}{}-{}-{nanos}",
        name.to_string_lossy(),
        std::process::id()
    ));
    // A sweep may clear our `.trash-*` while we delete it: NotFound either
    // way means the dir is gone, which is the goal.
    match std::fs::rename(dir, &trash).and_then(|()| std::fs::remove_dir_all(&trash)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// What [`sweep_orphan_turn_targets`] did.
#[derive(Debug, Default)]
pub struct SweepReport {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Remove every turn dir under `<project>/target/<subdir>/` whose worktree
/// `<project>/.worktrees/<name>` is gone, plus leftover `.trash-*` dirs from an
/// interrupted delete. A missing `<subdir>` is an empty report, not an error.
pub fn sweep_orphan_turn_targets(project: &Path, subdir: &str) -> SweepReport {
    let mut report = SweepReport::default();
    let root = project.join(PROJECT_TARGET_DIR).join(subdir);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return report;
    };
    let worktrees = project.join(WORKTREES_DIR);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let path = entry.path();
        if name.to_string_lossy().starts_with(TRASH_PREFIX) {
            // Another runtime may be deleting it right now; losing that race
            // is fine, so errors are ignored.
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        if worktrees.join(&name).exists() {
            continue;
        }
        match remove_turn_target(&path) {
            Ok(()) => report.removed.push(path),
            Err(e) => report.failed.push((path, e.to_string())),
        }
    }
    report
}
