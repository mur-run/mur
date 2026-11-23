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

/// Not implemented off Unix: reports the path as unchanged. The Windows
/// comparison (volume serial and file index) belongs to the runtime task.
#[cfg(not(unix))]
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
