//! Persistent index state: metadata (`*.meta.json`), live progress
//! (`*.progress.json`) and the single-writer lock (`*.lock`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{CodebaseIndex, FileMeta};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexMetadata {
    #[serde(default)]
    pub project_path: String,
    /// Main repo root (== project_path for the primary worktree). Recorded so the
    /// orphan sweep can tell "linked worktree removed" (repo present, worktree gone
    /// → prune) from "drive unmounted / repo deleted" (repo gone → keep). Empty on
    /// legacy metadata, which is therefore never auto-pruned.
    #[serde(default)]
    pub repo_root: String,
    /// Vector width the index was built at. `None` on legacy metadata (built
    /// before dims were recorded) — stamped on the next save without forcing
    /// a rebuild. ponytail: legacy None can't detect an old-dims table; a
    /// lance schema probe would close that, add it if users hit it.
    #[serde(default)]
    pub dimensions: Option<usize>,
    /// Commit HEAD pointed at when this index was built, read *before* the
    /// scan and stamped only on success — so a commit landing mid-scan leaves
    /// the index stale and it is rebuilt on the next pass. `None` on legacy
    /// metadata and for projects that are not git repositories.
    #[serde(default)]
    pub indexed_head: Option<String>,
    pub files: HashMap<String, FileMeta>,
    pub last_indexed: String,
}

/// Written during background indexing so `mur project status` can show live progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexProgress {
    pub status: IndexStatus,
    pub total_chunks: usize,
    pub done_chunks: usize,
    pub errors: usize,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IndexStatus {
    Running,
    Done,
    Error,
}

/// Lightweight lock file to prevent concurrent indexing of the same project.
/// An I/O failure on one of the index's own state files, with the path it hit.
/// `op` keeps the historical wording ("writing index lock <path>").
#[derive(Debug, thiserror::Error)]
#[error("{op} {}", path.display())]
pub struct IndexStateIoError {
    pub op: &'static str,
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

impl IndexStateIoError {
    pub(crate) fn new(op: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self {
            op,
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexLock {
    pub pid: u32,
    pub project_name: String,
    pub started_at: String,
}

impl CodebaseIndex {
    pub(super) fn meta_path(&self) -> PathBuf {
        self.lance_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}.meta.json", self.project_name))
    }

    pub(super) fn load_meta(&self) -> Option<IndexMetadata> {
        let path = self.meta_path();
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Vector width recorded at build time (`None` for legacy indexes). Used
    /// by `mur project status` to flag stale-dims indexes (#757).
    pub fn recorded_dimensions(&self) -> Option<usize> {
        self.load_meta()?.dimensions
    }

    /// Atomically replace the metadata: write a sibling `.tmp-<pid>` file in
    /// the same directory, then `rename` it over the target. A reader (or a
    /// crash mid-write) therefore only ever sees the old or the new document,
    /// never a torn one.
    pub(super) fn save_meta(&self, meta: &IndexMetadata) -> Result<()> {
        let path = self.meta_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(meta)?;
        let mut tmp = path.clone().into_os_string();
        tmp.push(format!(".tmp-{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, data)
            .with_context(|| format!("writing index meta temp {}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(
                anyhow::Error::new(e).context(format!("replacing index meta {}", path.display()))
            );
        }
        Ok(())
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.lance_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}.lock", self.project_name))
    }

    pub(super) fn progress_path(&self) -> PathBuf {
        self.lance_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}.progress.json", self.project_name))
    }

    /// Try to acquire the index lock. Returns Ok(true) if we got it, Ok(false) if another
    /// live process holds it, Err if I/O fails.
    ///
    /// Write failures carry an [`IndexStateIoError`] naming the path, so a
    /// caller can tell a denied index dir apart from any other failure without
    /// matching message text.
    pub fn try_acquire_lock(&self) -> Result<bool> {
        let path = self.lock_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| IndexStateIoError::new("creating index dir", parent, e))?;
        }
        // Check existing lock
        if path.exists()
            && let Ok(data) = std::fs::read_to_string(&path)
            && let Ok(lock) = serde_json::from_str::<IndexLock>(&data)
            && mur_common::lock_file::pid_alive(lock.pid)
        {
            return Ok(false); // Another live process holds the lock
            // Stale lock — pid is dead, we'll overwrite
        }
        let lock = IndexLock {
            pid: std::process::id(),
            project_name: self.project_name.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        std::fs::write(&path, serde_json::to_string(&lock)?)
            .map_err(|e| IndexStateIoError::new("writing index lock", &path, e))?;
        Ok(true)
    }

    pub fn release_lock(&self) {
        let path = self.lock_path();
        let _ = std::fs::remove_file(&path);
    }

    /// Write progress for `mur project status` to read.
    pub fn write_progress(&self, progress: &IndexProgress) -> Result<()> {
        let path = self.progress_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string(progress)?)?;
        Ok(())
    }

    /// Read progress file if it exists.
    pub fn read_progress(&self) -> Option<IndexProgress> {
        let path = self.progress_path();
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
