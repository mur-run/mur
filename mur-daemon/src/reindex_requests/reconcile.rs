//! Decide which indexes are stale. The inbox never names a project: the
//! allowlist is the set of `*.meta.json` files under `indexes/codebase/`,
//! which no agent can write, and staleness is each index's own scan-root HEAD
//! against the HEAD it was last built at.

use std::path::{Path, PathBuf};

use mur_core::codebase::IndexMetadata;

const META_SUFFIX: &str = ".meta.json";

/// One index that needs a rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Job {
    /// Index name: the `<name>` of `<name>.meta.json`.
    pub name: String,
    /// The index's own scan root (`IndexMetadata.project_path`).
    pub root: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Reconciled {
    pub stale: Vec<Job>,
    /// A meta could not be parsed (possibly mid-write by a terminal build).
    /// It is retried on the next sweep instead of being forgotten.
    pub deferred: bool,
}

/// Scan `indexes_dir` for metas and return the stale ones, sorted by name.
pub(super) fn reconcile(indexes_dir: &Path) -> Reconciled {
    let mut out = Reconciled::default();
    let Ok(rd) = std::fs::read_dir(indexes_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str().and_then(|n| n.strip_suffix(META_SUFFIX)) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        // Regular file only — a symlinked meta could name any project.
        match entry.file_type() {
            Ok(ft) if ft.is_file() => {}
            Ok(ft) if ft.is_symlink() => {
                tracing::warn!(index = name, "symlinked index meta rejected");
                continue;
            }
            _ => continue,
        }
        let parsed = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|d| serde_json::from_str::<IndexMetadata>(&d).ok());
        let Some(meta) = parsed else {
            tracing::debug!(index = name, "index meta unreadable; deferring");
            out.deferred = true;
            continue;
        };
        if let Some(job) = job_if_stale(name, &meta) {
            out.stale.push(job);
        }
    }
    out.stale.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Stale iff the scan root is a git repo whose HEAD differs from the one the
/// index was built at. A legacy meta (`indexed_head: None`) on a repo is
/// stale once; a non-repo root is never stale here (the trigger is commits).
fn job_if_stale(name: &str, meta: &IndexMetadata) -> Option<Job> {
    let root = PathBuf::from(&meta.project_path);
    if !root.is_absolute() {
        return None;
    }
    let current = mur_core::codebase::head::resolve(&root)?;
    (meta.indexed_head.as_deref() != Some(current.as_str())).then(|| Job {
        name: name.to_string(),
        root,
    })
}
