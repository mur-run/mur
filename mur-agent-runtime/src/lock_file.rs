//! running.lock file — flock-based stale detection + JSON persistence.

use fs2::FileExt;
use mur_common::LockFile;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("lock already held")]
    AlreadyHeld,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug)]
pub struct LockHandle {
    _file: File,
    path: PathBuf,
}

impl LockHandle {
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.try_lock_exclusive()
            .map_err(|_| LockError::AlreadyHeld)?;
        Ok(Self {
            _file: file,
            path: path.to_path_buf(),
        })
    }

    pub fn release(self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn write_lock(path: &Path, lock: &LockFile) -> Result<(), LockError> {
    let bytes = serde_json::to_vec_pretty(lock)?;
    let tmp = path.with_extension("lock.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_lock(path: &Path) -> Result<LockFile, LockError> {
    let mut buf = String::new();
    File::open(path)?.read_to_string(&mut buf)?;
    Ok(serde_json::from_str(&buf)?)
}

/// A lock is stale if either
/// (a) its pid is not alive, or
/// (b) flock can be acquired (nobody's holding it).
///
/// Steps (a) uses `mur_common::lock_file::read` and `pid_alive` — the
/// canonical single source of truth (closes #36). Step (b) (flock probe) is
/// a stronger, runtime-specific check and stays local.
pub fn is_stale(path: &Path) -> Result<bool, LockError> {
    // Step (a): read + pid liveness via the canonical common helpers.
    let lock = match mur_common::lock_file::read(path).map_err(LockError::Io)? {
        None => return Ok(true), // no lock file → stale
        Some(l) => l,
    };
    if !mur_common::lock_file::pid_alive(lock.pid) {
        return Ok(true);
    }
    // Step (b): flock probe — if this process owns the lock, flock on a
    // fresh fd returns a misleading result on macOS/BSD (independent locks
    // per open). Trust the pid in that case.
    if lock.pid == std::process::id() {
        return Ok(false);
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let can_acquire = file.try_lock_exclusive().is_ok();
    if can_acquire {
        let _ = FileExt::unlock(&file);
        return Ok(true);
    }
    Ok(false)
}
