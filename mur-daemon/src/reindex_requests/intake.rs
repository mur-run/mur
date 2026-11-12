//! Drain `<mur_home>/inbox/reindex-requests/` without trusting its contents.
//!
//! The inbox is agent-writable. Entries are classified from `read_dir`'s
//! file type and an `lstat` only — nothing is ever opened, so a FIFO cannot
//! block the sweep and a symlink cannot point the daemon anywhere. Every
//! entry looked at is unlinked, valid or not, so the directory cannot fill up.

use std::path::Path;
use std::time::{Duration, SystemTime};

use mur_common::reindex_request::TMP_PREFIX;

/// An in-flight `.tmp-*` file older than this was left by a writer that died
/// between write and rename; it is removed. Younger ones belong to a writer
/// that is mid-rename and are left alone.
pub(super) const TMP_STALE_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Swept {
    /// Entries looked at this sweep (never more than the intake bound).
    pub examined: usize,
    /// Regular, empty, non-temp files: the only shape a wake-up has.
    pub wakeups: usize,
}

/// Look at no more than `max_intake` entries of `dir`.
pub(super) fn sweep(dir: &Path, max_intake: usize, now: SystemTime) -> Swept {
    let mut out = Swept::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.take(max_intake) {
        out.examined += 1;
        let Ok(entry) = entry else { continue };
        // `DirEntry::file_type` / `metadata` do not follow symlinks.
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        let is_tmp = entry.file_name().to_string_lossy().starts_with(TMP_PREFIX);
        if is_tmp {
            let fresh = ft.is_file()
                && entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| now.duration_since(t).unwrap_or_default() < TMP_STALE_AFTER);
            if fresh {
                continue;
            }
        } else if ft.is_file() && entry.metadata().is_ok_and(|m| m.len() == 0) {
            out.wakeups += 1;
        }
        let removed = if ft.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = removed {
            tracing::warn!(entry = %path.display(), error = %e, "could not remove reindex inbox entry");
        }
    }
    out
}
