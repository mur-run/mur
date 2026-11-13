//! The post-commit hook's answer to a denied index write: queue a wake-up for
//! the daemon instead of failing (sandboxed codebase reindex, Task 5).

use std::path::Path;

use mur_common::reindex_request::{WakeupOutcome, drop_wakeup};

use crate::codebase::IndexStateIoError;

/// Subdirectory of the MUR home that holds the codebase indexes. A denial is
/// only turned into a wake-up when it hit a path under here.
const CODEBASE_INDEX_SUBDIR: &str = "indexes/codebase";

/// Handle a permission-denied error from the quiet (hook) path and return the
/// one line to print. Never fails: every branch ends in a line.
///
/// A wake-up is written only when the error chain carries an
/// [`IndexStateIoError`] whose path is under the codebase index dir. Anything
/// else — notably a config load or migration failure — keeps the plain skip
/// line, so an unrelated denial is never mistaken for "the index is
/// read-only here".
pub(super) fn quiet_denied_line(e: &anyhow::Error, project: &str, mur_home: &Path) -> String {
    let skip = || format!("mur: skipped auto-index for '{project}' — {e:#}");
    if !denied_under_index_dir(e, &mur_home.join(CODEBASE_INDEX_SUBDIR)) {
        return skip();
    }
    match drop_wakeup(mur_home) {
        Ok(WakeupOutcome::Queued(_)) => {
            format!("mur: queued auto-index for '{project}' with the daemon")
        }
        Ok(WakeupOutcome::AtCap) | Err(_) => skip(),
    }
}

fn denied_under_index_dir(e: &anyhow::Error, index_dir: &Path) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<IndexStateIoError>().is_some_and(|s| {
            s.source.kind() == std::io::ErrorKind::PermissionDenied && s.path.starts_with(index_dir)
        })
    })
}

#[cfg(test)]
#[path = "wakeup_tests.rs"]
mod tests;
