//! §7.0 run lock: a review driver holds an exclusive OS advisory lock on
//! `driver.lock` in its channel directory for as long as it runs.
//!
//! Liveness is decided ONLY by whether the lock can be taken. The kernel
//! drops the lock when the holder exits for any reason, SIGKILL included,
//! so a dead driver never looks alive, and a recycled pid cannot fake
//! liveness because the pid written in the file is never consulted. The body
//! (`pid`, start time, host) exists for messages like "running in pid 4123"
//! and lives in a separate `driver.owner` file: Windows locks are mandatory,
//! so a held `driver.lock` cannot be read by anyone else.
//!
//! `fs2`, not `libc::flock`, for the reasons in `cmd/agent/cli/login.rs`
//! (`acquire_login_lock`): Windows CI, existing dependency, same contention
//! test via `fs2::lock_contended_error`.

use std::fs::File;
use std::path::{Path, PathBuf};

use chrono::Utc;
use fs2::FileExt;
use mur_channel::ChannelService;

use super::constants::{DRIVER_LOCK_FILE, DRIVER_OWNER_FILE};

/// A held run lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct DriverLock(#[allow(dead_code)] File);

/// Why the run lock could not be taken.
#[derive(Debug)]
pub enum LockDenied {
    /// Another driver holds it: the session is running. Carries the holder's
    /// self-description for display, when readable.
    Running(Option<String>),
    /// The lock file could not be opened or locked at all.
    Unavailable(std::io::Error),
}

impl std::fmt::Display for LockDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockDenied::Running(Some(who)) => write!(f, "it is running ({who})"),
            LockDenied::Running(None) => write!(f, "it is running"),
            LockDenied::Unavailable(e) => write!(f, "its run lock cannot be taken: {e}"),
        }
    }
}

/// `driver.lock` for a session channel.
pub fn lock_path(svc: &ChannelService, channel_id: &str) -> PathBuf {
    svc.store()
        .events_path(channel_id)
        .with_file_name(DRIVER_LOCK_FILE)
}

/// Try to take the run lock without blocking. On success the owner file is
/// rewritten with this process's pid, start time and host (display only).
pub fn try_acquire(svc: &ChannelService, channel_id: &str) -> Result<DriverLock, LockDenied> {
    acquire_at(&lock_path(svc, channel_id))
}

fn acquire_at(path: &Path) -> Result<DriverLock, LockDenied> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(LockDenied::Unavailable)?;
    match f.try_lock_exclusive() {
        Ok(()) => {
            // Best effort: a body we cannot write only costs a message.
            let host = hostname::get()
                .map(|h| h.to_string_lossy().into_owned())
                .unwrap_or_default();
            let body = serde_json::json!({
                "pid": std::process::id(),
                "started_at": Utc::now(),
                "host": host,
            });
            let _ = std::fs::write(owner_path(path), body.to_string());
            Ok(DriverLock(f))
        }
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Err(LockDenied::Running(describe(path)))
        }
        Err(e) => Err(LockDenied::Unavailable(e)),
    }
}

/// The owner file that sits next to a lock file.
fn owner_path(lock: &Path) -> PathBuf {
    lock.with_file_name(DRIVER_OWNER_FILE)
}

/// "pid 4123 on host since …", from the holder's owner file. Display only.
fn describe(lock: &Path) -> Option<String> {
    let text = std::fs::read_to_string(owner_path(lock)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(format!(
        "pid {} on {} since {}",
        v.get("pid")?,
        v.get("host")?.as_str().unwrap_or("?"),
        v.get("started_at")?.as_str().unwrap_or("?"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC15c: liveness ignores the stored pid. A body naming THIS live
    /// process does not make an unlocked file look held, and a body naming a
    /// pid that cannot exist does not make a held file look free.
    #[test]
    fn liveness_is_the_lock_never_the_stored_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(DRIVER_LOCK_FILE);

        std::fs::write(
            owner_path(&path),
            format!(r#"{{"pid":{}}}"#, std::process::id()),
        )
        .unwrap();
        let held = acquire_at(&path).expect("a live pid in the body must not block");

        // A second, independent open is a separate lock owner (flock is per
        // open file description; LockFileEx is per handle), standing in for
        // another process.
        match acquire_at(&path) {
            Err(LockDenied::Running(_)) => {}
            other => panic!("expected Running, got {other:?}"),
        }
        drop(held);

        std::fs::write(owner_path(&path), r#"{"pid":4294967295}"#).unwrap();
        let other = File::open(&path).unwrap();
        other.try_lock_exclusive().unwrap();
        assert!(matches!(acquire_at(&path), Err(LockDenied::Running(_))));
        FileExt::unlock(&other).unwrap();
        assert!(acquire_at(&path).is_ok(), "released lock is free again");
    }

    /// Env var naming the lock file the child-holder test should lock.
    const HOLD_ENV: &str = "MUR_TEST_REVIEW_DRIVER_LOCK_HOLD";

    /// Child half of the cross-process test: does nothing in a normal run.
    #[test]
    fn child_holds_the_lock_until_killed() {
        let Some(path) = std::env::var_os(HOLD_ENV) else {
            return;
        };
        // The parent polls with `acquire_at` too, so it briefly holds the
        // lock on every probe; retry instead of dying on that collision.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let _held = loop {
            match acquire_at(Path::new(&path)) {
                Ok(l) => break l,
                Err(LockDenied::Running(_)) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("child takes the lock: {e}"),
            }
        };
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// AC15c: a lock held by ANOTHER process (different pid) reads as
    /// running, and SIGKILL of that process frees it — no `paused`, no
    /// cleanup, the kernel alone releases it.
    #[test]
    fn a_killed_holder_process_releases_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(DRIVER_LOCK_FILE);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cmd::fleet::review::run_lock::tests::child_holds_the_lock_until_killed",
                "--nocapture",
            ])
            .env(HOLD_ENV, &path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        // The child writes its owner body only AFTER it takes the lock, and
        // every successful probe here rewrites that body with OUR pid, so a
        // `Running` can briefly carry a stale or empty body. Wait for the
        // child's own body rather than trusting the first `Running`.
        let child_pid = format!("pid {}", child.id());
        let mut last = None;
        loop {
            match acquire_at(&path) {
                Err(LockDenied::Running(Some(who))) if who.contains(&child_pid) => break,
                Err(LockDenied::Running(who)) => last = who,
                Ok(l) => drop(l),
                Err(e) => panic!("{e}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child never locked with its own body (last: {last:?})"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_ne!(child.id(), std::process::id());

        child.kill().unwrap(); // SIGKILL on Unix, TerminateProcess on Windows
        child.wait().unwrap();
        assert!(
            acquire_at(&path).is_ok(),
            "kernel released the dead holder's lock"
        );
    }
}
