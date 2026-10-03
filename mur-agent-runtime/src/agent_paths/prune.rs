//! Start-time retention pass over one agent's scratch dir (spec §Cleanup).
//!
//! Top-level entries only; an entry's age is the newest mtime anywhere in
//! its tree, walked with `symlink_metadata` so links are never followed.
//! An entry is removed whole or kept whole — never partially pruned.

use std::path::Path;
use std::time::{Duration, SystemTime};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    pub removed: usize,
    pub kept: usize,
    pub errors: usize,
}

/// Remove top-level entries of `dir` whose newest mtime is older than
/// `now - retention`. `dir` itself is always kept. Per-entry failures are
/// logged at `warn`, counted, and do not stop the pass.
pub fn prune_scratch(dir: &Path, retention: Duration, now: SystemTime) -> PruneReport {
    let mut report = PruneReport::default();
    let cutoff = now.checked_sub(retention).unwrap_or(SystemTime::UNIX_EPOCH);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "scratch prune: cannot read dir");
            report.errors += 1;
            return report;
        }
    };
    for entry in entries {
        let path = match entry {
            Ok(e) => e.path(),
            Err(e) => {
                tracing::warn!(error = %e, "scratch prune: bad dir entry");
                report.errors += 1;
                continue;
            }
        };
        match prune_entry(&path, cutoff) {
            Ok(true) => report.removed += 1,
            Ok(false) => report.kept += 1,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "scratch prune: entry skipped");
                report.errors += 1;
            }
        }
    }
    report
}

/// `Ok(true)` when removed, `Ok(false)` when kept as still fresh.
fn prune_entry(path: &Path, cutoff: SystemTime) -> std::io::Result<bool> {
    if newest_mtime(path)? >= cutoff {
        return Ok(false);
    }
    // `symlink_metadata`: a link to a directory is unlinked, never descended.
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(true)
}

fn newest_mtime(path: &Path) -> std::io::Result<SystemTime> {
    let meta = std::fs::symlink_metadata(path)?;
    let mut newest = meta.modified()?;
    if meta.is_dir() {
        for child in std::fs::read_dir(path)? {
            newest = newest.max(newest_mtime(&child?.path())?);
        }
    }
    Ok(newest)
}
