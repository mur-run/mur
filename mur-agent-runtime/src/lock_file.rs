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
    /// Held open so the exclusive flock stays active for this process's
    /// lifetime.  This is the *sentinel* file, never renamed by `write_lock`,
    /// so the flock inode is always stable — see `is_stale` for why.
    _sentinel: File,
    /// Path of the JSON data file (`running.lock`), removed on `release`.
    path: PathBuf,
}

/// Returns the sentinel file path for a given lock path.
///
/// The sentinel (`running.sentinel`) is a separate, always-stable file used
/// exclusively for flock-based ownership tracking.  `write_lock` performs a
/// temp-file + rename on the JSON data file, which swaps the inode.  If we
/// flocked the JSON file directly the inode swap would leave `is_stale`
/// probing an inode that nobody holds a lock on, incorrectly classifying a
/// live agent as stale.  By keeping the flock on a file that is never
/// renamed we avoid the race entirely.
fn sentinel_path(path: &Path) -> PathBuf {
    path.with_extension("sentinel")
}

impl LockHandle {
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let sentinel = sentinel_path(path);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&sentinel)?;
        file.try_lock_exclusive()
            .map_err(|_| LockError::AlreadyHeld)?;
        // Record the holder pid in the sentinel, best-effort: failure to
        // write must not tear down the lock we just acquired.
        if file.set_len(0).is_ok() {
            let _ = file.write_all(std::process::id().to_string().as_bytes());
        }
        Ok(Self {
            _sentinel: file,
            path: path.to_path_buf(),
        })
    }

    pub fn release(self) {
        // Only remove the lock if we still own it: a stale duplicate instance
        // shutting down must not clobber the lock a newer instance has since
        // written (its release would leave the live agent looking stopped).
        if let Ok(lock) = read_lock(&self.path)
            && lock.pid != std::process::id()
        {
            return;
        }
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(sentinel_path(&self.path));
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
    // Step (b): flock probe on the sentinel file (never renamed by write_lock,
    // so its inode is always the one held by a live LockHandle).  If this
    // process owns the lock, a fresh-fd flock returns a misleading result on
    // macOS/BSD (independent locks per open) — trust the pid in that case.
    if lock.pid == std::process::id() {
        return Ok(false);
    }
    let sentinel = sentinel_path(path);
    if !sentinel.exists() {
        // Sentinel absent means the lock was never cleanly acquired or was
        // already released; treat as stale.
        return Ok(true);
    }
    let file = OpenOptions::new().read(true).write(true).open(&sentinel)?;
    let can_acquire = file.try_lock_exclusive().is_ok();
    if can_acquire {
        let _ = FileExt::unlock(&file);
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::LockTransports;

    fn lock_json(pid: u32) -> LockFile {
        LockFile {
            schema: 1,
            uuid: "u".into(),
            name: "t".into(),
            pid,
            ppid: 0,
            started_at: String::new(),
            binary_version: String::new(),
            transports: LockTransports {
                stdio: true,
                unix_socket: None,
                tcp: None,
                webhook: None,
            },
            card_digest: String::new(),
            capabilities: vec![],
            build_sha: String::new(),
            proto_version: 0,
        }
    }

    #[test]
    fn release_skips_when_lock_owned_by_other_pid() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("running.lock");
        let handle = LockHandle::acquire(&path).unwrap();
        // A newer instance has since written its own lock.
        write_lock(&path, &lock_json(std::process::id() + 1)).unwrap();
        handle.release();
        assert!(path.exists(), "release must not clobber another pid's lock");
        assert!(sentinel_path(&path).exists());
    }

    #[test]
    fn release_removes_own_lock() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("running.lock");
        let handle = LockHandle::acquire(&path).unwrap();
        write_lock(&path, &lock_json(std::process::id())).unwrap();
        handle.release();
        assert!(!path.exists());
        assert!(!sentinel_path(&path).exists());
    }
}
