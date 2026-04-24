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
pub fn is_stale(path: &Path) -> Result<bool, LockError> {
    if !path.exists() {
        return Ok(true);
    }
    let lock: LockFile = match read_lock(path) {
        Ok(l) => l,
        Err(_) => return Ok(true), // corrupt = stale
    };
    if !pid_alive(lock.pid) {
        return Ok(true);
    }
    // If this process owns the lock, flock on a fresh fd returns a misleading
    // result on macOS/BSD (independent locks per open). Trust the pid.
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

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        CloseHandle(h);
        true
    }
}
