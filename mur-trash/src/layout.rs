//! On-disk trash layout (D3): `<agent_home>/trash/<entry-uuid>/<filename>`.
//!
//! One directory per entry, named by `TrashEntry.id`, so deleting the same name
//! twice — even in the same second — never collides.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Directory under the agent home that holds every trash entry.
pub const TRASH_DIR_NAME: &str = "trash";
/// Suffix of an in-flight cross-device copy (D4). Never a finished entry.
pub const PARTIAL_SUFFIX: &str = ".partial";
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

/// Remove `.partial` leftovers older than `min_age` from crashed moves.
///
/// The age floor keeps a sweep from deleting a copy that is still in flight.
/// Never follows symlinks; an entry directory left empty is removed too.
/// Returns how many partials were removed. A missing trash root is not an error.
pub fn sweep_partials(trash_root: &Path, min_age: Duration) -> io::Result<usize> {
    let entries = match std::fs::read_dir(trash_root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        // A symlinked "entry dir" is not ours to walk into.
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        for child in std::fs::read_dir(&dir)? {
            let child = child?;
            let is_partial = child
                .file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(PARTIAL_SUFFIX));
            if !is_partial {
                continue;
            }
            let meta = child.metadata()?; // DirEntry::metadata does not follow links
            let age = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .unwrap_or(Duration::ZERO);
            if age < min_age {
                continue;
            }
            if meta.is_dir() {
                std::fs::remove_dir_all(child.path())?;
            } else {
                std::fs::remove_file(child.path())?;
            }
            removed += 1;
        }
        // Only succeeds when empty; a finished entry beside the partial stays.
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(removed)
}
