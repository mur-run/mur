//! The reply-directory lifecycle lock, and the one "does this agent exist"
//! predicate (fleet shared area spec v3.7-v3.11).
//!
//! One exclusive `flock` on `<mur_home>/fleet-shared/.reply-dirs.lock` orders
//! every change to whether an agent of a given name exists against every
//! creation and removal of that agent's reply directory. Same shape as
//! `entitlements_pin::lock`: the file is never removed, dropping the guard (or
//! the process dying) releases the lock, and it is not reentrant, so a path
//! that already holds it hands the [`LifecycleGuard`] down instead of taking
//! it again.
//!
//! The file sits in the store root, which no sealed member may read or write,
//! so a member can neither hold the lock nor swap the file.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;

/// Store root under `<mur_home>`; denied to members for read and write.
pub const FLEET_SHARED_DIR: &str = "fleet-shared";
/// Lock file inside [`FLEET_SHARED_DIR`].
pub const LIFECYCLE_LOCK_FILE: &str = ".reply-dirs.lock";
/// Pause between non-blocking `flock` attempts.
pub const LOCK_POLL: Duration = Duration::from_millis(25);
/// A wait longer than this is logged.
const SLOW_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum LifecycleLockError {
    /// The lock stayed held (or kept being replaced) until the deadline.
    #[error("timed out after {waited:?} waiting for the lifecycle lock {path}")]
    Timeout { path: PathBuf, waited: Duration },
    #[error("io error on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

/// Proof that the lifecycle lock is held. Released on drop.
#[derive(Debug)]
pub struct LifecycleGuard {
    file: File,
    path: PathBuf,
}

impl LifecycleGuard {
    /// True while the lock path still names the file this guard holds. A lock
    /// file replaced by hand after acquisition makes this false.
    pub fn still_at_path(&self) -> io::Result<bool> {
        same_file(&self.file, &self.path)
    }
}

/// Take the lifecycle lock, waiting at most `timeout` in total. Creates the
/// store root if it is absent and syncs `<mur_home>` so the root survives a
/// crash.
pub fn lock_reply_dirs(
    mur_home: &Path,
    timeout: Duration,
) -> Result<LifecycleGuard, LifecycleLockError> {
    lock_until(mur_home, Instant::now() + timeout, |_| {})
}

/// `lock_reply_dirs` against a fixed `deadline`. `after_lock` runs once per
/// attempt, after the lock is held and before the inode check: the seam the
/// tests use to replace the file at that exact point.
fn lock_until(
    mur_home: &Path,
    deadline: Instant,
    mut after_lock: impl FnMut(&Path),
) -> Result<LifecycleGuard, LifecycleLockError> {
    let started = Instant::now();
    let root = mur_home.join(FLEET_SHARED_DIR);
    let path = root.join(LIFECYCLE_LOCK_FILE);
    let io_err = |source: io::Error| LifecycleLockError::Io {
        path: path.clone(),
        source,
    };
    if !root.exists() {
        std::fs::create_dir_all(&root).map_err(io_err)?;
        sync_dir(mur_home).map_err(io_err)?;
    }
    let mut logged = false;
    loop {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(io_err)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                after_lock(&path);
                if same_file(&file, &path).map_err(io_err)? {
                    if started.elapsed() > SLOW_WAIT {
                        tracing::info!(path = %path.display(), waited = ?started.elapsed(), "waited for the lifecycle lock");
                    }
                    return Ok(LifecycleGuard { file, path });
                }
                // Replaced under us: drop it and try the new file against the
                // same deadline, so a file that keeps changing ends in a
                // timeout and not in an endless wait.
                drop(file);
            }
            Err(e) if e.kind() == fs2::lock_contended_error().kind() => {}
            Err(e) => return Err(io_err(e)),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(LifecycleLockError::Timeout {
                path,
                waited: started.elapsed(),
            });
        }
        if !logged && started.elapsed() > SLOW_WAIT {
            logged = true;
            tracing::info!(path = %path.display(), "waiting for the lifecycle lock");
        }
        std::thread::sleep(LOCK_POLL.min(deadline - now));
    }
}

#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let held = file.metadata()?;
    match std::fs::metadata(path) {
        Ok(at_path) => Ok(held.dev() == at_path.dev() && held.ino() == at_path.ino()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Windows: compare the volume serial number and the 128-bit file id of the
/// held handle with those of whatever is at `path` now (`FILE_ID_INFO`; the
/// 64-bit index of `BY_HANDLE_FILE_INFORMATION` is not unique on ReFS).
#[cfg(windows)]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let held = file_id(file)?;
    // Attribute-only access with full sharing: the probe must neither take
    // part in the lock nor stop another process replacing the file.
    let at_path = match OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    Ok(held == file_id(&at_path)?)
}

/// `(volume serial, file id)` of an open handle.
#[cfg(windows)]
fn file_id(file: &File) -> io::Result<(u64, [u8; 16])> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::zeroed();
    // SAFETY: the handle is owned by `file` and open for the whole call; the
    // buffer is a `FILE_ID_INFO` of exactly the size passed, which is the
    // layout the `FileIdInfo` class writes.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so it filled the struct.
    let info = unsafe { info.assume_init() };
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

/// Neither Unix nor Windows: no file identity to compare, so the path is
/// reported unchanged.
#[cfg(not(any(unix, windows)))]
fn same_file(_file: &File, _path: &Path) -> io::Result<bool> {
    Ok(true)
}

#[cfg(test)]
thread_local! {
    static SYNCED_DIRS: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The directories synced on this thread since the last call (test seam).
#[cfg(test)]
fn take_synced_dirs() -> Vec<PathBuf> {
    SYNCED_DIRS.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

/// Make a directory entry created inside `dir` durable.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(test)]
    SYNCED_DIRS.with(|d| d.borrow_mut().push(dir.to_path_buf()));
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests;
