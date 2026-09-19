use anyhow::Result;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockState {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
}

pub fn lock_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("murmurd.lock")
}

/// The flock sentinel beside `murmurd.lock`.
///
/// A separate file on purpose: `murmurd.lock` is rewritten every 10 s by the
/// heartbeat, and a rewrite that ever swaps the inode would leave the flock
/// on an inode nobody can see. The sentinel is created once and never
/// rewritten, so its inode is stable for the daemon's whole life. (Same
/// reasoning as `mur-agent-runtime/src/lock_file.rs`, which learned it first.)
pub fn sentinel_path(lock: &Path) -> PathBuf {
    lock.with_extension("sentinel")
}

/// Exclusive ownership of the murmurd singleton, held for the process's
/// lifetime. Dropping it — or the process dying, for any reason including
/// SIGKILL, OOM or a yanked power cable — releases the flock, because the
/// kernel releases it when the fd closes. There is no stale state to clean
/// up, which is the whole reason this replaces a heartbeat timestamp.
///
/// The sentinel FILE is deliberately left on disk. Its existence means
/// nothing; only the lock held on it does. Deleting it on shutdown would
/// introduce exactly the inode race the sentinel exists to avoid.
#[derive(Debug)]
pub struct SingletonGuard {
    _sentinel: File,
}

/// Why a singleton acquisition failed.
#[derive(Debug)]
pub enum AcquireError {
    /// Another live murmurd holds the flock. Carries the pid it recorded in
    /// the sentinel, when that could be read.
    AlreadyRunning {
        pid: Option<u32>,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::AlreadyRunning { pid: Some(pid) } => {
                write!(f, "murmurd already running (pid {pid})")
            }
            AcquireError::AlreadyRunning { pid: None } => write!(f, "murmurd already running"),
            AcquireError::Io(e) => write!(f, "murmurd lock io error: {e}"),
        }
    }
}

/// Take the process-wide murmurd singleton, or report who already holds it.
///
/// This is the ONLY check that actually excludes a second daemon. The
/// heartbeat in `murmurd.lock` cannot: reading a stale timestamp and then
/// starting is a check-then-act race, and worse, a daemon that is alive but
/// briefly starved (a long store-health scan, a stopped process, a laptop
/// resuming from sleep) publishes a stale heartbeat while still running — so
/// every hook that saw it spawned another one, and none of them ever exited.
/// That is the five-murmurd screenshot in issue 014.
///
/// `flock` has no such window: the kernel grants it to exactly one process,
/// and releases it on death however the process died.
pub fn acquire_singleton(lock: &Path) -> Result<SingletonGuard, AcquireError> {
    let sentinel = sentinel_path(lock);
    if let Some(parent) = sentinel.parent() {
        std::fs::create_dir_all(parent).map_err(AcquireError::Io)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&sentinel)
        .map_err(AcquireError::Io)?;

    if file.try_lock_exclusive().is_err() {
        // Held by a live daemon. Read the pid it left for the error message;
        // a failure here only costs us a nicer message.
        let pid = std::fs::read_to_string(&sentinel)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        return Err(AcquireError::AlreadyRunning { pid });
    }

    // Record our pid inside the sentinel so the loser can name the winner.
    // Best-effort: a write failure must not tear down the lock we just won.
    if file.set_len(0).is_ok() {
        let _ = file.write_all(std::process::id().to_string().as_bytes());
        let _ = file.flush();
    }

    Ok(SingletonGuard { _sentinel: file })
}

pub fn write_lock(path: &Path, state: &LockState) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

/// Round-trip reader for the heartbeat JSON. Test-only inside this crate: the
/// daemon no longer *reads* its own lock for any decision (the flock decides),
/// and the readers that still care — `mur daemon status`, the hook's respawn
/// check — live in `mur-core/src/daemon.rs`.
#[cfg(test)]
pub fn read_lock(path: &Path) -> Result<Option<LockState>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_lock_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let state = LockState {
            pid: std::process::id(),
            started_at: Utc::now(),
            heartbeat_at: Utc::now(),
        };
        write_lock(&path, &state).unwrap();
        let loaded = read_lock(&path).unwrap().unwrap();
        assert_eq!(loaded.pid, state.pid);
    }

    #[test]
    fn read_lock_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.lock");
        assert!(read_lock(&path).unwrap().is_none());
    }

    #[test]
    fn second_acquire_is_refused_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let first = acquire_singleton(&lock).expect("first acquire should win");
        match acquire_singleton(&lock) {
            Err(AcquireError::AlreadyRunning { pid }) => {
                assert_eq!(
                    pid,
                    Some(std::process::id()),
                    "loser should name the holder's pid"
                );
            }
            Err(e) => panic!("expected AlreadyRunning, got {e}"),
            Ok(_) => panic!("two murmurd singletons were granted at once"),
        }
        drop(first);
    }

    #[test]
    fn acquire_succeeds_again_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let first = acquire_singleton(&lock).unwrap();
        drop(first);
        let _second = acquire_singleton(&lock).expect("released lock should be re-acquirable");
    }

    /// The regression 014 is actually about: the heartbeat in `murmurd.lock`
    /// says the daemon is dead (stale by a minute), but the process is in fact
    /// alive and still holding the flock. The old `is_healthy` gate started a
    /// second daemon here; the flock must not.
    #[test]
    fn stale_heartbeat_does_not_let_a_second_daemon_in() {
        use chrono::TimeDelta;
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let held = acquire_singleton(&lock).unwrap();

        let stale = LockState {
            pid: std::process::id(),
            started_at: Utc::now() - TimeDelta::seconds(600),
            heartbeat_at: Utc::now() - TimeDelta::seconds(60),
        };
        write_lock(&lock, &stale).unwrap();
        let age = Utc::now()
            .signed_duration_since(stale.heartbeat_at)
            .num_seconds();
        assert!(age >= 30, "precondition: heartbeat reads as dead");

        assert!(
            matches!(
                acquire_singleton(&lock),
                Err(AcquireError::AlreadyRunning { .. })
            ),
            "a live holder must win even when its heartbeat looks stale"
        );
        drop(held);
    }

    /// The heartbeat rewrites `murmurd.lock` every 10 s. If the flock lived on
    /// that file, a rewrite would move the inode out from under it and a
    /// second daemon would sail in. The sentinel is a different file.
    #[test]
    fn heartbeat_rewrites_do_not_break_the_flock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("murmurd.lock");
        let held = acquire_singleton(&lock).unwrap();

        for _ in 0..3 {
            let beat = LockState {
                pid: std::process::id(),
                started_at: Utc::now(),
                heartbeat_at: Utc::now(),
            };
            write_lock(&lock, &beat).unwrap();
        }

        assert!(
            matches!(
                acquire_singleton(&lock),
                Err(AcquireError::AlreadyRunning { .. })
            ),
            "flock must survive heartbeat rewrites of the JSON lock"
        );
        drop(held);
    }

    #[test]
    fn sentinel_is_a_sibling_of_the_lock() {
        let p = sentinel_path(Path::new("/tmp/x/murmurd.lock"));
        assert_eq!(p, Path::new("/tmp/x/murmurd.sentinel"));
    }
}
