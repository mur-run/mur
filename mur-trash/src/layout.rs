//! On-disk trash layout (D3): `<agent_home>/trash/<entry-uuid>/<filename>`.
//!
//! One directory per entry, named by `TrashEntry.id`, so deleting the same name
//! twice — even in the same second — never collides.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Directory under the agent home that holds every trash entry.
pub const TRASH_DIR_NAME: &str = "trash";
/// Suffix of an in-flight cross-device copy made outside the trash (restore).
pub const PARTIAL_SUFFIX: &str = ".partial";
/// Directory under the trash root where cross-device copies INTO the trash are
/// staged. Not a UUID, so it never collides with an entry directory, and the
/// only place [`sweep_partials`] looks: a retained entry is never judged by
/// its name (review #2164 #1 — `rm report.partial` must stay restorable).
pub const STAGING_DIR_NAME: &str = ".staging";
pub const ACTIONS_DIR_NAME: &str = "actions";
pub const LEDGER_DIR_NAME: &str = "ledger";

/// `<agent_home>/actions/ledger`. Same as `Pipeline::ledger_dir()` in `mur-core`.
pub fn ledger_dir(agent_home: &Path) -> PathBuf {
    agent_home.join(ACTIONS_DIR_NAME).join(LEDGER_DIR_NAME)
}

/// `<agent_home>/trash`. Same root as `Pipeline::trash_dir()` in `mur-core`.
pub fn trash_root(agent_home: &Path) -> PathBuf {
    agent_home.join(TRASH_DIR_NAME)
}

/// `<trash_root>/.staging` — see [`STAGING_DIR_NAME`].
pub fn staging_root(trash_root: &Path) -> PathBuf {
    trash_root.join(STAGING_DIR_NAME)
}

/// `<trash_root>/<id>` — the directory owned by one entry.
pub fn entry_dir(trash_root: &Path, id: impl std::fmt::Display) -> PathBuf {
    trash_root.join(id.to_string())
}

/// `<trash_root>/<id>/<filename>` — where a target lands.
///
/// `None` when `original` has no usable final component (`/`, `..`); the
/// caller refuses such targets before reaching here (D5).
pub fn entry_path(
    trash_root: &Path,
    id: impl std::fmt::Display,
    original: &Path,
) -> Option<PathBuf> {
    let name = original.file_name()?;
    Some(entry_dir(trash_root, id).join(name))
}

/// `<path>.partial`, the staging name for a cross-device copy.
pub fn partial_path(dst: &Path) -> PathBuf {
    let mut s = dst.as_os_str().to_owned();
    s.push(PARTIAL_SUFFIX);
    PathBuf::from(s)
}

/// Remove staging directories older than `min_age` left by crashed moves.
///
/// Only `<trash_root>/.staging/<move>/` is examined; entry directories are
/// never walked, whatever their contents are named. Age is the staging dir's
/// own mtime (created fresh per move), not the copied object's, which keeps
/// the source's mtime. Never follows symlinks. Returns how many were removed.
/// A missing trash root or staging dir is not an error.
pub fn sweep_partials(trash_root: &Path, min_age: Duration) -> io::Result<usize> {
    let entries = match std::fs::read_dir(staging_root(trash_root)) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let meta = entry.metadata()?; // DirEntry::metadata does not follow links
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or(Duration::ZERO);
        if age < min_age {
            continue;
        }
        if meta.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
        removed += 1;
    }
    Ok(removed)
}
